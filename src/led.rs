use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::Timer;
use esp_hal::gpio::{AnyPin, Output};

#[allow(dead_code)]
#[derive(Debug)]
pub enum LedState {
    Ok,
    TCP(bool),
    MQTT(bool),
    RPCError,
}

static LED: Channel<CriticalSectionRawMutex, LedState, 10> = Channel::new();

#[embassy_executor::task]
pub async fn task(led: AnyPin) {
    let mut led = Output::new(led, esp_hal::gpio::Level::Low);

    loop {
        led.set_low();
        Timer::after_millis(500).await;
        let state = LED.receive().await;
        match state {
            LedState::Ok => {
                led.set_high();
                Timer::after_millis(50).await;
                led.set_low();
                Timer::after_millis(100).await;
                led.set_high();
                Timer::after_millis(50).await;
            }
            LedState::TCP(on) => {
                led.set_high();
                Timer::after_millis(200).await;
                if on {
                    led.set_low();
                    Timer::after_millis(100).await;
                    led.set_high();
                    Timer::after_millis(50).await;
                }
            }
            LedState::MQTT(on) => {
                led.set_high();
                Timer::after_millis(50).await;
                if on {
                    led.set_low();
                    Timer::after_millis(100).await;
                    led.set_high();
                    Timer::after_millis(200).await;
                }
            }
            LedState::RPCError =>{
                led.set_high();
                Timer::after_millis(100).await;
                led.set_low();
                Timer::after_millis(100).await;
                led.set_high();
                Timer::after_millis(100).await;
                led.set_low();
                Timer::after_millis(100).await;
                led.set_high();
                Timer::after_millis(100).await;
            }
        }
    }
}

pub fn state(state: LedState) {
    if let Err(err) = LED.try_send(state) {
        defmt::error!("{:?}", defmt::Debug2Format(&err));
    }
}
