use core::{convert::TryInto, str::FromStr};

use alloc::vec::Vec;
use alloc::vec;
use embassy_net::{IpEndpoint, Stack, tcp::TcpSocket};
use embassy_time::{Duration, Timer, WithTimeout};
use heapless::String;
use mountain_mqtt::client::{Client, ClientNoQueue, Delay};
use serde::Serialize;

use crate::{iot_topic, led};

use super::inbound::{InboundEventHandler, MAX_APPLICATION_PROPERTIES};

pub const RX_BUFFER_SIZE: usize = 2024;
pub const TX_BUFFER_SIZE: usize = 2024;
pub const MQTT_BUFFER_SIZE: usize = 2024;
pub const CLIENT_TIMEOUT_MS: u32 = 5000;

const CONNECTION_PAYLOAD_SIZE: usize = 256;
const CONNECTION_MSG_CAPACITY: usize = 64;

const DNS_HOST: &str = "ssca.desrochers.space";
const MQTT_PORT: u16 = 1883;

#[derive(Debug, Serialize)]
struct ConnectionPacket {
    msg: String<CONNECTION_MSG_CAPACITY>,
    last_will: bool,
}

pub type ClientType<'a> = ClientNoQueue<
    'a,
    mountain_mqtt::embedded_io_async::ConnectionEmbedded<TcpSocket<'a>>,
    MyDelay,
    InboundEventHandler,
    MAX_APPLICATION_PROPERTIES,
>;

pub(super) async fn setup_client<'a>(
    stack: Stack<'a>,
    rx_buffer: &'a mut [u8],
    tx_buffer: &'a mut [u8],
    mqtt_buffer: &'a mut [u8],
) -> Option<ClientType<'a>> {
    let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
    let addr = stack
        .dns_query(DNS_HOST, smoltcp::wire::DnsQueryType::A)
        .await;
    if let Err(e) = addr {
        defmt::error!("dns query {:?}", defmt::Debug2Format(&e));
        Timer::after_millis(500).await;
        led::state(led::LedState::MQTT(false)).await;
        return None;
    }
    defmt::info!("MQTT DNS addr: {:?}", addr.as_ref().map(|a| defmt::Debug2Format(a)));
    let result_connection = socket
        .connect(IpEndpoint::new(
            addr.unwrap().first().unwrap().clone(),
            // smoltcp::wire::IpAddress::Ipv4(Ipv4Addr::new(192, 168, 2, 14)),
            MQTT_PORT,
        )).with_timeout(Duration::from_secs(2))
        .await;
    if let Err(e) = result_connection {
        defmt::error!("socket connect {:?}", defmt::Debug2Format(&e));
        Timer::after_millis(500).await;
        led::state(led::LedState::MQTT(false)).await;
        return None;
    }
    let connection = mountain_mqtt::embedded_io_async::ConnectionEmbedded::new(socket);
    Some(ClientNoQueue::new(
        connection,
        mqtt_buffer,
        MyDelay,
        CLIENT_TIMEOUT_MS,
        InboundEventHandler,
    ))
}

pub(super) async fn setup_subscriptions<'a>(client: &mut ClientType<'a>) {
    let topics = [
        concat!(iot_topic!(), "/rpc/tcp"),
        concat!(iot_topic!(), "/ctrl"),
        concat!(iot_topic!(), "/echo"),
    ];
    for topic in topics.iter() {
        let result = client
            .subscribe(
                *topic,
                mountain_mqtt::data::quality_of_service::QualityOfService::Qos0,
            )
            .await;
        if let Err(e) = result {
            defmt::error!("{:?}", defmt::Debug2Format(&e));
            led::state(led::LedState::RPCError).await;
        }
    }
}

pub(super) struct MyDelay;
impl Delay for MyDelay {
    async fn delay_us(&mut self, us: u32) {
        Timer::after_micros(us as u64).await
    }
}

pub(super) fn alloc_buffers() -> (
    &'static mut [u8; RX_BUFFER_SIZE],
    &'static mut [u8; TX_BUFFER_SIZE],
    &'static mut [u8; MQTT_BUFFER_SIZE],
) {
    let rx_buffer_vec = vec![0u8; RX_BUFFER_SIZE];
    let rx_buffer: &'static mut [u8; RX_BUFFER_SIZE] = rx_buffer_vec
        .leak()
        .try_into()
        .expect("failed to convert RX buffer slice into array");

    let tx_buffer_vec = vec![0u8; TX_BUFFER_SIZE];
    let tx_buffer: &'static mut [u8; TX_BUFFER_SIZE] = tx_buffer_vec
        .leak()
        .try_into()
        .expect("failed to convert TX buffer slice into array");

    let mqtt_buffer_vec = vec![0u8; MQTT_BUFFER_SIZE];
    let mqtt_buffer: &'static mut [u8; MQTT_BUFFER_SIZE] = mqtt_buffer_vec
        .leak()
        .try_into()
        .expect("failed to convert MQTT buffer slice into array");

    (rx_buffer, tx_buffer, mqtt_buffer)
}

pub(super) fn build_connection_packet(last_will: bool, message: &str) -> (Vec<u8>, usize) {
    let mut payload = vec![0u8; CONNECTION_PAYLOAD_SIZE];
    let payload_len = serde_json_core::to_slice(
        &ConnectionPacket {
            last_will,
            msg: String::from_str(message)
                .expect("failed to convert connection message to heapless string"),
        },
        &mut payload[..],
    )
    .expect("failed to serialize connection packet");
    (payload, payload_len)
}
