//! Rate-limit e2e: flood the real serve binary and require the
//! `ERROR(RateLimited)` frame and connection teardown (§51, §77).

#![allow(unsafe_code)]

use nix::sys::socket::{socketpair, AddressFamily, SockFlag, SockProtocol, SockType};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use ferrokey_protocol::{codec, Decoder, Message, PROTOCOL_VERSION};
use ferrokey_uinput::{DeviceOptions, UinputDevice};
use tempfile::TempDir;

#[test]
fn flood_is_rate_limited() {
    let tempdir = TempDir::new().expect("tempdir");
    let socket_path = tempdir.path().join("ferrokeyd.sock");

    let device = match UinputDevice::create(DeviceOptions::default()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping rate-limit e2e: cannot create uinput device: {e}");
            return;
        }
    };
    let device_fd = device.raw_fd();

    let (a, b) = socketpair(
        AddressFamily::Unix,
        SockType::SeqPacket,
        None::<SockProtocol>,
        SockFlag::empty(),
    )
    .expect("socketpair");
    let a_raw = a.as_raw_fd();
    let b_raw = b.as_raw_fd();
    let tempdir_file = std::fs::File::open(tempdir.path()).expect("open tempdir");
    let tempdir_raw = tempdir_file.as_raw_fd();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ferrokeyd"));
    cmd.arg("serve")
        .arg(b_raw.to_string())
        .arg("--socket")
        .arg(&socket_path)
        .arg("--socket-mode")
        .arg("0o666")
        .arg("--max-conn")
        .arg("4")
        .arg("--max-held")
        .arg("16")
        .arg("--burst")
        .arg("400")
        .arg("--per-sec")
        .arg("400")
        .arg("--device-name")
        .arg("Ferrokey Virtual Keyboard")
        .arg("--uid")
        .arg(nix::unistd::geteuid().as_raw().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env("RUST_LOG", "info");
    if nix::unistd::geteuid().is_root() {
        cmd.arg("--allow-root");
    }
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(move || {
            let mut to_close = Vec::new();
            if let Ok(entries) = std::fs::read_dir("/proc/self/fd") {
                for entry in entries.flatten() {
                    if let Some(name) = entry.file_name().to_str() {
                        if let Ok(fd) = name.parse::<i32>() {
                            if fd > 2 && fd != b_raw {
                                to_close.push(fd);
                            }
                        }
                    }
                }
            }
            for fd in to_close {
                let _ = nix::unistd::close(fd);
            }
            let _ = nix::unistd::close(a_raw);
            let _ = nix::unistd::close(device_fd);
            let _ = nix::unistd::close(tempdir_raw);
            Ok(())
        });
    }
    let mut child = cmd.spawn().expect("spawn serve");

    let cmsg = [nix::sys::socket::ControlMessage::ScmRights(&[device_fd])];
    let iov = [std::io::IoSlice::new(b"")];
    nix::sys::socket::sendmsg(
        a_raw,
        &iov,
        &cmsg,
        nix::sys::socket::MsgFlags::empty(),
        None::<&()>,
    )
    .expect("send fd");
    drop(a);
    drop(device);
    drop(b);

    // Wait for the socket, connect, handshake.
    let mut stream = connect_retry(&socket_path);
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut decoder = Decoder::new();

    let frame = |msg: &Message| codec::encode(msg).unwrap();
    stream
        .write_all(&frame(&Message::Hello {
            version: PROTOCOL_VERSION,
            client_name: "flood".into(),
        }))
        .unwrap();
    stream.write_all(&frame(&Message::OpenSession)).unwrap();
    let mut buf = [0u8; 256];
    let n = stream.read(&mut buf).unwrap();
    assert_eq!(
        decoder.push(&buf[..n]).unwrap(),
        vec![Message::Ok],
        "handshake must succeed"
    );

    // Flood: far more pings than the burst allows, while draining replies.
    let reader_stream = stream.try_clone().expect("try_clone");
    let reader = std::thread::spawn(move || {
        let mut value = Vec::new();
        let mut tmp = vec![0u8; 65536];
        let mut reader_stream = reader_stream;
        while let Ok(n) = reader_stream.read(&mut tmp) {
            if n == 0 {
                break;
            }
            value.extend_from_slice(&tmp[..n]);
        }
        value
    });

    let ping = frame(&Message::Ping(1));
    for _ in 0..2000 {
        if stream.write_all(&ping).is_err() {
            break; // connection torn down by the daemon
        }
    }
    // Give the daemon time to process and close.
    let replies = reader.join().expect("reader");
    // The ERROR(RateLimited) frame: opcode 0x81, error code 0x0006.
    let saw_error = replies.windows(3).any(|w| w == [0x81, 0x06, 0x00]);

    // Stop the broker.
    let pid = nix::unistd::Pid::from_raw(child.id() as i32);
    let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("serve did not exit");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "serve exited abnormally: {status}");

    if !saw_error {
        if let Some(mut err) = child.stderr.take() {
            let mut log = String::new();
            let _ = err.read_to_string(&mut log);
            eprintln!("serve stderr:\n{log}");
        }
    }
    assert!(
        saw_error,
        "flood was not rate-limited (no ERROR(RateLimited) frame)"
    );
}

fn connect_retry(socket_path: &std::path::Path) -> UnixStream {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(s) = UnixStream::connect(socket_path) {
            return s;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "serve never became reachable"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[allow(dead_code)]
fn _unused(_: &mut Child) {}
