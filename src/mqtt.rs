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

use crate::{iot_topic, led, output, tcp::tcp_send};

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

const RX_BUFFER_SIZE: usize = 1024;
const TX_BUFFER_SIZE: usize = 1024;
const MQTT_BUFFER_SIZE: usize = 2048;
const CONNECTION_PAYLOAD_SIZE: usize = 256;
const CLIENT_TIMEOUT_MS: u32 = 5000;
const PING_INTERVAL_SECS: u64 = 5;
const DNS_HOST: &str = "ssca.desrochers.space";
const MQTT_PORT: u16 = 1883;

/// Send a packet to the MQTT queue. Panics if topic or buffer is too large.
pub fn mqtt_send(buf: &[u8], topic: &str) {
    let topic = String::try_from(topic).expect("Topic too big");
    let len = buf.len();
    assert!(len < MQTT_PACKET_LEN, "Packet too big");
    let mut stack_buf = [0u8; MQTT_PACKET_LEN];
    stack_buf[..len].copy_from_slice(&buf[..len]);
    WRITE
        .try_send(Packet {
            topic,
            buf: stack_buf,
            len,
        })
        .inspect_err(|_e| defmt::error!("mqtt queue full"))
        .ok();
}

/// Sets up the MQTT client connection.
async fn setup_client<'a>(
    stack: Stack<'a>,
    rx_buffer: &'a mut [u8],
    tx_buffer: &'a mut [u8],
    mqtt_buffer: &'a mut [u8],
) -> Option<
    mountain_mqtt::client::ClientNoQueue<
        'a,
        mountain_mqtt::embedded_io_async::ConnectionEmbedded<TcpSocket<'a>>,
        MyDelay,
        fn(Message) -> Result<(), ClientError>,
    >,
> {
    let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
    let addr = stack
        .dns_query(DNS_HOST, smoltcp::wire::DnsQueryType::A)
        .await;
    if let Err(e) = addr {
        defmt::error!("dns query {:?}", defmt::Debug2Format(&e));
        Timer::after_millis(500).await;
        led::state(led::LedState::MQTT(false));
        return None;
    }
    let result_connection = socket
        .connect(IpEndpoint::new(
            addr.unwrap().first().unwrap().clone(),
            MQTT_PORT,
        ))
        .await;
    if let Err(e) = result_connection {
        defmt::error!("socket connect {:?}", defmt::Debug2Format(&e));
        Timer::after_millis(500).await;
        led::state(led::LedState::MQTT(false));
        return None;
    }
    let connection = mountain_mqtt::embedded_io_async::ConnectionEmbedded::new(socket);
    Some(mountain_mqtt::client::ClientNoQueue::new(
        connection,
        mqtt_buffer,
        MyDelay,
        CLIENT_TIMEOUT_MS,
        message_handler,
    ))
}

/// Sets up MQTT topic subscriptions.
async fn setup_subscriptions<'a>(
    client: &mut mountain_mqtt::client::ClientNoQueue<
        'a,
        mountain_mqtt::embedded_io_async::ConnectionEmbedded<TcpSocket<'a>>,
        MyDelay,
        fn(Message) -> Result<(), ClientError>,
    >,
) {
    let topics = [
        concat!(iot_topic!(), "/rpc/tcp"),
        concat!(iot_topic!(), "/ctrl"),
        concat!(iot_topic!(), "/echo"),
    ];
    for topic in topics.iter() {
        let result = client
            .subscribe(
                *topic,
                mountain_mqtt::data::quality_of_service::QualityOfService::QoS0,
            )
            .await;
        if let Err(e) = result {
            defmt::error!("{:?}", defmt::Debug2Format(&e));
            led::state(led::LedState::RPCError);
        }
    }
}

#[embassy_executor::task]
pub async fn mqtt_task(stack: Stack<'static>) -> ! {
    let rx_buffer = &mut *Box::new([0u8; RX_BUFFER_SIZE]);
    let tx_buffer = &mut *Box::new([0u8; TX_BUFFER_SIZE]);
    let mqtt_buffer = &mut *Box::new([0u8; MQTT_BUFFER_SIZE]);

    'main: loop {
        let mut client = match setup_client(stack, rx_buffer, tx_buffer, mqtt_buffer).await {
            Some(c) => c,
            None => continue 'main,
        };

        let mut will_payload = [0u8; 256];
        let will_payload_len = serde_json_core::to_slice(
            &ConnectionPacket {
                last_will: true,
                msg: heapless::String::from_str("me dead").unwrap(),
            },
            &mut will_payload,
        )
        .unwrap();

        let result_connection = client
            .connect(Connect::<0>::new(
                60,
                Some(env!("TOKEN")),
                Some(b"."),
                env!("ID"),
                true,
                Some(Will {
                    qos: mountain_mqtt::data::quality_of_service::QualityOfService::QoS0,
                    retain: false,
                    topic_name: concat!(iot_topic!(), "/connection"),
                    payload: &will_payload[..will_payload_len],
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

        let mut payload = [0u8; CONNECTION_PAYLOAD_SIZE];
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

        setup_subscriptions(&mut client).await;

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
            match select3(
                WRITE.receive(),
                client.poll(true),
                Timer::after_secs(PING_INTERVAL_SECS),
            )
            .await
            {
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

/// Handles incoming MQTT messages and dispatches actions based on topic.
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
