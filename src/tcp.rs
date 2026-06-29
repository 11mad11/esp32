use alloc::{format, vec, vec::Vec};
use embassy_futures::select::{self, select};
use embassy_net::{Stack, tcp::TcpSocket};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Timer};

use crate::{
    iot_topic, led,
    mqtt::{MQTT_PACKET_LEN, mqtt_send},
    serial_frame::{self, SERIAL_TO_MQTT_MAX_FRAME},
};

pub static TCP_PACKET_LEN: usize = 64;
const SERIAL_TO_MQTT_PROTOCOL_ENABLED: bool = option_env!("SERIAL_TO_MQTT").is_some();

type HeapVec = Vec<u8>;

struct Packet {
    buf: HeapVec,
    len: usize,
}

static WRITE: Channel<CriticalSectionRawMutex, Packet, 2> = Channel::new();

pub async fn tcp_send(buf: &[u8]) {
    let len = buf.len();
    if len >= TCP_PACKET_LEN {
        panic!("Packet too big");
    }
    let mut heap_buf = vec![0u8; len];
    heap_buf.copy_from_slice(&buf[..len]);
    WRITE.send(Packet { buf: heap_buf, len }).await;
}

#[embassy_executor::task]
pub async fn tcp_task(stack: Stack<'static>) {
    let mut rx_buffer = vec![0u8; 1024];
    let mut tx_buffer = vec![0u8; 1024];

    loop {
        loop {
            let mut socket = {
                let mut socket = TcpSocket::new(stack, &mut rx_buffer[..], &mut tx_buffer[..]);
                socket.set_timeout(Some(Duration::from_secs(10)));
                if let Err(e) = socket.accept(10001).await {
                    defmt::info!("accept error: {:?}", defmt::Debug2Format(&e));
                    continue;
                }

                defmt::info!(
                    "accepted connection from {:?}",
                    defmt::Debug2Format(&socket.remote_endpoint())
                );
                led::state(led::LedState::Ok).await;
                socket
            };

            mqtt_send(
                format!("Accepted tcp connection: {:?}", socket.remote_endpoint()).as_bytes(),
                concat!(iot_topic!(), "/logs"),
            )
            .await;

            loop_s(&mut socket).await;

            socket.close();

            Timer::after_secs(1).await;
        }
    }
}

async fn loop_s<'a>(socket: &mut TcpSocket<'a>) {
    if SERIAL_TO_MQTT_PROTOCOL_ENABLED {
        serial_to_mqtt_loop(socket).await;
    } else {
        legacy_loop(socket).await;
    }
}

async fn legacy_loop<'a>(socket: &mut TcpSocket<'a>) {
    let mut accum = vec![0u8; MQTT_PACKET_LEN];
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
            select::Either::First(Err(e)) => {
                defmt::error!("Connection reset by peer: {:?}", defmt::Debug2Format(&e));
                mqtt_send(
                    b"Connection reset by peer",
                    concat!(iot_topic!(), "/logs"),
                )
                .await;
                break;
            }
            select::Either::First(Ok(Some(_))) => {
                mqtt_send(&accum[..index], concat!(iot_topic!(), "/data")).await;
                index = 0;
            }
            select::Either::First(Ok(None)) => (), //receive part of the packet, wait for the rest
            select::Either::Second(pk) => {
                socket.write(&pk.buf[..pk.len]).await.unwrap();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Serial-to-MQTT protocol loop.
//
// Incoming data is hex-encoded ASCII (e.g. the bytes 0x00 0x01 0xFF are sent
// as the ASCII string "0001FF").  There is no COBS framing; the frame length
// is obtained directly from the 16-bit `declared_len` field in the protocol
// header.
// ---------------------------------------------------------------------------

async fn serial_to_mqtt_loop<'a>(socket: &mut TcpSocket<'a>) {
    let mut decoder = HexFrameDecoder::new(vec![0u8; SERIAL_TO_MQTT_MAX_FRAME]);

    loop {
        let read_future = socket.read_with(|buf| decoder.feed(buf));

        match select(read_future, WRITE.receive()).await {
            select::Either::First(Err(e)) => {
                defmt::error!("Connection reset by peer: {:?}", defmt::Debug2Format(&e));
                mqtt_send(
                    b"Connection reset by peer",
                    concat!(iot_topic!(), "/logs"),
                )
                .await;
                break;
            }
            select::Either::First(Ok(Some(()))) => {
                let result = serial_frame::prepare_dispatch(decoder.frame_slice_mut());
                match result {
                    Ok((topic, payload)) => {
                        mqtt_send(payload, topic.as_str()).await;
                    }
                    Err(e) => {
                        defmt::warn!("serial-to-mqtt frame dropped: {:?}", e);
                    }
                }
                decoder.reset();
            }
            select::Either::First(Ok(None)) => (),
            select::Either::Second(pk) => {
                socket.write(&pk.buf[..pk.len]).await.unwrap();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Hex-stream frame decoder
//
// Decodes hex-encoded ASCII bytes on the fly into a binary buffer.  Once the
// protocol header has been decoded (3 bytes) the total frame size is known
// from `declared_len`, and the decoder signals completion as soon as exactly
// that many bytes have been received.
// ---------------------------------------------------------------------------

struct HexFrameDecoder {
    /// Binary-decoded receive buffer.
    bin_buf: Vec<u8>,
    /// Number of valid binary bytes currently in `bin_buf`.
    bin_len: usize,
    /// Total expected frame size in bytes (known after the first 3 bytes).
    expected_frame_len: Option<usize>,
    /// First nibble of a partially decoded hex byte pair.
    pending_nibble: Option<u8>,
}

impl HexFrameDecoder {
    fn new(bin_buf: Vec<u8>) -> Self {
        Self {
            bin_buf,
            bin_len: 0,
            expected_frame_len: None,
            pending_nibble: None,
        }
    }

    /// Feed a chunk of raw TCP data (hex ASCII characters) into the decoder.
    ///
    /// Returns `(bytes_consumed, Some(()))` when a complete frame is ready,
    /// or `(bytes_consumed, None)` while still accumulating data.
    fn feed(&mut self, data: &mut [u8]) -> (usize, Option<()>) {
        let mut idx = 0;

        while idx < data.len() {
            let byte = data[idx];
            idx += 1;

            let nibble = match hex_nibble(byte) {
                Some(n) => n,
                None => continue, // skip whitespace / non-hex chars
            };

            if let Some(hi) = self.pending_nibble.take() {
                // Complete a byte from the two nibbles.
                if self.bin_len < self.bin_buf.len() {
                    self.bin_buf[self.bin_len] = (hi << 4) | nibble;
                    self.bin_len += 1;
                }

                // After 3 binary bytes we can read `declared_len` from the header.
                if self.expected_frame_len.is_none() && self.bin_len >= 3 {
                    let declared =
                        u16::from_le_bytes([self.bin_buf[1], self.bin_buf[2]]) as usize;
                    self.expected_frame_len = Some(declared + 1);
                }

                // Check for completion.
                if let Some(expected) = self.expected_frame_len {
                    if self.bin_len >= expected {
                        return (idx, Some(()));
                    }
                }
            } else {
                self.pending_nibble = Some(nibble);
            }
        }

        (idx, None)
    }

    /// Return a mutable slice over the completed frame.
    ///
    /// Only valid after `feed` has returned `Some(())`.
    fn frame_slice_mut(&mut self) -> &mut [u8] {
        let len = self.expected_frame_len.unwrap_or(self.bin_len);
        &mut self.bin_buf[..len]
    }

    /// Consume the current frame and prepare the decoder for the next one.
    ///
    /// Any binary bytes decoded beyond the frame boundary are shifted to the
    /// front of the buffer.
    fn reset(&mut self) {
        let frame_len = self.expected_frame_len.unwrap_or(self.bin_len);
        if frame_len < self.bin_len {
            self.bin_buf.copy_within(frame_len..self.bin_len, 0);
        }
        self.bin_len -= frame_len;
        self.expected_frame_len = None;
        // pending_nibble is always None after a successful frame decode.
    }
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}


