use core::convert::TryInto;

use embassy_net::{tcp::TcpSocket, IpEndpoint, Stack};
use embassy_time::Timer;
use mountain_mqtt::client::{Client, ClientNoQueue, Delay};

use crate::{iot_topic, led};

use super::inbound::{InboundEventHandler, MAX_APPLICATION_PROPERTIES};

pub const RX_BUFFER_SIZE: usize = 4096;
pub const TX_BUFFER_SIZE: usize = 4096;
pub const MQTT_BUFFER_SIZE: usize = 4096;
pub const CLIENT_TIMEOUT_MS: u32 = 5000;
const DNS_HOST: &str = "ssca.desrochers.space";
const MQTT_PORT: u16 = 1883;

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
    let result_connection = socket
        .connect(IpEndpoint::new(
            addr.unwrap().first().unwrap().clone(),
            MQTT_PORT,
        ))
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
    let rx_buffer_vec = crate::vec_in_myheap!(0u8; RX_BUFFER_SIZE);
    let rx_buffer: &'static mut [u8; RX_BUFFER_SIZE] = rx_buffer_vec
        .leak()
        .try_into()
        .expect("failed to convert RX buffer slice into array");

    let tx_buffer_vec = crate::vec_in_myheap!(0u8; TX_BUFFER_SIZE);
    let tx_buffer: &'static mut [u8; TX_BUFFER_SIZE] = tx_buffer_vec
        .leak()
        .try_into()
        .expect("failed to convert TX buffer slice into array");

    let mqtt_buffer_vec = crate::vec_in_myheap!(0u8; MQTT_BUFFER_SIZE);
    let mqtt_buffer: &'static mut [u8; MQTT_BUFFER_SIZE] = mqtt_buffer_vec
        .leak()
        .try_into()
        .expect("failed to convert MQTT buffer slice into array");

    (rx_buffer, tx_buffer, mqtt_buffer)
}
