use core::str;

use alloc::vec::Vec;
use defmt::Format;
use heapless::String as HeapString;

use crate::{iot_topic, mqtt::MQTT_PACKET_LEN};

pub const SERIAL_TO_MQTT_MAX_FRAME: usize = 10_240;
pub const SERIAL_TO_MQTT_MAX_BODY: usize = SERIAL_TO_MQTT_MAX_FRAME - 2;

pub type TopicString = HeapString<64>;

#[derive(Debug)]
pub struct SerialParsedFrame<'a> {
    #[allow(dead_code)]
    pub version: u8,
    pub msg_id: u32,
    pub channel: &'a str,
    pub ctype: &'a str,
    pub payload: &'a [u8],
}

#[derive(Clone, Copy, Debug, Format)]
pub enum SerialFrameError {
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

/// Parse a raw binary serial frame (after any COBS decoding has already been applied).
pub fn parse_serial_frame(buf: &mut [u8]) -> Result<SerialParsedFrame<'_>, SerialFrameError> {
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

/// Build an MQTT topic string for the given channel name.
pub fn build_topic(channel: &str) -> Result<TopicString, SerialFrameError> {
    let mut topic = TopicString::new();
    topic
        .push_str(concat!(iot_topic!(), "/data/"))
        .map_err(|_| SerialFrameError::TopicTooLong)?;
    topic
        .push_str(channel)
        .map_err(|_| SerialFrameError::TopicTooLong)?;
    Ok(topic)
}

/// Dispatch a raw binary frame: parse it and return `(topic, payload)`.
/// The caller is responsible for any COBS decoding before calling this.
pub fn prepare_dispatch<'a>(
    buf: &'a mut [u8],
) -> Result<(TopicString, &'a [u8]), SerialFrameError> {
    if buf.is_empty() {
        return Err(SerialFrameError::EmptyFrame);
    }
    let parsed = parse_serial_frame(buf)?;
    defmt::debug!(
        "serial-frame msg={} chan={} ctype={} len={}",
        parsed.msg_id,
        parsed.channel,
        parsed.ctype,
        parsed.payload.len()
    );
    let topic = build_topic(parsed.channel)?;
    Ok((topic, parsed.payload))
}

/// Dispatch a COBS-encoded frame: COBS-decode it in place, then parse and
/// return `(topic, payload)`.
pub fn prepare_dispatch_cobs<'a>(
    buf: &'a mut [u8],
) -> Result<(TopicString, &'a [u8]), SerialFrameError> {
    if buf.is_empty() {
        return Err(SerialFrameError::EmptyFrame);
    }
    let decoded_len = cobs_decode_in_place(buf)?;
    if decoded_len == 0 {
        return Err(SerialFrameError::EmptyFrame);
    }
    prepare_dispatch(&mut buf[..decoded_len])
}

/// COBS decode `buf` in place, returning the number of decoded bytes.
pub fn cobs_decode_in_place(buf: &mut [u8]) -> Result<usize, SerialFrameError> {
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

pub fn crc32_mpeg2(data: &[u8]) -> u32 {
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

// ---------------------------------------------------------------------------
// COBS frame assembler – used by transports that delimit frames with 0x00
// bytes (e.g. UART RS-485).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Format)]
pub enum SerialFrameEvent {
    Ready,
    Overflow,
}

pub struct SerialFrameAssembler {
    buf: Vec<u8>,
    pub len: usize,
    collecting: bool,
}

impl SerialFrameAssembler {
    pub fn new(buf: Vec<u8>) -> Self {
        Self {
            buf,
            len: 0,
            collecting: false,
        }
    }

    /// Feed raw bytes from the transport into the assembler.
    ///
    /// Returns `(bytes_consumed, Option<event>)`.  When `Some(Ready)` is
    /// returned the caller should read the frame via [`frame_slice_mut`] and
    /// then call [`reset`].
    pub fn feed(&mut self, buf: &mut [u8]) -> (usize, Option<SerialFrameEvent>) {
        defmt::info!(
            "serial-frame assembler: received {} bytes: {:?}",
            buf.len(),
            defmt::Debug2Format(buf)
        );
        let mut idx = 0;

        while idx < buf.len() {
            let byte = buf[idx];
            idx += 1;

            if !self.collecting {
                if byte == 0 {
                    defmt::info!("serial-frame: start of frame detected");
                    self.collecting = true;
                    self.len = 0;
                }
                continue;
            }

            if byte == 0 {
                if self.len == 0 {
                    defmt::info!("serial-frame: empty frame delimiter");
                    continue;
                }
                defmt::info!("serial-frame: end of frame, len={}", self.len);
                return (idx, Some(SerialFrameEvent::Ready));
            }

            if self.len >= self.buf.len() {
                defmt::warn!("serial-frame: frame overflow, len={}", self.len);
                self.reset();
                return (idx, Some(SerialFrameEvent::Overflow));
            }

            self.buf[self.len] = byte;
            self.len += 1;
        }

        (buf.len(), None)
    }

    pub fn frame_slice_mut(&mut self) -> &mut [u8] {
        &mut self.buf[..self.len]
    }

    pub fn reset(&mut self) {
        self.len = 0;
        self.collecting = false;
    }
}

#[cfg(test)]
mod tests {
    use super::crc32_mpeg2;

    #[test]
    fn crc32_mpeg2_matches_reference() {
        assert_eq!(crc32_mpeg2(b"123456789"), 0x0376E6E7);
    }
}
