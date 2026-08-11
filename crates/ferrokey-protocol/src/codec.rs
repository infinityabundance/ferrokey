//! Frame encoding/decoding for the Ferrokey protocol.
//!
//! Wire layout:
//!
//! ```text
//! ┌──────────┬─────────────┬──────────────┐
//! │ magic    │ payload len │ payload      │
//! │ "FK01"   │ u16 LE      │ (opcode + …) │
//! └──────────┴─────────────┴──────────────┘
//! ```
//!
//! The [`Decoder`] is streaming and hostile-input resistant: it accepts
//! arbitrarily fragmented writes (including byte-at-a-time delivery), never
//! allocates more than [`MAX_FRAME_LEN`], and treats any violation of the
//! framing (bad magic, impossible lengths, unknown opcodes, malformed
//! payloads) as a fatal connection error.

use crate::message::{ErrorCode, Message, Opcode, MAGIC, MAX_FRAME_LEN, MAX_STRING_LEN};
use std::io;

/// Errors produced by the codec.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("malformed frame: {0}")]
    Malformed(String),
    #[error("frame payload length {len} exceeds maximum of {MAX_FRAME_LEN}")]
    TooLarge { len: usize },
    #[error("string of length {len} exceeds maximum of {MAX_STRING_LEN}")]
    StringTooLong { len: usize },
    #[error("I/O error: {0}")]
    Io(String),
}

impl From<io::Error> for CodecError {
    fn from(e: io::Error) -> Self {
        CodecError::Io(e.to_string())
    }
}

const HEADER_LEN: usize = 4 + 2;

/// Encode a message into a complete frame (magic + length + payload).
pub fn encode(msg: &Message) -> Result<Vec<u8>, CodecError> {
    let payload = encode_payload(msg)?;
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.extend_from_slice(MAGIC);
    frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

fn encode_payload(msg: &Message) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::new();
    match msg {
        Message::Hello {
            version,
            client_name,
        } => {
            out.push(Opcode::Hello as u8);
            out.push(*version);
            push_string(&mut out, client_name)?;
        }
        Message::CreateKeyboard => {
            out.push(Opcode::CreateKeyboard as u8);
        }
        Message::KeyDown(code) => {
            out.push(Opcode::KeyDown as u8);
            out.extend_from_slice(&code.to_le_bytes());
        }
        Message::KeyUp(code) => {
            out.push(Opcode::KeyUp as u8);
            out.extend_from_slice(&code.to_le_bytes());
        }
        Message::ReleaseAll => {
            out.push(Opcode::ReleaseAll as u8);
        }
        Message::Ping(nonce) => {
            out.push(Opcode::Ping as u8);
            out.extend_from_slice(&nonce.to_le_bytes());
        }
        Message::Pong(nonce) => {
            out.push(Opcode::Pong as u8);
            out.extend_from_slice(&nonce.to_le_bytes());
        }
        Message::Ok => {
            out.push(Opcode::Ok as u8);
        }
        Message::Error(code, msg) => {
            out.push(Opcode::Error as u8);
            out.extend_from_slice(&code.code().to_le_bytes());
            push_string(&mut out, msg)?;
        }
    }
    Ok(out)
}

fn push_string(out: &mut Vec<u8>, s: &str) -> Result<(), CodecError> {
    let bytes = s.as_bytes();
    if bytes.len() > MAX_STRING_LEN {
        return Err(CodecError::StringTooLong { len: bytes.len() });
    }
    out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

/// Streaming decoder.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Self {
        Decoder { buf: Vec::new() }
    }

    /// Feed more bytes. Returns all complete messages that could be parsed,
    /// or a fatal error if the stream is malformed.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Message>, CodecError> {
        self.buf.extend_from_slice(bytes);
        let mut messages = Vec::new();
        while let Some(frame_len) = self.frame_len()? {
            let end = HEADER_LEN + frame_len;
            let payload = self.buf[HEADER_LEN..end].to_vec();
            self.buf.drain(..end);
            messages.push(decode_payload(&payload)?);
        }
        Ok(messages)
    }

    /// The number of bytes currently buffered (diagnostics).
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// If a complete frame is buffered, its payload length; otherwise `None`.
    fn frame_len(&self) -> Result<Option<usize>, CodecError> {
        if self.buf.len() < HEADER_LEN {
            return Ok(None);
        }
        if &self.buf[..4] != MAGIC {
            return Err(CodecError::Malformed(format!(
                "bad magic {:02x?}",
                &self.buf[..4]
            )));
        }
        let len = u16::from_le_bytes([self.buf[4], self.buf[5]]) as usize;
        if len > MAX_FRAME_LEN {
            return Err(CodecError::TooLarge { len });
        }
        if self.buf.len() < HEADER_LEN + len {
            return Ok(None); // partial frame; wait for more
        }
        Ok(Some(len))
    }
}

fn take_string(payload: &[u8], pos: &mut usize) -> Result<String, CodecError> {
    let remaining = payload.len() - *pos;
    if remaining < 2 {
        return Err(CodecError::Malformed("truncated string length".into()));
    }
    let len = u16::from_le_bytes([payload[*pos], payload[*pos + 1]]) as usize;
    *pos += 2;
    if len > MAX_STRING_LEN {
        return Err(CodecError::StringTooLong { len });
    }
    let end = *pos + len;
    if end > payload.len() {
        return Err(CodecError::Malformed("truncated string body".into()));
    }
    let s = std::str::from_utf8(&payload[*pos..end])
        .map_err(|_| CodecError::Malformed("string is not valid UTF-8".into()))?
        .to_string();
    *pos = end;
    Ok(s)
}

fn decode_payload(payload: &[u8]) -> Result<Message, CodecError> {
    if payload.is_empty() {
        return Err(CodecError::Malformed("empty payload".into()));
    }
    let op = Opcode::from_u8(payload[0])
        .ok_or_else(|| CodecError::Malformed(format!("unknown opcode 0x{:02x}", payload[0])))?;
    let mut pos = 1;
    match op {
        Opcode::Hello => {
            if payload.len() < 2 {
                return Err(CodecError::Malformed("truncated hello".into()));
            }
            let version = payload[1];
            pos = 2;
            let client_name = take_string(payload, &mut pos)?;
            if pos != payload.len() {
                return Err(CodecError::Malformed("trailing bytes after hello".into()));
            }
            Ok(Message::Hello {
                version,
                client_name,
            })
        }
        Opcode::CreateKeyboard => {
            if pos != payload.len() {
                return Err(CodecError::Malformed(
                    "trailing bytes after create-keyboard".into(),
                ));
            }
            Ok(Message::CreateKeyboard)
        }
        Opcode::KeyDown | Opcode::KeyUp => {
            if payload.len() != 3 {
                return Err(CodecError::Malformed(format!(
                    "key message with payload length {} (expected 3)",
                    payload.len()
                )));
            }
            let code = u16::from_le_bytes([payload[1], payload[2]]);
            Ok(if op == Opcode::KeyDown {
                Message::KeyDown(code)
            } else {
                Message::KeyUp(code)
            })
        }
        Opcode::ReleaseAll => {
            if pos != payload.len() {
                return Err(CodecError::Malformed(
                    "trailing bytes after release-all".into(),
                ));
            }
            Ok(Message::ReleaseAll)
        }
        Opcode::Ping | Opcode::Pong => {
            if payload.len() != 5 {
                return Err(CodecError::Malformed(format!(
                    "ping/pong with payload length {} (expected 5)",
                    payload.len()
                )));
            }
            let nonce = u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]);
            Ok(if op == Opcode::Ping {
                Message::Ping(nonce)
            } else {
                Message::Pong(nonce)
            })
        }
        Opcode::Ok => {
            if pos != payload.len() {
                return Err(CodecError::Malformed("trailing bytes after ok".into()));
            }
            Ok(Message::Ok)
        }
        Opcode::Error => {
            if payload.len() < 3 {
                return Err(CodecError::Malformed("truncated error".into()));
            }
            let code = ErrorCode::from_u16(u16::from_le_bytes([payload[1], payload[2]]))
                .ok_or_else(|| {
                    CodecError::Malformed(format!(
                        "unknown error code 0x{:04x}",
                        u16::from_le_bytes([payload[1], payload[2]])
                    ))
                })?;
            pos = 3;
            let msg = take_string(payload, &mut pos)?;
            if pos != payload.len() {
                return Err(CodecError::Malformed("trailing bytes after error".into()));
            }
            Ok(Message::Error(code, msg))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(msg: &Message) {
        let frame = encode(msg).unwrap();
        let mut dec = Decoder::new();
        let got = dec.push(&frame).unwrap();
        assert_eq!(got, vec![msg.clone()]);
        assert!(dec.buffered() == 0);
    }

    #[test]
    fn round_trips_all_messages() {
        round_trip(&Message::Hello {
            version: 1,
            client_name: "ferrokey-ui".into(),
        });
        round_trip(&Message::CreateKeyboard);
        round_trip(&Message::KeyDown(30));
        round_trip(&Message::KeyUp(42));
        round_trip(&Message::ReleaseAll);
        round_trip(&Message::Ping(0xDEAD_BEEF));
        round_trip(&Message::Pong(7));
        round_trip(&Message::Ok);
        round_trip(&Message::Error(ErrorCode::Unauthorized, "nope".into()));
        round_trip(&Message::Hello {
            version: 1,
            client_name: String::new(),
        });
    }

    #[test]
    fn byte_at_a_time_delivery() {
        let frame = encode(&Message::KeyDown(30)).unwrap();
        let mut dec = Decoder::new();
        let mut all = Vec::new();
        for b in frame {
            all.extend(dec.push(&[b]).unwrap());
        }
        assert_eq!(all, vec![Message::KeyDown(30)]);
    }

    #[test]
    fn two_frames_in_one_push() {
        let f1 = encode(&Message::KeyDown(30)).unwrap();
        let f2 = encode(&Message::KeyUp(30)).unwrap();
        let mut joined = f1;
        joined.extend_from_slice(&f2);
        let mut dec = Decoder::new();
        assert_eq!(
            dec.push(&joined).unwrap(),
            vec![Message::KeyDown(30), Message::KeyUp(30)]
        );
    }

    #[test]
    fn bad_magic_is_fatal() {
        let mut dec = Decoder::new();
        let err = dec.push(b"XXXX\x05\x00hello").unwrap_err();
        assert!(matches!(err, CodecError::Malformed(_)));
    }

    #[test]
    fn oversized_length_is_rejected() {
        let mut frame = vec![];
        frame.extend_from_slice(MAGIC);
        frame.extend_from_slice(&0xFFFFu16.to_le_bytes());
        let mut dec = Decoder::new();
        assert!(matches!(dec.push(&frame), Err(CodecError::TooLarge { .. })));
    }

    #[test]
    fn unknown_opcode_is_fatal() {
        let mut dec = Decoder::new();
        let err = dec
            .push(&[b'F', b'K', b'0', b'1', 0x01, 0x00, 0x7F])
            .unwrap_err();
        assert!(matches!(err, CodecError::Malformed(_)));
    }

    #[test]
    fn truncated_payload_waits() {
        let frame = encode(&Message::KeyDown(30)).unwrap();
        let mut dec = Decoder::new();
        // Only the header.
        assert_eq!(
            dec.push(&frame[..HEADER_LEN]).unwrap(),
            Vec::<Message>::new()
        );
        assert_eq!(dec.buffered(), HEADER_LEN);
    }

    #[test]
    fn truncated_string_is_malformed() {
        let mut dec = Decoder::new();
        // Hello with a string length of 10 but no body.
        let payload = vec![0x01, 0x01, 0x0A, 0x00];
        let mut frame = MAGIC.to_vec();
        frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        frame.extend_from_slice(&payload);
        let err = dec.push(&frame).unwrap_err();
        assert!(matches!(err, CodecError::Malformed(_)));
    }

    #[test]
    fn key_message_with_wrong_length_is_malformed() {
        let mut dec = Decoder::new();
        let payload = vec![0x10, 0x2A]; // KeyDown with 1 byte of code
        let mut frame = MAGIC.to_vec();
        frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        frame.extend_from_slice(&payload);
        assert!(matches!(dec.push(&frame), Err(CodecError::Malformed(_))));
    }

    #[test]
    fn oversized_string_is_rejected() {
        let long = "x".repeat(MAX_STRING_LEN + 1);
        assert!(matches!(
            encode(&Message::Hello {
                version: 1,
                client_name: long
            }),
            Err(CodecError::StringTooLong { .. })
        ));
    }

    #[test]
    fn garbage_after_good_frame_is_fatal_on_next_push() {
        let f1 = encode(&Message::Ok).unwrap();
        let mut garbage = f1;
        garbage.extend_from_slice(b"JUNK");
        let mut dec = Decoder::new();
        // The good message is delivered; the trailing junk poisons the stream
        // and becomes a fatal error once the bogus header is complete.
        let msgs = dec.push(&garbage).unwrap();
        assert_eq!(msgs, vec![Message::Ok]);
        // Two more bytes complete the bogus 4-byte "JUNK" header.
        assert!(matches!(dec.push(b"ab"), Err(CodecError::Malformed(_))));
    }

    #[test]
    fn hello_payload_with_trailing_bytes_is_malformed() {
        let mut dec = Decoder::new();
        // Hello with extra byte after the string.
        let mut payload = vec![0x01, 0x01, 0x00, 0x00, 0xEE];
        let mut frame = MAGIC.to_vec();
        frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        frame.append(&mut payload);
        assert!(matches!(dec.push(&frame), Err(CodecError::Malformed(_))));
    }
}
