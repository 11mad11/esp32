use alloc::vec::Vec;
use embassy_futures::select::select;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::Timer;
use embedded_io_async::Write;
use esp_alloc::EspHeap;
use esp_hal::{gpio::Output, uart::Uart, Async};

use crate::{iot_topic, led, mqtt};

pub static UART_PACKET_LEN: usize = 128;

struct Packet {
    buf: Vec<u8, &'static EspHeap>,
    len: usize,
}

static WRITE: Channel<CriticalSectionRawMutex, Packet, 2> = Channel::new();

#[allow(dead_code)] //TODO remove
pub async fn uart_send(buf: &[u8]) {
    let len = buf.len();
    if len >= UART_PACKET_LEN {
        panic!("Packet too big");
    }
    let mut heap_buf = crate::vec_in_myheap!(0u8; len);
    heap_buf.copy_from_slice(&buf[..len]);
    WRITE.send(Packet { buf: heap_buf, len }).await;
}

#[embassy_executor::task]
pub async fn uart_task(mut uart: Uart<'static, Async>, mut de_pin: Output<'static>) {
    let mut buf = crate::vec_in_myheap!(0u8; 256);
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
