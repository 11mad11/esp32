use core::{ascii::Char, cell::RefCell};

use embassy_futures::select::select;
use embassy_net::{tcp::TcpSocket, IpEndpoint, Stack};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::Timer;
use embedded_hal::digital::OutputPin;
use esp_hal::gpio::{AnyPin, Output};
use heapless::Vec;
use mountain_mqtt::{
    client::{Client, ClientError, Delay, Message},
    packets::connect::Connect,
};

use crate::led;

pub const MQTT_PACKET_LEN: usize = 1024;

struct Packet {
    buf: [u8; MQTT_PACKET_LEN],
    len: usize,
}

static WRITE: Channel<CriticalSectionRawMutex, Packet, 2> = Channel::new();

pub fn mqtt_send(buf: &[u8]) {
    let mut stack_buf = [0u8; MQTT_PACKET_LEN];
    let len = buf.len();
    if len >= MQTT_PACKET_LEN {
        panic!("Packet too big");
    }
    stack_buf[..len].copy_from_slice(&buf[..len]);
    WRITE
        .try_send(Packet {
            buf: stack_buf,
            len,
        })
        .inspect_err(|_e| defmt::error!("mqtt queue full"))
        .ok();
}

#[embassy_executor::task]
pub async fn mqtt_task(stack: Stack<'static>, pins: (AnyPin, AnyPin, AnyPin, AnyPin)) {
    let mut rx_buffer = [0u8; 256];
    let mut tx_buffer = [0u8; 256];
    let mut mqtt_buffer = [0u8; 256];
    let handler = message_handler(pins);

    'main: loop {
        let mut client = {
            let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);

            let addr = stack
                .dns_query("ssca.desrochers.space", smoltcp::wire::DnsQueryType::A)
                .await;

            if let Err(e) = addr {
                defmt::error!("dns query {:?}", defmt::Debug2Format(&e));
                Timer::after_millis(500).await;
                led::state(led::LedState::MQTT(false));
                continue 'main;
            }

            let result_connection = socket
                .connect(IpEndpoint::new(
                    addr.unwrap().first().unwrap().clone(),
                    1883,
                ))
                .await;

            if let Err(e) = result_connection {
                defmt::error!("socket connect {:?}", defmt::Debug2Format(&e));
                Timer::after_millis(500).await;
                led::state(led::LedState::MQTT(false));
                continue 'main;
            }

            let connection = mountain_mqtt::embedded_io_async::ConnectionEmbedded::new(socket);
            mountain_mqtt::client::ClientNoQueue::new(
                connection,
                &mut mqtt_buffer,
                MyDelay,
                5000,
                |msg| handler(msg),
            )
        };

        {
            let result_connection = client
                .connect(Connect::<0>::new(
                    60,
                    Some(env!("TOKEN")),
                    Some(&[0]),
                    env!("ID"),
                    true,
                    None,
                    Vec::new(),
                ))
                .await;
            if let Err(e) = result_connection {
                defmt::error!("connect mqtt {:?}", defmt::Debug2Format(&e));
                Timer::after_millis(500).await;
                led::state(led::LedState::MQTT(false));
                continue 'main;
            }
        }

        {
            let ctrl = client
                .subscribe(
                    concat!("iot/", env!("ID"), "/ctrl"),
                    mountain_mqtt::data::quality_of_service::QualityOfService::QoS0,
                )
                .await;
            if let Err(e) = ctrl {
                defmt::error!("{:?}", defmt::Debug2Format(&e));
                led::state(led::LedState::RPCError);
            }
        }

        led::state(led::LedState::MQTT(true));

        const TOPIC_NAME: &str = concat!("iot/", env!("ID"), "/data");
        loop {
            match select(WRITE.receive(), client.poll(true)).await {
                embassy_futures::select::Either::First(packet) => {
                    let ascii = packet.buf[..packet.len].as_ascii();
                    if let Some(ascii) = ascii {
                        defmt::debug!("{}", defmt::Debug2Format(ascii));
                    }
                    let r = client
                        .publish(
                            TOPIC_NAME,
                            &packet.buf[..packet.len],
                            mountain_mqtt::data::quality_of_service::QualityOfService::QoS0,
                            false,
                        )
                        .await;
                    if let Err(e) = r {
                        defmt::error!("publish mqtt {:?}", defmt::Debug2Format(&e));
                        Timer::after_millis(500).await;
                        led::state(led::LedState::MQTT(false));
                        continue 'main;
                    }
                }
                embassy_futures::select::Either::Second(Ok(true)) => {}
                embassy_futures::select::Either::Second(Ok(false)) => {
                    Timer::after_millis(10).await;
                }
                embassy_futures::select::Either::Second(Err(e)) => {
                    defmt::error!("poll mqtt {:?}", defmt::Debug2Format(&e));
                    led::state(led::LedState::MQTT(false));
                    Timer::after_millis(500).await;
                    continue 'main;
                }
            };
        }
    }
}

pub fn message_handler(
    pins: (AnyPin, AnyPin, AnyPin, AnyPin),
) -> impl Fn(Message) -> Result<(), ClientError> {
    let one = Char::digit(1).unwrap();
    let pins = RefCell::new((
        Output::new(pins.0, esp_hal::gpio::Level::High),
        Output::new(pins.1, esp_hal::gpio::Level::High),
        Output::new(pins.2, esp_hal::gpio::Level::High),
        Output::new(pins.3, esp_hal::gpio::Level::High),
    ));

    move |msg: Message| -> Result<(), ClientError> {
        let ascii = msg.payload.as_ascii().unwrap();
        defmt::info!("{}", defmt::Debug2Format(ascii));

        if ascii.len() >= 4 {
            let mut pins = pins.borrow_mut();
            pins.0
                .set_state(if ascii[0] == one {
                    embedded_hal::digital::PinState::Low
                } else {
                    embedded_hal::digital::PinState::High
                })
                .unwrap();
            pins.1
                .set_state(if ascii[1] == one {
                    embedded_hal::digital::PinState::Low
                } else {
                    embedded_hal::digital::PinState::High
                })
                .unwrap();
            pins.2
                .set_state(if ascii[2] == one {
                    embedded_hal::digital::PinState::Low
                } else {
                    embedded_hal::digital::PinState::High
                })
                .unwrap();
            pins.3
                .set_state(if ascii[3] == one {
                    embedded_hal::digital::PinState::Low
                } else {
                    embedded_hal::digital::PinState::High
                })
                .unwrap();
        } else {
            defmt::warn!("Payload too short to control pins");
        }

        Ok(())
    }
}

struct MyDelay;
impl Delay for MyDelay {
    async fn delay_us(&mut self, us: u32) {
        Timer::after_micros(us as u64).await
    }
}
