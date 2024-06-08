/// Interacts with the Flair58 heating device: detects its state from the LED changes, and
/// manipulates the state by emulating the button press.
use crate::mqtt_log;
use embassy_rp::{gpio, peripherals};
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};

// Power levels of the device, as labelled on it.
#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) enum PowerLevel {
    Low,
    Medium,
    High,
}

// The device state observed from LEDs.
#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) enum DeviceState {
    // All LEDs are off.
    Off,
    // Happens if something went wrong (the device is producing unknown led patterns), or for some
    // transitional states: for example, when the device turns off, all its LEDs are considered
    // blinking for a short time, and all LEDs blinking is not a valid state.
    Unknown,
    // All LEDs before the given power level are on, the LEDs at the given power level is blinking,
    // and LEDs after the given power level are off.
    Heating(PowerLevel),
    // LEDs before and at the given power level are on, and LEDs after the given power level are
    // off.
    On(PowerLevel),
}

impl DeviceState {
    // Represents the state as a bytes string, for publishing in MQTT topic.
    pub(crate) fn as_bytes(&self) -> &'static [u8] {
        match self {
            DeviceState::Off => b"off",
            DeviceState::Unknown => b"unknown",
            DeviceState::Heating(PowerLevel::Low) => b"heating_low",
            DeviceState::Heating(PowerLevel::Medium) => b"heating_medium",
            DeviceState::Heating(PowerLevel::High) => b"heating_high",
            DeviceState::On(PowerLevel::Low) => b"on_low",
            DeviceState::On(PowerLevel::Medium) => b"on_medium",
            DeviceState::On(PowerLevel::High) => b"on_high",
        }
    }
}

// Returns the currently known state of the device. This function returns fast and does not perform
// any IO.
pub(crate) async fn get_current_state(now: Instant) -> DeviceState {
    DEVICE_STATE_MANAGER.lock().await.state(now)
}

// Target state for the device, to be set by emulating a button press.
#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) enum TargetState {
    // Considered reached when the device is Off.
    Off,
    // Considered reached when the device is either Heating or On for the given level.
    On(PowerLevel),
}

// Sets the target state. This function returns fast and does not perform the state actuation: it is
// done in a different background task.
pub(crate) async fn set_target_state(state: TargetState) {
    *TARGET_STATE.lock().await = state;
}

// Duration after which the LED is considered not blinking and steady.
const BLINK_DURATION: Duration = Duration::from_millis(900);

enum LedState {
    // Off for at least BLINK_DURATION.
    Off,
    // On for at least BLINK_DURATION.
    On,
    // Changed the state within BLINK_DURATION.
    Blinking,
}

fn led_state((last_instant, last_level): &(Instant, gpio::Level), now: Instant) -> LedState {
    if now.duration_since(*last_instant) > BLINK_DURATION {
        match last_level {
            gpio::Level::Low => LedState::Off,
            gpio::Level::High => LedState::On,
        }
    } else {
        LedState::Blinking
    }
}

// Stores the last observed LED state for all LEDs on the device, and computes the device state
// based on this.
struct DeviceStateManager {
    leds: [(Instant, gpio::Level); 3], // [PowerLevel::Low, PowerLevel::Medium, PowerLevel::High].
}

static DEVICE_STATE_MANAGER: Mutex<ThreadModeRawMutex, DeviceStateManager> =
    Mutex::new(DeviceStateManager::new());

impl DeviceStateManager {
    const fn new() -> DeviceStateManager {
        DeviceStateManager {
            leds: [(Instant::MIN, gpio::Level::Low); 3],
        }
    }

    fn update(&mut self, led: PowerLevel, level: gpio::Level, now: Instant) {
        let last = &mut self.leds[led as usize];
        if last.1 != level {
            *last = (now, level);
        }
    }

    fn state(&self, now: Instant) -> DeviceState {
        match (
            led_state(&self.leds[0], now),
            led_state(&self.leds[1], now),
            led_state(&self.leds[2], now),
        ) {
            (LedState::Off, LedState::Off, LedState::Off) => DeviceState::Off,
            (LedState::On, LedState::Off, LedState::Off) => DeviceState::On(PowerLevel::Low),
            (LedState::On, LedState::On, LedState::Off) => DeviceState::On(PowerLevel::Medium),
            (LedState::On, LedState::On, LedState::On) => DeviceState::On(PowerLevel::High),
            (LedState::Blinking, LedState::Off, LedState::Off) => {
                DeviceState::Heating(PowerLevel::Low)
            }
            (LedState::On, LedState::Blinking, LedState::Off) => {
                DeviceState::Heating(PowerLevel::Medium)
            }
            (LedState::On, LedState::On, LedState::Blinking) => {
                DeviceState::Heating(PowerLevel::High)
            }
            _ => DeviceState::Unknown,
        }
    }
}

// How often the LEDs should be polled, to ensure that blinks are properly recognised.
const POLL_PERIOD: Duration = Duration::from_millis(BLINK_DURATION.as_millis() / 2 - 50);

// Polls LEDs over GPIO and logs the result to the DeviceStateManager.
#[embassy_executor::task]
pub(super) async fn led_detector_task(
    pin_low: peripherals::PIN_12,
    pin_medium: peripherals::PIN_13,
    pin_high: peripherals::PIN_14,
) -> ! {
    let mut pin_low = gpio::Input::new(pin_low, gpio::Pull::Down);
    let mut pin_medium = gpio::Input::new(pin_medium, gpio::Pull::Down);
    let mut pin_high = gpio::Input::new(pin_high, gpio::Pull::Down);

    loop {
        embassy_futures::select::select4(
            pin_low.wait_for_any_edge(),
            pin_medium.wait_for_any_edge(),
            pin_high.wait_for_any_edge(),
            // wait_for_any_edge might be racy if the pin changed its state between the last
            // get_level call and the start of the wait_for_any_edge call. So explicitly poll all
            // pins every 400 milliseconds nevertheless.
            // TODO: This can be rewritten to check the last state known to state_manager and
            // waiting for an opposite value (wait_for_high / wait_for_low) in select4() instead.
            Timer::after(POLL_PERIOD),
        )
        .await;

        {
            let mut device_state_manager = DEVICE_STATE_MANAGER.lock().await;
            let now = Instant::now();
            device_state_manager.update(PowerLevel::Low, pin_low.get_level(), now);
            device_state_manager.update(PowerLevel::Medium, pin_medium.get_level(), now);
            device_state_manager.update(PowerLevel::High, pin_high.get_level(), now);
        }
    }
}

static TARGET_STATE: Mutex<ThreadModeRawMutex, TargetState> = Mutex::new(TargetState::Off);

// Period of time after which the device being in unknown state triggers a log message.
const STATE_WARNING_TIMEOUT: Duration = Duration::from_secs(11);
// Period of time after which the device being in unknown state triggers an attempt to reset the
// device.
const RESET_TIMEOUT: Duration = Duration::from_secs(21);

enum Action {
    None,
    ShortPush,
    LongPush,
}

// Returns the action that should be performed on the button to bring the device closer to the
// target state.
fn get_action(
    current_state: DeviceState,
    target_state: TargetState,
    now: Instant,
    unknown_state_since: &mut Option<Instant>,
) -> Action {
    // Convert the current state to the corresponding target state, if possible.
    let current_state = match current_state {
        DeviceState::Off => TargetState::Off,
        DeviceState::Heating(x) | DeviceState::On(x) => TargetState::On(x),
        DeviceState::Unknown => {
            let unknown_state_for = match *unknown_state_since {
                Some(x) => now.duration_since(x),
                None => {
                    *unknown_state_since = Some(now);
                    Duration::from_nanos(0)
                }
            };
            if unknown_state_for > STATE_WARNING_TIMEOUT {
                mqtt_log!(
                    "State actuator: unknown state for {:?}ms",
                    unknown_state_for.as_millis()
                );
            }
            if unknown_state_for > RESET_TIMEOUT {
                // Try to reset the device. Also reset the unknown state timer, so that the next
                // reset attempt happens in some time.
                *unknown_state_since = None;
                return Action::LongPush;
            }
            // If the state is unknown for a short period of time, it might be some kind of
            // transition; just do nothing and hope that the transition will finish by the next
            // actuation cycle.
            return Action::None;
        }
    };
    // If the code above did not early return, the state is known.
    *unknown_state_since = None;

    match (current_state, target_state) {
        (x, y) if x == y => Action::None,
        (TargetState::Off, TargetState::On(_)) | (TargetState::On(_), TargetState::Off) => {
            Action::LongPush
        }
        // Remaining arm is when both states are TargetState::On, but with different power levels.
        _ => Action::ShortPush,
    }
}

#[embassy_executor::task]
pub(super) async fn state_actuator_task(pin: peripherals::PIN_15) -> ! {
    let mut pin = gpio::Output::new(pin, gpio::Level::High);
    let mut unknown_state_since = None;

    loop {
        let now = Instant::now();
        let target_state: TargetState = *TARGET_STATE.lock().await;
        let current_state = get_current_state(now).await;

        match get_action(current_state, target_state, now, &mut unknown_state_since) {
            Action::None => {}
            Action::ShortPush => {
                mqtt_log!(
                    "Sending short push: current_state: {:?}; target_state: {:?}",
                    current_state,
                    target_state
                );
                pin.set_low();
                Timer::after_millis(500).await;
                pin.set_high();
            }
            Action::LongPush => {
                mqtt_log!(
                    "Sending long push: current_state: {:?}; target_state: {:?}",
                    current_state,
                    target_state
                );
                pin.set_low();
                Timer::after_millis(2000).await;
                pin.set_high();
            }
        }
        // Give the device some time to settle if a button push happened.
        Timer::after_millis(5000).await;
    }
}
