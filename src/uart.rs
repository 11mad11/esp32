use alloc::{vec, vec::Vec};
use embassy_futures::select::select;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::Timer;
use embedded_io_async::Write;
use esp_hal::{Async, gpio::Output, uart::Uart};

use crate::{
    iot_topic, led, mqtt,
    serial_frame::{self, SerialFrameAssembler, SerialFrameEvent, SERIAL_TO_MQTT_MAX_BODY},
};

pub static UART_PACKET_LEN: usize = 128;
const SERIAL_TO_UART_PROTOCOL_ENABLED: bool = option_env!("SERIAL_TO_UART").is_some();

struct Packet {
    buf: Vec<u8>,
    len: usize,
}

static WRITE: Channel<CriticalSectionRawMutex, Packet, 2> = Channel::new();

#[allow(dead_code)] //TODO remove
pub async fn uart_send(buf: &[u8]) {
    let len = buf.len();
    if len >= UART_PACKET_LEN {
        panic!("Packet too big");
    }
    let mut heap_buf = vec![0u8; len];
    heap_buf.copy_from_slice(&buf[..len]);
    WRITE.send(Packet { buf: heap_buf, len }).await;
}

#[embassy_executor::task]
pub async fn uart_task(uart: Uart<'static, Async>, de_pin: Output<'static>) {
    if SERIAL_TO_UART_PROTOCOL_ENABLED {
        uart_protocol_loop(uart, de_pin).await;
    } else {
        uart_basic_loop(uart, de_pin).await;
    }
}

// ---------------------------------------------------------------------------
// Basic mode – raw bytes received on UART are forwarded to MQTT as-is.
// ---------------------------------------------------------------------------

async fn uart_basic_loop(mut uart: Uart<'static, Async>, mut de_pin: Output<'static>) {
    let mut buf = vec![0u8; 256];
    loop {
        match select(WRITE.receive(), uart.read_async(&mut buf[..])).await {
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
                mqtt::mqtt_send(&buf[..len], concat!(iot_topic!(), "/uart")).await;
            }
            embassy_futures::select::Either::Second(Err(e)) => {
                defmt::error!("uart read {}", e);
                led::state(led::LedState::UartError).await;
                Timer::after_secs(1).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Protocol mode – COBS-framed serial protocol, parsed and forwarded to
// per-channel MQTT topics (same framing that was previously on TCP).
//
// Wire format (UART):  0x00 <COBS-encoded frame> 0x00
// Frame format after COBS decode: same binary layout as the TCP hex mode.
// ---------------------------------------------------------------------------

async fn uart_protocol_loop(mut uart: Uart<'static, Async>, mut de_pin: Output<'static>) {
    let mut assembler = SerialFrameAssembler::new(vec![0u8; SERIAL_TO_MQTT_MAX_BODY]);
    let mut read_buf = vec![0u8; 256];

    loop {
        match select(WRITE.receive(), uart.read_async(&mut read_buf[..])).await {
            embassy_futures::select::Either::First(pkt) => {
                de_pin.set_high();
                uart.write_all(&pkt.buf[..pkt.len])
                    .await
                    .inspect_err(|e| defmt::error!("uart write {}", e))
                    .ok();
                de_pin.set_low();
            }
            embassy_futures::select::Either::Second(Ok(len)) => {
                // A single read may contain multiple frames or partial frames;
                // process all consumed bytes before going back to the select.
                let mut pos = 0;
                while pos < len {
                    let (consumed, event) = assembler.feed(&mut read_buf[pos..len]);
                    pos += consumed;

                    match event {
                        Some(SerialFrameEvent::Ready) => {
                            let result =
                                serial_frame::prepare_dispatch_cobs(assembler.frame_slice_mut());
                            match result {
                                Ok((topic, payload)) => {
                                    mqtt::mqtt_send(payload, topic.as_str()).await;
                                }
                                Err(e) => {
                                    defmt::warn!("uart protocol frame dropped: {:?}", e);
                                }
                            }
                            assembler.reset();
                        }
                        Some(SerialFrameEvent::Overflow) => {
                            defmt::warn!(
                                "uart protocol frame overflow (>{} bytes)",
                                SERIAL_TO_MQTT_MAX_BODY
                            );
                            assembler.reset();
                        }
                        None => {}
                    }

                    if consumed == 0 {
                        break; // guard against infinite loop on unexpected input
                    }
                }
            }
            embassy_futures::select::Either::Second(Err(e)) => {
                defmt::error!("uart read {}", e);
                led::state(led::LedState::UartError).await;
                Timer::after_secs(1).await;
            }
        }
    }
}
