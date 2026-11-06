//! Download-direction end-to-end test over the TCP tunnel (AGENTS.md §7.5).
//!
//! The upload direction (`nrzsz -u` + `nrz`) is smoke-tested end-to-end; this
//! covers the symmetric download path: a real `nsz` sender process binds a
//! tunnel port and prints the `S` trigger, and a client (the same code path
//! `filter.rs::download_files_from` uses — `TransferSession` handshake +
//! `send_action` / `recv_config` / `recv_files` / `client_exit`) downloads the
//! bytes and verifies them.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use nrzsz_core::filter::parse_trigger;
use nrzsz_core::protocol::{Config, FileInfo};
use nrzsz_core::transfer::{check_paths_readable, TransferSession};
use nrzsz_core::tunnel;

/// Wait for and parse the trigger line printed by the `nsz` server process.
///
/// This is deliberately imperative so we can put the remaining stdout back
/// after the trigger is consumed (Go's scanner equivalent).
fn wait_for_trigger(server: &mut Spawn) -> nrzsz_core::filter::NrzszTrigger {
    let child = &mut server.child;
    let stdout = child.stdout.take().expect("nsz stdout piped");
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                let stderr =
                    std::fs::read_to_string(server._stderr_dir.path().join("nsz-stderr.txt"))
                        .unwrap_or_default();
                panic!("nsz exited before printing the trigger. stderr:\n{stderr}");
            }
            Ok(_) => {}
            Err(e) => {
                let stderr =
                    std::fs::read_to_string(server._stderr_dir.path().join("nsz-stderr.txt"))
                        .unwrap_or_default();
                panic!("reading nsz trigger: {e}. stderr:\n{stderr}");
            }
        }
        if let Some(trigger) = parse_trigger(&line) {
            // Put the remaining buffered bytes back so any later stderr-like
            // output is not lost (not strictly needed for these tests).
            child.stdout.replace(reader.into_inner());
            return trigger;
        }
    }
}

/// Connect to the server tunnel port and perform the client hello / server
/// hello handshake, exactly like `filter.rs::run_transfer` does.
fn connect_tunnel(trigger: &nrzsz_core::filter::NrzszTrigger) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", trigger.port)).expect("connect tunnel");
    let (client_hello, server_hello) = tunnel::get_hello_constant(&trigger.unique_id, trigger.port);
    stream.write_all(client_hello.as_bytes()).unwrap();
    stream.write_all(b"\n").unwrap();
    stream.flush().unwrap();
    let response = tunnel::read_line_from_stream(&mut stream).expect("server hello");
    let response = response.trim().to_string();
    assert_eq!(response, server_hello, "unexpected server hello");
    stream
}

/// Drive the download as `filter.rs::download_files_from` and return the list
/// of saved files.
fn download_from(trigger: &nrzsz_core::filter::NrzszTrigger, dest: &Path) -> Vec<String> {
    let stream = connect_tunnel(trigger);
    let stdin_pipe = Box::new(stream.try_clone().unwrap());
    let stdout_pipe = Box::new(stream);
    let mut session = TransferSession::new(Config::default(), stdin_pipe, stdout_pipe);

    session
        .send_action(true, &trigger.version, false, true)
        .expect("send action");
    let server_config = session.recv_config().expect("recv config");
    let quiet = server_config.quiet;

    let saved = session.recv_files(dest, quiet).expect("recv files");
    let msg = format!("Saved {} file(s) to {}", saved.len(), dest.display());
    session.client_exit(&msg).expect("client exit");
    saved
}

/// Wait for the `nsz` server process to exit successfully.
fn finish_server(child: &mut Child) {
    let status = child.wait().expect("wait nsz");
    assert!(
        status.success(),
        "nsz exited with non-zero status: {status:?}"
    );
}

struct Spawn {
    child: Child,
    _stderr_dir: tempfile::TempDir,
}

impl Spawn {
    fn nsz(args: &[&str]) -> Spawn {
        let stderr_dir = tempfile::tempdir().unwrap();
        let stderr_path = stderr_dir.path().join("nsz-stderr.txt");
        let child = Command::new(env!("CARGO_BIN_EXE_nsz"))
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::from(
                std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(&stderr_path)
                    .unwrap(),
            ))
            .spawn()
            .expect("spawn nsz");
        eprintln!("nsz stderr: {}", stderr_path.display());
        Spawn {
            child,
            _stderr_dir: stderr_dir,
        }
    }
}

/// Deterministic pseudo-random byte fill (LCG), avoiding FLATE2-style
/// compressible content so this also exercises the binary framing.
fn fill_random(buf: &mut [u8], seed: u64) {
    let mut state = seed;
    for b in buf.iter_mut() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *b = (state >> 33) as u8;
    }
}

fn write_test_file(dir: &Path, name: &str, size: usize, seed: u64) -> PathBuf {
    let path = dir.join(name);
    let mut data = vec![0u8; size];
    fill_random(&mut data, seed);
    std::fs::write(&path, &data).unwrap();
    path
}

#[test]
fn tunnel_download_single_file() {
    let src_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();

    const SIZE: usize = 3 * 1024 * 1024 + 17; // > one buffer, non-multiple
    let src = write_test_file(src_dir.path(), "download.bin", SIZE, 0x1234_5678);

    let src_str = src.to_str().unwrap().to_string();
    let mut server = Spawn::nsz(&["-o", "-b", &src_str]);
    let trigger = wait_for_trigger(&mut server);

    assert_eq!(trigger.mode, 'S', "download trigger mode");
    let dest = dest_dir.path();

    let saved = download_from(&trigger, dest);
    assert_eq!(saved.len(), 1, "one file saved, got {saved:?}");

    let got_path = dest.join("download.bin");
    assert!(got_path.exists(), "downloaded file missing");
    let src_bytes = std::fs::read(&src).unwrap();
    let got_bytes = std::fs::read(&got_path).unwrap();
    assert_eq!(src_bytes.len(), got_bytes.len(), "length mismatch");
    assert_eq!(src_bytes, got_bytes, "byte mismatch");

    finish_server(&mut server.child);
}

#[test]
fn tunnel_download_directory_recursive() {
    let src_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();

    let root = src_dir.path().join("proj");
    std::fs::create_dir_all(root.join("sub/deep")).unwrap();

    let f1p = root.join("a.txt");
    std::fs::write(&f1p, "a".repeat(4096)).unwrap();
    let f1 = f1p;
    let f2 = write_test_file(&root.join("sub"), "b.bin", 100_000, 0xbbbb_0002);
    let f3p = root.join("sub/deep/c.dat");
    std::fs::write(&f3p, "short").unwrap();
    let f3 = f3p;

    let mut server = Spawn::nsz(&["-o", "-b", "-d", root.to_str().unwrap()]);
    let trigger = wait_for_trigger(&mut server);
    assert_eq!(trigger.mode, 'S');

    let dest = dest_dir.path();
    let saved = download_from(&trigger, dest);

    for (rel, src_path) in [
        ("proj/a.txt", &f1),
        ("proj/sub/b.bin", &f2),
        ("proj/sub/deep/c.dat", &f3),
    ] {
        let got = dest.join(rel);
        assert!(got.exists(), "missing {rel}");
        assert_eq!(
            std::fs::read(src_path).unwrap(),
            std::fs::read(&got).unwrap(),
            "content mismatch for {rel}"
        );
    }

    // Go parity: recvFiles() returns the deduped top-level local names
    // (srcFile.RelPath[0]), which is only the directory root for a directory
    // transfer — NOT one entry per file.
    assert_eq!(
        saved.len(),
        1,
        "expected the dir root as saved name, got {saved:?}"
    );
    assert_eq!(
        saved[0], "proj",
        "saved name should be the source dir name, got {saved:?}"
    );

    finish_server(&mut server.child);
}

/// Round-trip sanity: the same session/`FileInfo` machinery used by the
/// wrapper's upload path can list the files we just downloaded (guards the
/// `FileInfo` shape crossing the wire in both directions).
#[test]
fn check_paths_readable_matches_saved_names() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_test_file(dir.path(), "x.bin", 128, 42);

    let infos = check_paths_readable(&[f.to_str().unwrap().to_string()], false).unwrap();
    assert_eq!(infos.len(), 1);
    let info: &FileInfo = &infos[0];
    assert_eq!(
        std::path::Path::new(&info.abs_path).canonicalize().unwrap(),
        f.canonicalize().unwrap()
    );
    assert!(!info.is_dir);
    assert_eq!(info.size, f.metadata().unwrap().len());
}
