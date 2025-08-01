use core::str::FromStr;

use alloc::boxed::Box;
use embassy_futures::select::select3;
use embassy_net::{tcp::TcpSocket, IpEndpoint, Stack};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::Timer;
use heapless::{String, Vec};
use mountain_mqtt::{
    client::{Client, ClientError, Delay, Message},
    packets::connect::{Connect, Will},
};
use serde::Serialize;

use crate::{
    iot_topic, led,
    ota::{ota_start, ota_write},
    output,
    tcp::tcp_send,
};

#[derive(Debug, Serialize)]
struct ConnectionPacket {
    msg: String<64>,
    last_will: bool,
}

pub const MQTT_PACKET_LEN: usize = 1024;

struct Packet {
    topic: String<64>,
    buf: [u8; MQTT_PACKET_LEN],
    len: usize,
}

static WRITE: Channel<CriticalSectionRawMutex, Packet, 8> = Channel::new();

pub fn mqtt_send(buf: &[u8], topic: &str) {
    let topic = String::try_from(topic);
    if let Err(_) = topic {
        panic!("Topic too big"); //TODO
    }

    let mut stack_buf = [0u8; MQTT_PACKET_LEN];
    let len = buf.len();
    if len >= MQTT_PACKET_LEN {
        panic!("Packet too big"); //TODO
    }
    stack_buf[..len].copy_from_slice(&buf[..len]);
    WRITE
        .try_send(Packet {
            topic: topic.unwrap(),
            buf: stack_buf,
            len,
        })
        .inspect_err(|_e| defmt::error!("mqtt queue full"))
        .ok();
}

#[embassy_executor::task]
pub async fn mqtt_task(stack: Stack<'static>) {
    let rx_buffer = &mut *Box::new([0u8; 1024]);
    let tx_buffer = &mut *Box::new([0u8; 1024]);
    let mqtt_buffer = &mut *Box::new([0u8; 2048]);

    'main: loop {
        let mut client = {
            let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
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
                mqtt_buffer,
                MyDelay,
                5000,
                message_handler,
            )
        };

        {
            let mut payload = [0u8; 256];
            let payload_len = serde_json_core::to_slice(
                &ConnectionPacket {
                    last_will: true,
                    msg: heapless::String::from_str("me dead").unwrap(),
                },
                &mut payload,
            )
            .unwrap();
            let result_connection = client
                .connect(Connect::<0>::new(
                    60,
                    Some(env!("TOKEN")),
                    Some(&[0]),
                    env!("ID"),
                    true,
                    Some(Will {
                        qos: mountain_mqtt::data::quality_of_service::QualityOfService::QoS0,
                        retain: false,
                        topic_name: concat!(iot_topic!(), "/connection"),
                        payload: &payload[..payload_len],
                        properties: Vec::new(),
                    }),
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
            let mut payload = [0u8; 256];
            let payload_len = serde_json_core::to_slice(
                &ConnectionPacket {
                    last_will: false,
                    msg: heapless::String::from_str("me alive").unwrap(),
                },
                &mut payload,
            )
            .unwrap();

            client
                .publish(
                    concat!(iot_topic!(), "/connection"),
                    &payload[..payload_len],
                    mountain_mqtt::data::quality_of_service::QualityOfService::QoS0,
                    false,
                )
                .await
                .ok();
        }

        {
            let ctrl = client
                .subscribe(
                    concat!(iot_topic!(), "/rpc/tcp"),
                    mountain_mqtt::data::quality_of_service::QualityOfService::QoS0,
                )
                .await;
            if let Err(e) = ctrl {
                defmt::error!("{:?}", defmt::Debug2Format(&e));
                led::state(led::LedState::RPCError);
            }
        }

        {
            let result = client
                .subscribe(
                    concat!(iot_topic!(), "/ctrl"),
                    mountain_mqtt::data::quality_of_service::QualityOfService::QoS0,
                )
                .await;
            if let Err(e) = result {
                defmt::error!("{:?}", defmt::Debug2Format(&e));
                led::state(led::LedState::RPCError);
            }

            let result = client
                .subscribe(
                    concat!(iot_topic!(), "/ota/start"),
                    mountain_mqtt::data::quality_of_service::QualityOfService::QoS0,
                )
                .await;
            if let Err(e) = result {
                defmt::error!("{:?}", defmt::Debug2Format(&e));
                led::state(led::LedState::RPCError);
            }

            let result = client
                .subscribe(
                    concat!(iot_topic!(), "/ota/data"),
                    mountain_mqtt::data::quality_of_service::QualityOfService::QoS0,
                )
                .await;
            if let Err(e) = result {
                defmt::error!("{:?}", defmt::Debug2Format(&e));
                led::state(led::LedState::RPCError);
            }

            let result = client
                .subscribe(
                    concat!(iot_topic!(), "/echo"),
                    mountain_mqtt::data::quality_of_service::QualityOfService::QoS0,
                )
                .await;
            if let Err(e) = result {
                defmt::error!("{:?}", defmt::Debug2Format(&e));
                led::state(led::LedState::RPCError);
            }
        }

        led::state(led::LedState::MQTT(true));
        client
            .publish(
                concat!(iot_topic!(), "/logs"),
                b"Connected!",
                mountain_mqtt::data::quality_of_service::QualityOfService::QoS0,
                false,
            )
            .await
            .ok();

        loop {
            match select3(WRITE.receive(), client.poll(true), Timer::after_secs(5)).await {
                embassy_futures::select::Either3::First(packet) => {
                    let ascii = packet.buf[..packet.len].as_ascii();
                    if let Some(ascii) = ascii {
                        defmt::debug!("{}", defmt::Debug2Format(ascii));
                    }
                    let r = client
                        .publish(
                            &packet.topic,
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
                embassy_futures::select::Either3::Second(Ok(true)) => {}
                embassy_futures::select::Either3::Second(Ok(false)) => {
                    Timer::after_millis(10).await;
                }
                embassy_futures::select::Either3::Second(Err(e)) => {
                    defmt::error!("poll mqtt {:?}", defmt::Debug2Format(&e));
                    led::state(led::LedState::MQTT(false));
                    Timer::after_millis(500).await;
                    continue 'main;
                }
                embassy_futures::select::Either3::Third(_) => {
                    let result = client.send_ping().await;
                    if let Err(e) = result {
                        defmt::error!("{:?}", defmt::Debug2Format(&e));
                        led::state(led::LedState::RPCError);
                    }
                }
            };
        }
    }
}

pub fn message_handler(msg: Message) -> Result<(), ClientError> {
    match msg.topic_name {
        concat!(iot_topic!(), "/ctrl") => {
            let ascii = msg.payload.as_ascii().unwrap();
            defmt::info!("{}", defmt::Debug2Format(ascii));

            let mut bytes = [None; 4];
            for (i, chunk) in ascii.chunks(2).take(4).enumerate() {
                if let Ok(byte) = u8::from_str_radix(chunk.as_str(), 16) {
                    bytes[i] = Some(byte);
                } else {
                    defmt::error!("Invalid hex byte: {}", defmt::Debug2Format(chunk));
                }
            }
            output::output_state(bytes);
        }
        concat!(iot_topic!(), "/ota/start") => {
            ota_start(msg.payload);
        }
        concat!(iot_topic!(), "/ota/data") => {
            ota_write(Box::from(msg.payload));
        }
        concat!(iot_topic!(), "/rpc/tcp") => {
            tcp_send(msg.payload);
        }
        concat!(iot_topic!(), "/echo") => {
            mqtt_send(msg.payload, "/echo");
        }
        _ => (),
    }
    Ok(())
}

struct MyDelay;
impl Delay for MyDelay {
    async fn delay_us(&mut self, us: u32) {
        Timer::after_micros(us as u64).await
    }
}
