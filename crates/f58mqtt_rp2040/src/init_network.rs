/// Bilerplate code to connects to WiFi, receive a DHCPv4 address, and start all networking
/// background tasks.
///
/// Mostly copy-pasted from embassy/examples/rp/src/bin/wifi_tcp_server.rs.
use crate::config::WifiConfig;
use cyw43_pio::PioSpi;
use embassy_executor::Spawner;
use embassy_net::{Config, Stack, StackResources};
use embassy_rp::{gpio, peripherals, pio};
use static_cell::StaticCell;

#[embassy_executor::task]
async fn wifi_task(
    runner: cyw43::Runner<
        'static,
        gpio::Output<'static>,
        PioSpi<'static, peripherals::PIO0, 0, peripherals::DMA_CH0>,
    >,
) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(stack: &'static Stack<cyw43::NetDriver<'static>>) -> ! {
    stack.run().await
}

// Returns the network stack once it ready (meaning: conencted and received IPv4 address from DHCP).
// Never returns errors, as it always retries failures.
#[allow(clippy::too_many_arguments)]
pub(super) async fn init_network(
    spawner: Spawner,
    wifi_config: &WifiConfig,
    pin_23: peripherals::PIN_23,
    pin_24: peripherals::PIN_24,
    pin_25: peripherals::PIN_25,
    pin_29: peripherals::PIN_29,
    pio0: peripherals::PIO0,
    dma_ch0: peripherals::DMA_CH0,
) -> &'static Stack<cyw43::NetDriver<'static>> {
    // Firmware, embedded into the binary.
    let fw = include_bytes!("../../../embassy/cyw43-firmware/43439A0.bin");
    let clm = include_bytes!("../../../embassy/cyw43-firmware/43439A0_clm.bin");

    let pwr = gpio::Output::new(pin_23, gpio::Level::Low);
    let cs = gpio::Output::new(pin_25, gpio::Level::High);
    let mut pio = pio::Pio::new(pio0, crate::Irqs);
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        pio.irq0,
        cs,
        pin_24,
        pin_29,
        dma_ch0,
    );
    static STATE: StaticCell<cyw43::State> = StaticCell::new();
    let state = STATE.init(cyw43::State::new());
    let (net_device, mut control, runner) = cyw43::new(state, pwr, spi, fw).await;
    spawner.must_spawn(wifi_task(runner));

    log::info!("initializing wifi...");
    control.init(clm).await;
    control
        .set_power_management(cyw43::PowerManagementMode::PowerSave)
        .await;
    log::info!("wifi initialized");

    static STACK: StaticCell<Stack<cyw43::NetDriver<'static>>> = StaticCell::new();
    static RESOURCES: StaticCell<StackResources<2>> = StaticCell::new();
    let stack = &*STACK.init(Stack::new(
        net_device,
        Config::dhcpv4(Default::default()),
        RESOURCES.init(StackResources::<2>::new()),
        0x2112_1221_2195_5659,
    ));
    spawner.must_spawn(net_task(stack));
    log::info!("joining wifi...");
    loop {
        match control
            .join_wpa2(wifi_config.wifi_network, wifi_config.wifi_password)
            .await
        {
            Ok(_) => break,
            Err(err) => log::warn!("cannot join the network: {}; retrying...", err.status),
        }
    }
    log::info!("wifi joined. waiting for dhcp...");
    stack.wait_config_up().await;
    log::info!(
        "dhcp done; address is {}",
        stack.config_v4().unwrap().address.address()
    );

    control.gpio_set(0, true).await; // LED means connected.

    stack
}
