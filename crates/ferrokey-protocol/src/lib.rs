//! # ferrokey-protocol
//!
//! The Ferrokey wire protocol between `ferrokey` (UI) and `ferrokeyd`:
//! a tiny, length-prefixed, binary protocol (no JSON) with a hostile-input
//! resistant streaming decoder.
//!
//! ```text
//! FK01
//!   HELLO            client handshake (protocol version, client name)
//!   CREATE_KEYBOARD  request device creation
//!   KEY_DOWN u16     key code
//!   KEY_UP u16       key code
//!   RELEASE_ALL      emergency release
//!   PING u32         heartbeat (server replies PONG)
//! ```

#![forbid(unsafe_code)]

pub mod client;
pub mod codec;
pub mod message;
pub mod peer;

pub use client::{Client, ProtocolError};
pub use codec::{CodecError, Decoder};
pub use message::{
    ErrorCode, Message, Opcode, MAGIC, MAX_FRAME_LEN, MAX_STRING_LEN, PROTOCOL_VERSION,
};
pub use peer::{peer_identity, PeerIdentity};
