//! Protocol client used by the UI to talk to `ferrokeyd`.
//!
//! The client is deliberately thin: it encodes/decodes frames and implements
//! [`ferrokey_core::action::KeySink`] so the core keyboard driver can run
//! against the daemon without knowing anything about sockets.

use crate::codec::{encode, CodecError, Decoder};
use crate::message::Message;
use ferrokey_core::{KeySink, PhysicalKey, SinkError};
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// Errors from client-side protocol operation.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("cannot connect to {path}: {source}")]
    Connect { path: String, source: io::Error },
    #[error("codec error: {0}")]
    Codec(#[from] CodecError),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("ping timed out after {0:?}")]
    PingTimeout(Duration),
    #[error("server replied with error: {0:?}")]
    Server(Message),
}

/// A connected protocol client.
pub struct Client {
    stream: UnixStream,
    decoder: Decoder,
}

impl Client {
    /// Connect to the daemon's Unix socket.
    pub fn connect(path: &std::path::Path) -> Result<Self, ProtocolError> {
        let stream = UnixStream::connect(path).map_err(|source| ProtocolError::Connect {
            path: path.display().to_string(),
            source,
        })?;
        Ok(Client {
            stream,
            decoder: Decoder::new(),
        })
    }

    /// Send one message.
    pub fn send(&mut self, msg: &Message) -> Result<(), ProtocolError> {
        let frame = encode(msg)?;
        self.stream.write_all(&frame)?;
        Ok(())
    }

    /// Set the socket non-blocking (used by the UI event loop).
    pub fn set_nonblocking(&mut self, nonblocking: bool) -> Result<(), ProtocolError> {
        self.stream.set_nonblocking(nonblocking)?;
        Ok(())
    }

    /// Read whatever complete messages are currently available.
    ///
    /// In non-blocking mode this returns `Ok(vec![])` when no data is ready.
    pub fn read_available(&mut self) -> Result<Vec<Message>, ProtocolError> {
        let mut buf = [0u8; 4096];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    // EOF: the daemon closed the connection.
                    return Err(ProtocolError::Io(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "daemon closed the connection",
                    )));
                }
                Ok(n) => return self.decoder.push(&buf[..n]).map_err(ProtocolError::Codec),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(Vec::new()),
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Send a ping and wait (blocking) for the matching pong.
    pub fn ping(&mut self, timeout: Duration) -> Result<(), ProtocolError> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        self.send(&Message::Ping(nonce))?;
        let deadline = std::time::Instant::now() + timeout;
        self.stream.set_read_timeout(Some(timeout))?;
        loop {
            if std::time::Instant::now() > deadline {
                return Err(ProtocolError::PingTimeout(timeout));
            }
            for msg in self.read_available()? {
                match msg {
                    Message::Pong(n) if n == nonce => return Ok(()),
                    Message::Error(code, _) => {
                        return Err(ProtocolError::Server(Message::Error(code, String::new())))
                    }
                    _ => continue,
                }
            }
        }
    }

    /// Detach the underlying socket (for handoff to another owner).
    pub fn into_stream(self) -> UnixStream {
        self.stream
    }
}

impl KeySink for Client {
    fn key_down(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        self.send(&Message::KeyDown(key.linux_code() as u16))
            .map_err(|e| SinkError(e.to_string()))
    }

    fn key_up(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        self.send(&Message::KeyUp(key.linux_code() as u16))
            .map_err(|e| SinkError(e.to_string()))
    }

    fn release_all(&mut self) -> Result<(), SinkError> {
        self.send(&Message::ReleaseAll)
            .map_err(|e| SinkError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::encode;

    #[test]
    fn client_round_trip_over_socket_pair() {
        let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
        let mut client = Client {
            stream: client_stream,
            decoder: Decoder::new(),
        };

        client.send(&Message::KeyDown(30)).unwrap();
        let mut buf = [0u8; 256];
        let n = server_stream.read(&mut buf).unwrap();
        let mut server_dec = Decoder::new();
        let msgs = server_dec.push(&buf[..n]).unwrap();
        assert_eq!(msgs, vec![Message::KeyDown(30)]);

        // Server replies, client reads it back.
        let frame = encode(&Message::Pong(7)).unwrap();
        server_stream.write_all(&frame).unwrap();
        client.set_nonblocking(false).unwrap();
        let msgs = client.read_available().unwrap();
        assert_eq!(msgs, vec![Message::Pong(7)]);
    }

    #[test]
    fn keysink_maps_to_protocol_messages() {
        let (a, mut b) = UnixStream::pair().unwrap();
        let mut client = Client {
            stream: a,
            decoder: Decoder::new(),
        };
        client.key_down(PhysicalKey::A).unwrap();
        client.key_up(PhysicalKey::A).unwrap();
        client.release_all().unwrap();

        b.set_nonblocking(true).unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 256];
        loop {
            match b.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("read failed: {e}"),
            }
        }
        let mut dec = Decoder::new();
        let msgs = dec.push(&buf).unwrap();
        assert_eq!(
            msgs,
            vec![
                Message::KeyDown(30),
                Message::KeyUp(30),
                Message::ReleaseAll,
            ]
        );
    }
}
