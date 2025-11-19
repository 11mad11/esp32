use core::str::FromStr;

use embassy_futures::select::select3;
use embassy_net::Stack;
use embassy_time::Timer;
use heapless::Vec;
use mountain_mqtt::{
    client::Client,
    data::quality_of_service::QualityOfService,
    packets::connect::{Connect, Will},
};
use serde::Serialize;

use crate::{iot_topic, led, output, tcp};

use super::{
    connection::{alloc_buffers, setup_client, setup_subscriptions},
    inbound::{drain_one, InboundAction},
    publish::{mqtt_send, next_publish as next_publish_packet},
};

const CONNECTION_PAYLOAD_SIZE: usize = 256;
const PING_INTERVAL_SECS: u64 = 5;

#[derive(Debug, Serialize)]
struct ConnectionPacket {
    msg: heapless::String<64>,
    last_will: bool,
}

#[embassy_executor::task]
pub async fn mqtt_task(stack: Stack<'static>) -> ! {
    let (rx_buffer, tx_buffer, mqtt_buffer) = alloc_buffers();

    'main: loop {
        let mut client = match setup_client(
            stack,
            &mut rx_buffer[..],
            &mut tx_buffer[..],
            &mut mqtt_buffer[..],
        )
        .await
        {
            Some(c) => c,
            None => continue 'main,
        };

        let mut will_payload = crate::vec_in_myheap!(0u8; CONNECTION_PAYLOAD_SIZE);
        let will_payload_len = serde_json_core::to_slice(
            &ConnectionPacket {
                last_will: true,
                msg: heapless::String::from_str("me dead").unwrap(),
            },
            &mut will_payload[..],
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
                    qos: QualityOfService::QoS0,
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
            led::state(led::LedState::MQTT(false)).await;
            continue 'main;
        }

        let mut payload = crate::vec_in_myheap!(0u8; CONNECTION_PAYLOAD_SIZE);
        let payload_len = serde_json_core::to_slice(
            &ConnectionPacket {
                last_will: false,
                msg: heapless::String::from_str("me alive").unwrap(),
            },
            &mut payload[..],
        )
        .unwrap();
        client
            .publish(
                concat!(iot_topic!(), "/connection"),
                &payload[..payload_len],
                QualityOfService::QoS0,
                false,
            )
            .await
            .ok();

        setup_subscriptions(&mut client).await;
        process_inbound_actions().await;

        led::state(led::LedState::MQTT(true)).await;
        client
            .publish(
                concat!(iot_topic!(), "/logs"),
                b"Connected!",
                QualityOfService::QoS0,
                false,
            )
            .await
            .ok();

        loop {
            match select3(
                next_publish_packet(),
                client.poll(true),
                Timer::after_secs(PING_INTERVAL_SECS),
            )
            .await
            {
                embassy_futures::select::Either3::First(packet) => {
                    if let Ok(ascii) = core::str::from_utf8(&packet.buf[..packet.len]) {
                        defmt::debug!("{}", ascii);
                    }
                    let r = client
                        .publish(
                            &packet.topic,
                            &packet.buf[..packet.len],
                            QualityOfService::QoS0,
                            false,
                        )
                        .await;
                    if let Err(e) = r {
                        defmt::error!("publish mqtt {:?}", defmt::Debug2Format(&e));
                        Timer::after_millis(500).await;
                        led::state(led::LedState::MQTT(false)).await;
                        continue 'main;
                    }
                }
                embassy_futures::select::Either3::Second(Ok(true)) => {}
                embassy_futures::select::Either3::Second(Ok(false)) => {
                    Timer::after_millis(10).await;
                }
                embassy_futures::select::Either3::Second(Err(e)) => {
                    defmt::error!("poll mqtt {:?}", defmt::Debug2Format(&e));
                    led::state(led::LedState::MQTT(false)).await;
                    Timer::after_millis(500).await;
                    continue 'main;
                }
                embassy_futures::select::Either3::Third(_) => {
                    if let Err(e) = client.send_ping().await {
                        defmt::error!("{:?}", defmt::Debug2Format(&e));
                        led::state(led::LedState::RPCError).await;
                    }
                }
            };

            process_inbound_actions().await;
        }
    }
}

async fn process_inbound_actions() {
    while let Some(action) = drain_one() {
        match action {
            InboundAction::Outputs(bytes) => {
                output::output_state(bytes).await;
            }
            InboundAction::Tcp(payload) => {
                tcp::tcp_send(&payload).await;
            }
            InboundAction::Echo(payload) => {
                mqtt_send(&payload, "/echo").await;
            }
        }
    }
}
