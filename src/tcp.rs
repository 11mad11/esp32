use embassy_futures::select::{self, select};
use embassy_net::{tcp::TcpSocket, Stack};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Timer};

use crate::{
    iot_topic, led,
    mqtt::{mqtt_send, MQTT_PACKET_LEN},
};

pub static TCP_PACKET_LEN: usize = 64;

struct Packet {
    buf: [u8; TCP_PACKET_LEN],
    len: usize,
}

static WRITE: Channel<CriticalSectionRawMutex, Packet, 2> = Channel::new();

pub fn tcp_send(buf: &[u8]) {
    let mut stack_buf = [0u8; TCP_PACKET_LEN];
    let len = buf.len();
    if len >= TCP_PACKET_LEN {
        panic!("Packet too big");
    }
    stack_buf[..len].copy_from_slice(&buf[..len]);
    WRITE
        .try_send(Packet {
            buf: stack_buf,
            len,
        })
        .inspect_err(|_e| defmt::error!("tcp queue full"))
        .ok();
}

#[embassy_executor::task]
pub async fn tcp_task(stack: Stack<'static>) {
    loop {
        let mut rx_buffer = [0u8; 1024];
        let mut tx_buffer = [0u8; 1024];

        loop {
            let mut socket = {
                let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
                socket.set_timeout(Some(Duration::from_secs(10)));
                if let Err(e) = socket.accept(80).await {
                    defmt::info!("accept error: {:?}", defmt::Debug2Format(&e));
                    continue;
                }

                defmt::info!(
                    "accepted connection from {:?}",
                    defmt::Debug2Format(&socket.remote_endpoint())
                );
                led::state(led::LedState::Ok);
                socket
            };

            loop_s(&mut socket).await;

            socket.close();

            Timer::after_secs(1).await;
        }
    }
}

async fn loop_s<'a>(socket: &mut TcpSocket<'a>) {
    let mut accum = [0u8; MQTT_PACKET_LEN];
    let mut index: usize = 0;

    loop {
        let read_future = socket.read_with(|buf| {
            defmt::debug!("{}", defmt::Debug2Format(buf));
            let mut i = 0;
            loop {
                accum[index] = buf[i];
                let current = buf[i];
                index += 1;
                i += 1;

                if current == 10 {
                    return (i, Some(()));
                }

                if i >= buf.len() {
                    break;
                }

                if index >= accum.len() {
                    panic!("Buffer overflow"); //TODO handle this better
                }
            }

            (buf.len(), None)
        });

        match select(read_future, WRITE.receive()).await {
            select::Either::First(Err(_)) => break, //connection was reset
            select::Either::First(Ok(Some(_))) => {
                mqtt_send(&accum[..index], concat!(iot_topic!(), "/data"));
                index = 0;
            }
            select::Either::First(Ok(None)) => (), //receive part of the packet, wait for the rest
            select::Either::Second(pk) => {
                socket.write(&pk.buf[..pk.len]).await.unwrap();
            }
        }
    }
}
