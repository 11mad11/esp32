use core::cell::RefCell;

use critical_section::Mutex as CsMutex;
use heapless::{Deque, Vec};
use mountain_mqtt::client::{ClientError, Message};

use crate::{iot_topic, output::NUM_OUT};

use super::publish::MQTT_PACKET_LEN;

const INBOUND_CAPACITY: usize = 2;

#[derive(Debug, Clone)]
pub enum InboundAction {
    Outputs([Option<u8>; NUM_OUT]),
    Tcp(Vec<u8, MQTT_PACKET_LEN>),
    Echo(Vec<u8, MQTT_PACKET_LEN>),
}

static INBOUND_QUEUE: CsMutex<RefCell<Deque<InboundAction, INBOUND_CAPACITY>>> =
    CsMutex::new(RefCell::new(Deque::new()));

fn enqueue(action: InboundAction) {
    critical_section::with(|cs| {
        let mut queue = INBOUND_QUEUE.borrow_ref_mut(cs);
        if queue.push_back(action).is_err() {
            panic!("Inbound MQTT queue overflow");
        }
    });
}

pub(super) fn drain_one() -> Option<InboundAction> {
    critical_section::with(|cs| INBOUND_QUEUE.borrow_ref_mut(cs).pop_front())
}

pub(super) fn message_handler(msg: Message) -> Result<(), ClientError> {
    match msg.topic_name {
        concat!(iot_topic!(), "/ctrl") => {
            let ascii = core::str::from_utf8(msg.payload).expect("ctrl payload not ascii");
            defmt::info!("{}", ascii);
            let mut bytes = [None; NUM_OUT];
            for (i, chunk) in ascii.as_bytes().chunks(2).take(NUM_OUT).enumerate() {
                if chunk.len() != 2 {
                    defmt::error!("Incomplete hex pair at index {}", i);
                    continue;
                }
                match core::str::from_utf8(chunk)
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                {
                    Some(byte) => bytes[i] = Some(byte),
                    None => defmt::error!("Invalid hex pair at index {}", i),
                }
            }
            enqueue(InboundAction::Outputs(bytes));
        }
        concat!(iot_topic!(), "/rpc/tcp") => {
            enqueue(InboundAction::Tcp(copy_payload(msg.payload)));
        }
        concat!(iot_topic!(), "/echo") => {
            enqueue(InboundAction::Echo(copy_payload(msg.payload)));
        }
        _ => {}
    }
    Ok(())
}

fn copy_payload(payload: &[u8]) -> Vec<u8, MQTT_PACKET_LEN> {
    if payload.len() >= MQTT_PACKET_LEN {
        panic!("Inbound payload too large");
    }
    let mut data = Vec::new();
    data.extend_from_slice(payload)
        .expect("payload fits buffer");
    data
}
