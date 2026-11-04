//! End-to-end zmodem passthrough smoke tests using real lrzsz binaries.
//!
//! These tests verify the ZmodemState bridge against a real `sz`/`rz` pair.
//! They are ignored by default because they require lrzsz on PATH.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use nrzsz_core::zmodem::ZmodemState;

fn rz_sz_available() -> bool {
    Command::new("sz")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}


/// A server-side process with piped stdio, simulating the remote `rz`/`sz`.
struct ServerProc {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
}

fn spawn_server(name: &str, dir: &std::path::Path, extra_args: &[&str]) -> ServerProc {
    let mut cmd = Command::new(name);
    cmd.args(["-v", "-e", "-b", "-B", "32768"])
        .args(extra_args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().expect("spawn server");
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    ServerProc { child, stdin, stdout }
}

/// Run an upload: the client runs `sz`, the server runs `rz`.
#[test]
#[ignore = "requires lrzsz on PATH"]
fn zmodem_upload_smoke() {
    if !rz_sz_available() {
        eprintln!("skipping: sz not found");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    let dst_dir = tmp.path().join("dst");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&dst_dir).unwrap();
    let payload = vec![0x5A; 256 * 1024];
    std::fs::write(src_dir.join("hello.bin"), &payload).unwrap();

    // Server side: `rz` receives into dst_dir.
    let mut server = spawn_server("rz", &dst_dir, &[]);

    // Client side: upload state (upload=true) bridging to the server's pipes.
    let state = Arc::new(ZmodemState::new(
        true,
        false,
        Arc::new(Mutex::new(Box::new(server.stdin))),
        Arc::new(Mutex::new(Box::new(std::io::sink()))),
    ));

    // Pump server output -> client state (handle_server_output) in a thread.
    let state_pump = state.clone();
    let mut server_stdout = server.stdout;
    let pump = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match server_stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if !state_pump.handle_server_output(&buf[..n]) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Run the client upload (launches `sz`, bridges to server).
    let files = vec![src_dir.join("hello.bin").to_string_lossy().to_string()];
    let st = state.clone();
    let dst_dir_clone = dst_dir.clone();
    std::thread::spawn(move || {
        nrzsz_core::zmodem::handle_zmodem_event(
            st,
            move || Ok(files.clone()),
            move || Ok(dst_dir_clone.to_string_lossy().to_string()),
        );
    });

    // Wait for the transfer to settle.
    std::thread::sleep(Duration::from_secs(3));
    // The server keeps its stdout open after a completed transfer; closing it
    // unblocks the pump so we can join.
    let _ = server.child.kill();
    let _ = pump.join();

    // Verify the file was received intact.
    let received = dst_dir.join("hello.bin");
    assert!(
        received.exists(),
        "expected received file at {:?}",
        received
    );
    let got = std::fs::read(&received).unwrap();
    assert_eq!(got, payload, "uploaded payload mismatch");

    let _ = server.child.wait();
}

/// Run a download: the client runs `rz`, the server runs `sz`.
#[test]
#[ignore = "requires lrzsz on PATH"]
fn zmodem_download_smoke() {
    if !rz_sz_available() {
        eprintln!("skipping: sz not found");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    let dst_dir = tmp.path().join("dst");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&dst_dir).unwrap();
    let payload = vec![0xA5; 256 * 1024];
    std::fs::write(src_dir.join("data.bin"), &payload).unwrap();

    // Server side: `sz` sends from src_dir.
    let mut server = spawn_server("sz", &src_dir, &["data.bin"]);
    // Client side: download state (upload=false) bridging to the server's pipes.
    let state = Arc::new(ZmodemState::new(
        false,
        false,
        Arc::new(Mutex::new(Box::new(server.stdin))),
        Arc::new(Mutex::new(Box::new(std::io::sink()))),
    ));

    // Pump server output -> client state in a thread.
    let state_pump = state.clone();
    let mut server_stdout = server.stdout;
    let pump = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match server_stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if !state_pump.handle_server_output(&buf[..n]) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Run the client download (launches `rz`, bridges to server).
    // For download, the client launches `rz` which writes the init sequence.
    let st = state.clone();
    let dst_dir_clone = dst_dir.clone();
    std::thread::spawn(move || {
        nrzsz_core::zmodem::handle_zmodem_event(
            st,
            move || Ok(vec![]),
            move || Ok(dst_dir_clone.to_string_lossy().to_string()),
        );
    });

    // Wait for the transfer to settle, then close the server's stdout so the
    // pump can finish and be joined.
    std::thread::sleep(Duration::from_secs(3));
    let _ = server.child.kill();
    let _ = pump.join();
    // Verify the file was downloaded intact.
    let received = dst_dir.join("data.bin");
    assert!(
        received.exists(),
        "expected downloaded file at {:?}",
        received
    );
    let got = std::fs::read(&received).unwrap();
    assert_eq!(got, payload, "downloaded payload mismatch");

    let _ = server.child.wait();
}
