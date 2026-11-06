//! Relay end-to-end test (AGENTS.md §7.3): `nrz` → `NrzszRelay` →
//! `NrzszFilter` (previous hop), over a real TCP tunnel.
//!
//! Symmetric to the download case: the relay rewrites the trigger's tunnel
//! port to a local listener, the previous hop filter uploads a file through
//! the relay, and the file lands in the relay's `nrz` destination byte-identical.

use std::io::Write;
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nrzsz_core::filter::{parse_trigger, NrzszFilter, NrzszTrigger};
use nrzsz_core::protocol::Config;
use nrzsz_core::relay::NrzszRelay;

#[derive(Clone)]
struct MemWriter(Arc<Mutex<Vec<u8>>>);
impl Write for MemWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Server {
    child: Child,
    out: MemWriter,
}

fn spawn_relay(mut server_cmd: Command) -> Server {
    let out = MemWriter(Arc::new(Mutex::new(Vec::new())));
    let mut child = server_cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn nrz");
    let server_out = child.stdout.take().unwrap();
    let server_in = child.stdin.take().unwrap();

    let child_in: Box<dyn std::io::Read + Send> = Box::new(std::io::empty());
    let client_out: Box<dyn Write + Send> = Box::new(out.clone());
    let server_in: Box<dyn Write + Send> = Box::new(server_in);
    let server_out: Box<dyn std::io::Read + Send> = Box::new(server_out);

    let mut relay = NrzszRelay::new(child_in, client_out, server_in, server_out);
    relay.set_tunnel_connector(|port: u16| {
        TcpStream::connect(("127.0.0.1", port)).map_err(anyhow::Error::from)
    });
    std::thread::spawn(move || {
        let _ = relay.run();
    });

    Server { child, out }
}

fn wait_for_trigger(server: &Server) -> NrzszTrigger {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let buf = server.out.0.lock().unwrap().clone();
        if let Some(trigger) = parse_trigger(&String::from_utf8_lossy(&buf)) {
            return trigger;
        }
        if Instant::now() > deadline {
            panic!(
                "timed out waiting for rewritten trigger in: {:?}",
                String::from_utf8_lossy(&buf)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn fill_random(buf: &mut [u8], seed: u64) {
    let mut state = seed;
    for b in buf.iter_mut() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *b = (state >> 33) as u8;
    }
}

const SIZE: usize = 2 * 1024 * 1024;

#[test]
fn relay_upload_full_stack() {
    let src_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();
    let src_file = src_dir.path().join("relay-upload.bin");
    let mut data = vec![0u8; SIZE];
    fill_random(&mut data, 0x7777_9999);
    std::fs::write(&src_file, &data).unwrap();

    // 1. Spawn `nrz` behind the relay.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nrz"));
    cmd.args(["-o", "-b", "-q", dest_dir.path().to_str().unwrap()]);
    let mut server = spawn_relay(cmd);

    // 2. The relay rewrites the trigger; mode is `R` (upload).
    let trigger = wait_for_trigger(&server);
    assert_eq!(trigger.mode, 'R', "upload trigger");

    // 3. Previous-hop role: an NrzszFilter with a tunnel connector and
    //    pre-set upload files receives the rewritten trigger and uploads.
    let mut filter = NrzszFilter::new(Config {
        quiet: true,
        ..Config::default()
    });
    let port = trigger.port;
    filter.set_tunnel_connector(move |_requested: u16| {
        TcpStream::connect(("127.0.0.1", port)).map_err(anyhow::Error::from)
    });
    filter.set_upload_files(vec![src_file.to_str().unwrap().to_string()]);
    filter.start().expect("start filter");

    let relay_output = server.out.0.lock().unwrap().clone();
    let _ = filter
        .process_output(&relay_output)
        .expect("process_output");

    // 4. Wait for the destination file to be written.
    let got = dest_dir.path().join("relay-upload.bin");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if got.exists() && std::fs::metadata(&got).map(|m| m.len()).unwrap_or(0) == SIZE as u64 {
            break;
        }
        if Instant::now() > deadline {
            panic!("timed out waiting for {got:?}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let got_bytes = std::fs::read(&got).unwrap();
    assert_eq!(data.len(), got_bytes.len(), "length mismatch");
    assert_eq!(data, got_bytes, "byte mismatch");

    let _ = server.child.wait();
}
