# ferrokey-protocol

The Ferrokey wire protocol between `ferrokey` (UI) and `ferrokeyd` (daemon):
a tiny, length-prefixed, binary protocol (no JSON) with a hostile-input
resistant streaming decoder.

```text
FK01
  HELLO            client handshake (protocol version, client name)
  CREATE_KEYBOARD  request device creation
  KEY_DOWN u16     key code
  KEY_UP u16       key code
  KEY_REPEAT u16   autorepeat (EV_KEY value=2) of a held key
  RELEASE_ALL      emergency release
  PING u32         heartbeat (server replies PONG)
```

## Why binary

The protocol carries thousands of key events per second with a hard
low-latency budget; a length-prefixed binary framing keeps parsing
deterministic, and the streaming `Decoder` never allocates more than the
frame size. Auth is done by Unix peer credentials (uid/gid), not by tokens on
the wire.

## Components

- `Message` / `Opcode` — the frame vocabulary.
- `codec::encode` / `codec::Decoder` — length-prefixed framing with strict
  length bounds and garbage rejection.
- `client::Client` — a `KeySink` implementation that sends events to the
  daemon socket.
- `peer::peer_identity` — peer uid/gid extraction for authorization.

## Example

```rust,no_run
use ferrokey_core::KeySink;
use ferrokey_protocol::client::Client;

let mut client = Client::connect(std::path::Path::new("/run/ferrokeyd.sock"))?;
client.key_down(ferrokey_core::PhysicalKey::A)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## License

Apache-2.0 OR MIT (see the workspace root `LICENSE-APACHE` / `LICENSE-MIT`).
