//! Wire messages of the Ferrokey protocol.
//!
//! The protocol is deliberately tiny — a `ferrokeyd` security boundary of a
//! few thousand lines needs to stay auditable:
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
//!
//! Messages are transported in length-prefixed frames; see [`crate::codec`].

/// The magic bytes at the start of every frame.
pub const MAGIC: &[u8; 4] = b"FK01";

/// The current protocol version.
pub const PROTOCOL_VERSION: u8 = 1;

/// Maximum accepted frame payload length (defends against hostile lengths).
/// The largest legitimate message is `HELLO`/`ERROR` with a 256-byte string
/// (≈270 bytes), so 4 KiB leaves generous headroom while keeping the
/// worst-case buffering the decoder will ever do tiny.
pub const MAX_FRAME_LEN: usize = 4096;

/// Opcodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    // Client → server
    Hello = 0x01,
    CreateKeyboard = 0x02,
    KeyDown = 0x10,
    KeyUp = 0x11,
    ReleaseAll = 0x12,
    Ping = 0x20,
    // Server → client
    Pong = 0x21,
    Ok = 0x80,
    Error = 0x81,
}

impl Opcode {
    pub const fn from_u8(v: u8) -> Option<Opcode> {
        match v {
            0x01 => Some(Opcode::Hello),
            0x02 => Some(Opcode::CreateKeyboard),
            0x10 => Some(Opcode::KeyDown),
            0x11 => Some(Opcode::KeyUp),
            0x12 => Some(Opcode::ReleaseAll),
            0x20 => Some(Opcode::Ping),
            0x21 => Some(Opcode::Pong),
            0x80 => Some(Opcode::Ok),
            0x81 => Some(Opcode::Error),
            _ => None,
        }
    }
}

/// A message in the Ferrokey protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Client greeting: protocol version + human-readable client name.
    Hello { version: u8, client_name: String },
    /// Request creation of the virtual keyboard.
    CreateKeyboard,
    /// Press a key (linux input code).
    KeyDown(u16),
    /// Release a key (linux input code).
    KeyUp(u16),
    /// Release every held key.
    ReleaseAll,
    /// Heartbeat; the server must reply [`Message::Pong`] with the same nonce.
    Ping(u32),
    /// Server heartbeat reply.
    Pong(u32),
    /// Server acknowledgement of a successful operation.
    Ok,
    /// Server rejection with a machine-readable code and human message.
    Error(ErrorCode, String),
}

/// Machine-readable error codes sent by the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ErrorCode {
    /// Protocol version mismatch in the handshake.
    VersionMismatch = 0x0001,
    /// Peer credentials failed SO_PEERCRED validation.
    Unauthorized = 0x0002,
    /// Device creation failed.
    DeviceError = 0x0003,
    /// A key code outside the explicit capability set.
    UnknownKey = 0x0004,
    /// Impossible key transition (e.g. KEY_UP without KEY_DOWN).
    InvalidKeyState = 0x0005,
    /// Message rate limit exceeded.
    RateLimited = 0x0006,
    /// Malformed frame.
    Malformed = 0x0007,
    /// The keyboard was already created (duplicate CREATE_KEYBOARD).
    AlreadyCreated = 0x0008,
    /// Server is shutting down.
    ShuttingDown = 0x0009,
    /// Unspecified internal error.
    Internal = 0x00FF,
}

impl ErrorCode {
    pub const fn from_u16(v: u16) -> Option<ErrorCode> {
        match v {
            0x0001 => Some(ErrorCode::VersionMismatch),
            0x0002 => Some(ErrorCode::Unauthorized),
            0x0003 => Some(ErrorCode::DeviceError),
            0x0004 => Some(ErrorCode::UnknownKey),
            0x0005 => Some(ErrorCode::InvalidKeyState),
            0x0006 => Some(ErrorCode::RateLimited),
            0x0007 => Some(ErrorCode::Malformed),
            0x0008 => Some(ErrorCode::AlreadyCreated),
            0x0009 => Some(ErrorCode::ShuttingDown),
            0x00FF => Some(ErrorCode::Internal),
            _ => None,
        }
    }

    pub const fn code(self) -> u16 {
        self as u16
    }
}

/// The maximum length of a client name / error string.
pub const MAX_STRING_LEN: usize = 256;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcodes_round_trip() {
        for op in [
            Opcode::Hello,
            Opcode::CreateKeyboard,
            Opcode::KeyDown,
            Opcode::KeyUp,
            Opcode::ReleaseAll,
            Opcode::Ping,
            Opcode::Pong,
            Opcode::Ok,
            Opcode::Error,
        ] {
            assert_eq!(Opcode::from_u8(op as u8), Some(op));
        }
        assert_eq!(Opcode::from_u8(0x00), None);
        assert_eq!(Opcode::from_u8(0x7f), None);
        assert_eq!(Opcode::from_u8(0x82), None);
    }

    #[test]
    fn error_codes_round_trip() {
        for code in [
            ErrorCode::VersionMismatch,
            ErrorCode::Unauthorized,
            ErrorCode::DeviceError,
            ErrorCode::UnknownKey,
            ErrorCode::InvalidKeyState,
            ErrorCode::RateLimited,
            ErrorCode::Malformed,
            ErrorCode::AlreadyCreated,
            ErrorCode::ShuttingDown,
            ErrorCode::Internal,
        ] {
            assert_eq!(ErrorCode::from_u16(code.code()), Some(code));
        }
        assert_eq!(ErrorCode::from_u16(0x0000), None);
    }

    #[test]
    fn magic_and_limits() {
        assert_eq!(MAGIC, b"FK01");
        assert_eq!(PROTOCOL_VERSION, 1);
    }
}
