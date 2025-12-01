use core::{convert::TryInto, str};

use alloc::format;
use defmt::Format;
use embassy_futures::select::{self, select};
use embassy_net::{Stack, tcp::TcpSocket};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Timer};
use heapless::String as HeapString;

use crate::{
    MyHeapVec, iot_topic, led,
    mqtt::{MQTT_PACKET_LEN, mqtt_send},
};

pub static TCP_PACKET_LEN: usize = 64;
const SERIAL_TO_MQTT_PROTOCOL_ENABLED: bool = option_env!("SERIAL_TO_MQTT").is_some();
const SERIAL_TO_MQTT_MAX_FRAME: usize = 65_535;
const SERIAL_TO_MQTT_MAX_BODY: usize = SERIAL_TO_MQTT_MAX_FRAME - 2;

type HeapVec = MyHeapVec<u8>;
type TopicString = HeapString<64>;

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
    let mut heap_buf = crate::vec_in_myheap!(0u8; len);
    heap_buf.copy_from_slice(&buf[..len]);
    WRITE.send(Packet { buf: heap_buf, len }).await;
}

#[embassy_executor::task]
pub async fn tcp_task(stack: Stack<'static>) {
    let mut rx_buffer = crate::vec_in_myheap!(0u8; 1024);
    let mut tx_buffer = crate::vec_in_myheap!(0u8; 1024);

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
    let mut accum = crate::vec_in_myheap!(0u8; MQTT_PACKET_LEN);
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

async fn serial_to_mqtt_loop<'a>(socket: &mut TcpSocket<'a>) {
    let mut assembler =
        SerialFrameAssembler::new(crate::vec_in_myheap!(0u8; SERIAL_TO_MQTT_MAX_BODY));

    loop {
        let read_future = socket.read_with(|buf| assembler.feed(buf));

        match select(read_future, WRITE.receive()).await {
            select::Either::First(Err(_)) => break,
            select::Either::First(Ok(Some(SerialFrameEvent::Ready))) => {
                let (topic, payload) = match prepare_dispatch(assembler.frame_slice_mut()) {
                    Ok(result) => result,
                    Err(err) => {
                        defmt::warn!("serial-to-mqtt frame dropped: {:?}", err);
                        assembler.reset();
                        continue;
                    }
                };

                mqtt_send(payload, topic.as_str()).await;
                assembler.reset();
            }
            select::Either::First(Ok(Some(SerialFrameEvent::Overflow))) => {
                defmt::warn!(
                    "serial-to-mqtt frame overflow (>{} bytes), waiting for next frame",
                    SERIAL_TO_MQTT_MAX_BODY
                );
                assembler.reset();
            }
            select::Either::First(Ok(None)) => (),
            select::Either::Second(pk) => {
                socket.write(&pk.buf[..pk.len]).await.unwrap();
            }
        }
    }
}

fn prepare_dispatch<'a>(buf: &'a mut [u8]) -> Result<(TopicString, &'a [u8]), SerialFrameError> {
    if buf.is_empty() {
        return Err(SerialFrameError::EmptyFrame);
    }

    let decoded_len = cobs_decode_in_place(buf)?;
    if decoded_len == 0 {
        return Err(SerialFrameError::EmptyFrame);
    }

    let parsed = parse_serial_frame(&mut buf[..decoded_len])?;

    defmt::debug!(
        "serial-to-mqtt msg={} chan={} ctype={} len={}",
        parsed.msg_id,
        parsed.channel,
        parsed.ctype,
        parsed.payload.len()
    );

    let mut topic = TopicString::new();
    topic
        .push_str(concat!(iot_topic!(), "/data/"))
        .map_err(|_| SerialFrameError::TopicTooLong)?;
    topic
        .push_str(parsed.channel)
        .map_err(|_| SerialFrameError::TopicTooLong)?;

    Ok((topic, parsed.payload))
}

#[derive(Clone, Copy, Debug, Format)]
enum SerialFrameEvent {
    Ready,
    Overflow,
}

struct SerialFrameAssembler {
    buf: HeapVec,
    len: usize,
    collecting: bool,
}

impl SerialFrameAssembler {
    fn new(buf: HeapVec) -> Self {
        Self {
            buf,
            len: 0,
            collecting: false,
        }
    }

    fn feed(&mut self, buf: &mut [u8]) -> (usize, Option<SerialFrameEvent>) {
        let mut idx = 0;

        while idx < buf.len() {
            let byte = buf[idx];
            idx += 1;

            if !self.collecting {
                if byte == 0 {
                    self.collecting = true;
                    self.len = 0;
                }
                continue;
            }

            if byte == 0 {
                if self.len == 0 {
                    continue;
                }
                return (idx, Some(SerialFrameEvent::Ready));
            }

            if self.len >= self.buf.len() {
                self.reset();
                return (idx, Some(SerialFrameEvent::Overflow));
            }

            self.buf[self.len] = byte;
            self.len += 1;
        }

        (buf.len(), None)
    }

    fn frame_slice_mut(&mut self) -> &mut [u8] {
        &mut self.buf[..self.len]
    }

    fn reset(&mut self) {
        self.len = 0;
        self.collecting = false;
    }
}

#[derive(Debug)]
struct SerialParsedFrame<'a> {
    #[allow(dead_code)]
    version: u8,
    msg_id: u32,
    channel: &'a str,
    ctype: &'a str,
    payload: &'a [u8],
}

#[derive(Clone, Copy, Debug, Format)]
enum SerialFrameError {
    EmptyFrame,
    CobsZeroByte,
    CobsUnexpectedEof,
    LengthMismatch { declared: usize, actual: usize },
    UnsupportedVersion(u8),
    ChannelTooLong,
    CTypeTooLong,
    InvalidChannelUtf8,
    InvalidCTypeUtf8,
    PayloadTooLarge { len: usize },
    PayloadLengthMismatch,
    TopicTooLong,
    CrcMismatch { expected: u32, actual: u32 },
}

fn parse_serial_frame(buf: &mut [u8]) -> Result<SerialParsedFrame<'_>, SerialFrameError> {
    if buf.len() < 1 + 2 + 4 + 1 + 1 + 2 + 4 {
        return Err(SerialFrameError::LengthMismatch {
            declared: 0,
            actual: buf.len(),
        });
    }

    let version = buf[0];
    if version != 1 {
        return Err(SerialFrameError::UnsupportedVersion(version));
    }

    let declared_len = u16::from_le_bytes([buf[1], buf[2]]) as usize;
    if declared_len != buf.len() - 1 {
        return Err(SerialFrameError::LengthMismatch {
            declared: declared_len,
            actual: buf.len() - 1,
        });
    }

    let mut offset = 3;

    let msg_id = u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap());
    offset += 4;

    let chan_len = buf[offset] as usize;
    offset += 1;
    if offset + chan_len > buf.len() {
        return Err(SerialFrameError::ChannelTooLong);
    }
    let channel = str::from_utf8(&buf[offset..offset + chan_len])
        .map_err(|_| SerialFrameError::InvalidChannelUtf8)?;
    offset += chan_len;

    let ctype_len = buf[offset] as usize;
    offset += 1;
    if offset + ctype_len > buf.len() {
        return Err(SerialFrameError::CTypeTooLong);
    }
    let ctype = str::from_utf8(&buf[offset..offset + ctype_len])
        .map_err(|_| SerialFrameError::InvalidCTypeUtf8)?;
    offset += ctype_len;

    if offset + 2 > buf.len() {
        return Err(SerialFrameError::PayloadLengthMismatch);
    }
    let payload_len = u16::from_le_bytes(buf[offset..offset + 2].try_into().unwrap()) as usize;
    offset += 2;

    if offset + payload_len + 4 > buf.len() {
        return Err(SerialFrameError::PayloadLengthMismatch);
    }
    let payload = &buf[offset..offset + payload_len];
    offset += payload_len;

    if offset + 4 > buf.len() {
        return Err(SerialFrameError::LengthMismatch {
            declared: declared_len,
            actual: buf.len() - 1,
        });
    }
    let crc_expected = u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap());
    if offset + 4 != buf.len() {
        return Err(SerialFrameError::LengthMismatch {
            declared: declared_len,
            actual: buf.len() - 1,
        });
    }

    if payload_len > MQTT_PACKET_LEN {
        return Err(SerialFrameError::PayloadTooLarge { len: payload_len });
    }

    let crc_actual = crc32_mpeg2(&buf[..offset]);
    if crc_actual != crc_expected {
        return Err(SerialFrameError::CrcMismatch {
            expected: crc_expected,
            actual: crc_actual,
        });
    }

    Ok(SerialParsedFrame {
        version,
        msg_id,
        channel,
        ctype,
        payload,
    })
}

fn cobs_decode_in_place(buf: &mut [u8]) -> Result<usize, SerialFrameError> {
    let mut read_index = 0;
    let mut write_index = 0;

    while read_index < buf.len() {
        let code = buf[read_index];
        if code == 0 {
            return Err(SerialFrameError::CobsZeroByte);
        }
        read_index += 1;

        let end = read_index + (code as usize - 1);
        while read_index < end {
            if read_index >= buf.len() {
                return Err(SerialFrameError::CobsUnexpectedEof);
            }
            buf[write_index] = buf[read_index];
            write_index += 1;
            read_index += 1;
        }

        if code != 0xFF && read_index < buf.len() {
            buf[write_index] = 0;
            write_index += 1;
        }
    }

    Ok(write_index)
}

fn crc32_mpeg2(data: &[u8]) -> u32 {
    const POLY: u32 = 0x04C11DB7;
    let mut crc: u32 = 0xFFFF_FFFF;

    for &byte in data {
        crc ^= (byte as u32) << 24;
        for _ in 0..8 {
            if (crc & 0x8000_0000) != 0 {
                crc = (crc << 1) ^ POLY;
            } else {
                crc <<= 1;
            }
        }
    }

    crc
}

#[cfg(test)]
mod tests {
    use super::crc32_mpeg2;

    #[test]
    fn crc32_mpeg2_matches_reference() {
        assert_eq!(crc32_mpeg2(b"123456789"), 0x0376E6E7);
    }
}
