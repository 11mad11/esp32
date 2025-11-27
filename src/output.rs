use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Instant, WithTimeout};
use esp_hal::gpio::Output;
use mountain_mqtt::{client::EventHandlerError, packets::publish::ApplicationMessage};

pub const NUM_OUT: usize = 4;
type Packet = [Option<u8>; NUM_OUT];

static WRITE: Channel<CriticalSectionRawMutex, Packet, 2> = Channel::new();


#[allow(dead_code)]
pub async fn output_state(relays: Packet) {
    WRITE.send(relays).await;
}

pub async fn output_state_from_mqtt<const P: usize>(
    message: ApplicationMessage<'_, P>,
) -> Result<(), EventHandlerError> {
    let ascii = core::str::from_utf8(message.payload)
        .map_err(|_| EventHandlerError::InvalidApplicationMessage)?;
    defmt::info!("{}", ascii);
    let mut bytes = [None; NUM_OUT];
    for (i, chunk) in ascii.as_bytes().chunks(2).take(NUM_OUT).enumerate() {
        if chunk.len() != 2 {
            defmt::error!("Incomplete hex pair at index {}", i);
            continue;
        }
        match core::str::from_utf8(chunk)
            .ok()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            Some(byte) => bytes[i] = Some(byte),
            None => defmt::error!("Invalid hex pair at index {}", i),
        }
    }
    WRITE.send(bytes).await;
    Ok(())
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
