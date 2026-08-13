//! End-to-end runtime test: the **real** `ferrokeyd serve` binary, run as a
//! subprocess, serving a real protocol client through the full security
//! freeze (capset → NO_NEW_PRIVS → seccomp → enforcement probes) and the
//! poll event loop.
//!
//! The parent test process plays the role of the supervisor's *bootstrap
//! component*: it creates the uinput device (host-side; the dev host ACL
//! grants the test user access), transfers the fd over a private socketpair,
//! then acts as an ordinary protocol client.
//!
//! This is host-side integration testing of the runtime mechanics; the
//! privileged aspects (non-root drop, single-device creation, no physical
//! input access) are proven by the VM security courts.

#![allow(unsafe_code)] // the pre_exec fd hygiene in this test is documented below

use nix::sys::socket::{socketpair, AddressFamily, SockFlag, SockProtocol, SockType};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use ferrokey_core::PhysicalKey;
use ferrokey_protocol::{codec, Decoder, Message, PROTOCOL_VERSION};
use ferrokey_uinput::{DeviceOptions, UinputDevice};
use tempfile::TempDir;

#[test]
fn serve_runtime_end_to_end() {
    // ── Parent mode (the test process) ───────────────────────────────────
    let tempdir = TempDir::new().expect("tempdir");
    let socket_path = tempdir.path().join("ferrokeyd.sock");

    // 1. The device (the bootstrap component's job).
    let device = match UinputDevice::create(DeviceOptions::default()) {
        Ok(d) => d,
        Err(e) => {
            // No /dev/uinput access in this environment (e.g. locked-down
            // CI): skip rather than fail — the VM courts are authoritative.
            eprintln!("skipping runtime e2e: cannot create uinput device: {e}");
            return;
        }
    };
    let device_fd = device.raw_fd();

    // 2. The private handoff channel.
    let (a, b) = socketpair(
        AddressFamily::Unix,
        SockType::SeqPacket,
        None::<SockProtocol>,
        SockFlag::empty(),
    )
    .expect("socketpair");
    let a_raw = a.as_raw_fd();
    let b_raw = b.as_raw_fd();
    // Keep an open fd on the tempdir so the child's fd-hygiene pre_exec can
    // close it (serve's FD inventory must see only stdio + handoff + device
    // + listener).
    let _tempdir_file = std::fs::File::open(tempdir.path()).expect("open tempdir");

    // 3. Spawn the real serve binary. In the child, close everything serve
    //    must not have: the other socketpair end, the device fd (serve
    //    receives its own copy), and the tempdir fd.
    let bin = env!("CARGO_BIN_EXE_ferrokeyd");
    let mut cmd = Command::new(bin);
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
        .arg("1000")
        .arg("--per-sec")
        .arg("1000")
        .arg("--device-name")
        .arg("Ferrokey Virtual Keyboard")
        .arg("--uid")
        .arg(nix::unistd::geteuid().as_raw().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env("RUST_LOG", "trace");
    if nix::unistd::geteuid().is_root() {
        // In a root-run unit court, allow the dev override; the VM courts
        // prove the real non-root path.
        cmd.arg("--allow-root");
    }
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(move || {
            // Give serve a clean fd table, as the real supervisor does:
            // close everything except stdio and the handoff end (b). The
            // test parent (running under an editor/CI harness) may hold
            // unrelated fds that serve's inventory check must never see.
            // Collect the fd numbers first, then close: closing a fd while
            // iterating /proc/self/fd would invalidate the directory fd.
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
            Ok(())
        });
    }
    let mut child = cmd.spawn().expect("spawn serve");

    // 4. Transfer the device fd (the bootstrap component's transfer step).
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

    // 5. Act as a protocol client.
    let result = run_client(&socket_path);
    drop(device);
    drop(b);

    // 6. Stop the broker cleanly and reap it.
    let pid = nix::unistd::Pid::from_raw(child.id() as i32);
    let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
    let status = wait_child(&mut child, Duration::from_secs(10));
    if !status.success() {
        if let Some(mut err) = child.stderr.take() {
            let mut log = String::new();
            let _ = err.read_to_string(&mut log);
            eprintln!("serve stderr:\n{log}");
        }
    }
    assert!(
        status.success(),
        "serve must exit cleanly after SIGTERM, got: {status}"
    );

    assert!(result, "protocol client flow failed");
}

fn run_client(socket_path: &PathBuf) -> bool {
    // Wait for the broker to bind the socket (startup includes device
    // verification over sysfs, which takes a few milliseconds).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
        match UnixStream::connect(socket_path) {
            Ok(s) => break s,
            Err(e) => {
                if std::time::Instant::now() > deadline {
                    eprintln!("client connect failed: {e}");
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut decoder = Decoder::new();

    let send = |stream: &mut UnixStream, msg: &Message| -> bool {
        match codec::encode(msg) {
            Ok(frame) => stream.write_all(&frame).is_ok(),
            Err(_) => false,
        }
    };
    let recv = |stream: &mut UnixStream, decoder: &mut Decoder| -> Option<Message> {
        let mut buf = [0u8; 4096];
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => None,
            Ok(n) => decoder
                .push(&buf[..n])
                .ok()
                .and_then(|v| v.into_iter().next()),
        }
    };

    // HELLO + OPEN_SESSION → Ok
    if !send(
        &mut stream,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            client_name: "e2e".into(),
        },
    ) {
        return false;
    }
    if !send(&mut stream, &Message::OpenSession) {
        return false;
    }
    match recv(&mut stream, &mut decoder) {
        Some(Message::Ok) => {}
        other => {
            eprintln!("expected Ok after OPEN_SESSION, got {other:?}");
            return false;
        }
    }

    // KEY_DOWN A, KEY_UP A (no replies), RELEASE_ALL → Ok
    let code = u16::try_from(PhysicalKey::A.linux_code()).unwrap();
    if !send(&mut stream, &Message::KeyDown(code)) {
        return false;
    }
    if !send(&mut stream, &Message::KeyUp(code)) {
        return false;
    }
    if !send(&mut stream, &Message::ReleaseAll) {
        return false;
    }
    match recv(&mut stream, &mut decoder) {
        Some(Message::Ok) => {}
        other => {
            eprintln!("expected Ok after RELEASE_ALL, got {other:?}");
            return false;
        }
    }

    // PING → PONG
    if !send(&mut stream, &Message::Ping(42)) {
        return false;
    }
    match recv(&mut stream, &mut decoder) {
        Some(Message::Pong(42)) => {}
        other => {
            eprintln!("expected Pong(42), got {other:?}");
            return false;
        }
    }
    true
}

fn wait_child(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("serve did not exit in time");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
