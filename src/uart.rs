use defmt::Debug2Format;
use embassy_futures::select::select;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::Timer;
use embedded_io_async::Write;
use esp_hal::{gpio::Output, system::software_reset, uart::Uart, Async};
use heapless::Vec;

use crate::{iot_topic, led, mqtt, wifi::WIFI_CLIENT_CONFIG_KEY};

pub static UART_PACKET_LEN: usize = 128;

struct Packet {
    buf: [u8; UART_PACKET_LEN],
    len: usize,
}

static WRITE: Channel<CriticalSectionRawMutex, Packet, 2> = Channel::new();

#[allow(dead_code)] //TODO remove
pub async fn uart_send(buf: &[u8]) {
    let mut stack_buf = [0u8; UART_PACKET_LEN];
    let len = buf.len();
    if len >= UART_PACKET_LEN {
        panic!("Packet too big");
    }
    stack_buf[..len].copy_from_slice(&buf[..len]);
    WRITE
        .send(Packet {
            buf: stack_buf,
            len,
        })
        .await;
}

#[embassy_executor::task]
pub async fn uart_task(mut uart: Uart<'static, Async>, mut de_pin: Output<'static>) {
    let mut buf = [0u8; 256];
    loop {
        match select(WRITE.receive(), uart.read_async(&mut buf)).await {
            embassy_futures::select::Either::First(pkt) => {
                de_pin.set_high();
                uart.write_all(&pkt.buf[..pkt.len])
                    .await
                    .inspect_err(|e| defmt::error!("uart write {}", e))
                    .ok();
                de_pin.set_low();
            }
            embassy_futures::select::Either::Second(Ok(len)) => {
                defmt::info!("UART received: {:02x}", &buf[..len]);
                /*if buf.starts_with(b"WIFI:S:") {
                    if let Ok(wifi_str) = core::str::from_utf8(&buf[7..len]) {
                        let parts: Vec<&str,20> = wifi_str.split(':').chain(wifi_str.split(';')).collect();
                        if parts.len() >= 7 && parts[1] == "S" && parts[3] == "T" && parts[5] == "P" {
                            let ssid = parts[2];
                            let password = parts[6];
                            let auth_method = parts[4];
                            
                            let auth = match auth_method {
                                "WPA2" => esp_wifi::wifi::AuthMethod::WPA2Personal,
                                "WPA3" => esp_wifi::wifi::AuthMethod::WPA3Personal,
                                "WEP" => esp_wifi::wifi::AuthMethod::WEP,
                                "NONE" => esp_wifi::wifi::AuthMethod::None,
                                _ => esp_wifi::wifi::AuthMethod::WPA2Personal,
                            };
                            
                            let config = esp_wifi::wifi::ClientConfiguration {
                                ssid: ssid.try_into().unwrap_or_default(),
                                password: password.try_into().unwrap_or_default(),
                                auth_method: auth,
                                ..Default::default()
                            };
                                defmt::info!("WiFi config created: {}", &config);
                            
                            let mem_guard = MEM.lock().await;
                            let mut mem = mem_guard.write_transaction().await;
                            match postcard::to_vec::<_, 256>(&config) {
                                Ok(serialized) => {
                                    if let Err(e) = mem.write(WIFI_CLIENT_CONFIG_KEY, &serialized).await {
                                        defmt::error!("Failed to write WiFi config to memory: {}", Debug2Format(&e));
                                    }else{
                                        defmt::info!("WiFi config saved, restarting ESP32...");
                                        software_reset();
                                    }
                                }
                                Err(e) => {
                                    defmt::error!("Failed to serialize WiFi config: {}", Debug2Format(&e));
                                }
                            }
                        }
                    }
                }*/
                mqtt::mqtt_send(&buf[..len], concat!(iot_topic!(), "/uart"));
            }
            embassy_futures::select::Either::Second(Err(e)) => {
                defmt::error!("uart read {}", e);
                led::state(led::LedState::UartError);
                Timer::after_secs(1).await;
            }
        }
    }
}
