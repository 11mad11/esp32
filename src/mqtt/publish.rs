use alloc::vec::Vec;
use heapless::String;

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use esp_alloc::EspHeap;

pub const MQTT_PACKET_LEN: usize = 1024;

pub struct PublishPacket {
    pub topic: String<64>,
    pub buf: Vec<u8, &'static EspHeap>,
    pub len: usize,
}

static WRITE: Channel<CriticalSectionRawMutex, PublishPacket, 8> = Channel::new();

/// Enqueue a packet for MQTT publishing. Panics if topic or payload exceed limits.
pub async fn mqtt_send(buf: &[u8], topic: &str) {
    let topic = String::try_from(topic).expect("Topic too big");
    let len = buf.len();
    assert!(len < MQTT_PACKET_LEN, "Packet too big");
    let mut heap_buf = crate::vec_in_myheap!(0u8; len);
    heap_buf.copy_from_slice(&buf[..len]);
    WRITE
        .send(PublishPacket {
            topic,
            buf: heap_buf,
            len,
        })
        .await;
}

pub(super) async fn next_publish() -> PublishPacket {
    WRITE.receive().await
}
