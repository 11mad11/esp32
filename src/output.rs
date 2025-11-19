use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Instant, WithTimeout};
use esp_hal::gpio::Output;

pub const NUM_OUT: usize = 4;
type Packet = [Option<u8>; NUM_OUT];

static WRITE: Channel<CriticalSectionRawMutex, Packet, 2> = Channel::new();

pub async fn output_state(relays: Packet) {
    WRITE.send(relays).await;
}

#[embassy_executor::task]
pub async fn output_task(mut pins: [Output<'static>; NUM_OUT]) {
    let mut timers: [Option<Instant>; NUM_OUT] = [None; NUM_OUT];

    loop {
        let soonest = timers
            .iter()
            .filter_map(|&t| t)
            .min_by_key(|t| t.as_secs())
            .unwrap_or(Instant::MAX);

        match WRITE.receive().with_deadline(soonest).await {
            Ok(pkt) => {
                for (i, &value) in pkt.iter().enumerate() {
                    if let Some(seconds) = value {
                        if seconds == 0u8 {
                            timers[i] = None;
                            pins[i].set_high();
                        } else if seconds == 255u8 {
                            timers[i] = Some(Instant::MAX);
                            pins[i].set_low();
                        } else {
                            timers[i] = Some(
                                Instant::now() + embassy_time::Duration::from_secs(seconds as u64),
                            );
                            pins[i].set_low();
                        }
                    }
                }
            }
            Err(_) => {
                for (i, pin) in pins.iter_mut().enumerate() {
                    if let Some(timer) = timers[i] {
                        if timer < Instant::now() {
                            pin.set_high();
                            timers[i] = None;
                        } else {
                            pin.set_low();
                        }
                    }
                }
            }
        }
    }
}
