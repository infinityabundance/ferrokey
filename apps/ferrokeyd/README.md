# ferrokeyd — the Ferrokey privileged daemon

The only privileged component of Ferrokey. It owns `/dev/uinput`, creates the
virtual keyboard, and serves key events to the unprivileged `ferrokey` UI
over an authenticated Unix socket (peer uid/gid authorization — deny by
default).

## Usage

```sh
sudo ferrokeyd [--config <path>] [--socket <path>]
```

- `--config <path>` — YAML configuration. The daemon is **deny-by-default**:
  it refuses to start without `allowed_uids` / `allowed_gids` configured
  (see `testing/fixtures/ferrokeyd.yaml` in the workspace for an example).
- `--socket <path>` — override the Unix socket path.

Requires access to `/dev/uinput` (root or the `uinput` group).

## Behavior guarantees

- **No stuck keys ever**: on client disconnect, crash, or SIGTERM the daemon
  releases every held key through the uinput device — and keeps the device
  alive briefly so the release events actually reach the compositor.
- **Rate-limited, size-bounded protocol**: a token bucket bounds per-peer
  throughput; the decoder is hostile-input resistant.
- **Recovery**: the device is created per-connection and torn down cleanly.

## License

Apache-2.0 OR MIT (see `LICENSE-APACHE` / `LICENSE-MIT`).
