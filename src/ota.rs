//ota.get_or_insert_with(|| Ota::new(FlashStorage::new()).unwrap())

use alloc::{boxed::Box, format};
use embassy_futures::select::select3;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::Timer;
use esp_hal_ota::Ota;
use esp_storage::FlashStorage;
use serde::Deserialize;

use crate::{iot_topic, mqtt::mqtt_send};

#[derive(Debug, Deserialize)]
struct Start {
    size: u32,
    target_crc: u32,
}

#[derive(Deserialize)]
struct Write {}

static START_CH: Channel<CriticalSectionRawMutex, Start, 1> = Channel::new();
static WRTIE_CH: Channel<CriticalSectionRawMutex, Box<[u8]>, 4> = Channel::new();

pub fn ota_start(payload: &[u8]) {
    let r = serde_json_core::from_slice(payload);
    if let Err(e) = r {
        mqtt_send(
            format!("Could not deserialize: {}", e).as_bytes(),
            concat!(iot_topic!(), "/ota/log"),
        );
    } else {
        let r = START_CH.try_send(r.unwrap().0);
        if let Err(_) = r {
            mqtt_send(
                "Full start queue, dropping".as_bytes(),
                concat!(iot_topic!(), "/ota/log"),
            );
        }
    }
}

pub fn ota_write(payload: Box<[u8]>) {
    let r = WRTIE_CH.try_send(payload);
    if let Err(_) = r {
        mqtt_send(
            "Full write queue, dropping".as_bytes(),
            concat!(iot_topic!(), "/ota/log"),
        );
    }
}

#[embassy_executor::task]
pub async fn ota_task() {
    let mut ota = Ota::new(FlashStorage::new()).unwrap();

    loop {
        match select3(
            START_CH.receive(),
            WRTIE_CH.receive(),
            Timer::after_secs(60 * 60),
        )
        .await
        {
            embassy_futures::select::Either3::First(s) => {
                ota.ota_begin(s.size, s.target_crc)
                    .inspect_err(|e| {
                        mqtt_send(
                            format!("Could not begin: {:?}", e).as_bytes(),
                            concat!(iot_topic!(), "/ota/log"),
                        );
                    })
                    .ok();
                mqtt_send(&[], concat!(iot_topic!(), "/ota/ready"));
            }
            embassy_futures::select::Either3::Second(c) => {
                let r = ota
                    .ota_write_chunk(&c)
                    .inspect_err(|e| {
                        mqtt_send(
                            format!("Could not write: {:?}", e).as_bytes(),
                            concat!(iot_topic!(), "/ota/log"),
                        );
                    })
                    .ok();

                mqtt_send(
                    format!("{:#?}", ota.get_ota_progress()).as_bytes(),
                    concat!(iot_topic!(), "/ota/log"),
                );

                if let Some(true) = r {
                    let r = ota.ota_flush(true, true).inspect_err(|e| {
                        mqtt_send(
                            format!("Could not flush: {:?}", e).as_bytes(),
                            concat!(iot_topic!(), "/ota/log"),
                        );
                    });
                    if r.is_ok() {
                        mqtt_send(b"Reseting...", concat!(iot_topic!(), "/ota/log"));
                        Timer::after_secs(2).await;
                        esp_hal::system::software_reset();
                    }
                }

                mqtt_send(&[], concat!(iot_topic!(), "/ota/ready"));
            }
            embassy_futures::select::Either3::Third(_) => {
                ota.ota_mark_app_valid().ok();
            }
        }
    }
}
