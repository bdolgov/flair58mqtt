#![no_std]
#![no_main]

use core::fmt::Arguments;
use embassy_executor::Spawner;
use embassy_rp::{bind_interrupts, peripherals, usb};
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use embassy_sync::channel::Channel;
use heapless::String;
use panic_probe as _;

mod config;
mod init_network;
mod mqtt;
mod state;

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ =>  embassy_rp::usb::InterruptHandler<peripherals::USB>;
    PIO0_IRQ_0 => embassy_rp::pio::InterruptHandler<peripherals::PIO0>;
});

#[embassy_executor::task]
async fn logger_task(driver: usb::Driver<'static, peripherals::USB>) {
    embassy_usb_logger::run!(8192, log::LevelFilter::Info, driver);
}

static LOG_CHANNEL: Channel<ThreadModeRawMutex, String<256>, 16> = Channel::new();

fn mqtt_log(args: Arguments<'_>) {
    let mut s = String::<256>::new();
    match core::fmt::write(&mut s, args) {
        Ok(()) => {
            log::info!("mqtt log: {}", s);
            if let Err(err) = LOG_CHANNEL.try_send(s) {
                log::warn!("^ the message above was not sent to mqtt log: {:?}", err);
            }
        }
        Err(err) => {
            log::warn!("Failed to format error message: {:?}", err);
        }
    }
}

// Logs the given formatted string to the MQTT log topic.
#[macro_export]
macro_rules! mqtt_log {
    ($($arg:tt)*) => {
        $crate::mqtt_log(core::format_args!($($arg)*))
    };
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    // Init USB first, so that early debug logging is available, including logs from interacting
    // with network.
    let usb_driver = usb::Driver::new(p.USB, Irqs);
    spawner.must_spawn(logger_task(usb_driver));

    // Start tasks responsible for interacting with Flair58.
    spawner.must_spawn(state::led_detector_task(p.PIN_12, p.PIN_13, p.PIN_14));
    spawner.must_spawn(state::state_actuator_task(p.PIN_15));

    // Connect to the network.
    let network_stack = init_network::init_network(
        spawner,
        &config::CONFIG.wifi_config,
        p.PIN_23,
        p.PIN_24,
        p.PIN_25,
        p.PIN_29,
        p.PIO0,
        p.DMA_CH0,
    )
    .await;
    mqtt_log!(
        "The device has started. Address: {:?}",
        network_stack.config_v4()
    );

    // Handle MQTT incoming and outgoing messages..
    spawner.must_spawn(mqtt::minimq_task(
        network_stack,
        &config::CONFIG.mqtt_topics,
        config::CONFIG.mqtt_endpoint,
        LOG_CHANNEL.receiver(),
    ));

    // Once main() exists, the executor continues to run already spawned tasks forever.
}
