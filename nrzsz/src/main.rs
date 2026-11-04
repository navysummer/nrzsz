use std::env;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use anyhow::Result;
use clap::Parser;


use nrzsz_core::config::NrzszConfig;
use nrzsz_core::filter::NrzszFilter;
use nrzsz_core::protocol::Config;
use nrzsz_core::relay::NrzszRelay;

#[derive(Parser, Debug)]
#[command(name = "nrzsz", about = "nrzsz compatible wrapper for file transfer", disable_help_flag = true, disable_version_flag = true)]
struct Args {
    #[arg(short, long)]
    help: bool,

    #[arg(short, long)]
    version: bool,

    #[arg(short, long)]
    relay: bool,

    #[arg(short = 't', long)]
    tracelog: bool,

    #[arg(short = 'd', long)]
    dragfile: bool,

    #[arg(short = 'z', long)]
    zmodem: bool,

    #[arg(short = 'E', long = "rename")]
    rename: bool,

    #[arg(short = 'o', long)]
    osc52: bool,

    #[arg(short = 'u', long = "upload")]
    upload_files: Vec<String>,

    #[arg(short = 'D', long = "download-dir")]
    download_dir: Option<String>,

    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.help {
        print_help();
        return Ok(());
    }
    if args.version {
        println!("nrzsz {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.tracelog {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();
    }
    if args.command.is_empty() {
        print_help();
        return Ok(());
    }

    let config = NrzszConfig::load().unwrap_or_default();

    let mut cmd = Command::new(&args.command[0]);
    if args.command.len() > 1 {
        cmd.args(&args.command[1..]);
    }
    cmd.env("NRZSZ", "1");
    cmd.env("NRZSZ_CWD", env::current_dir()?);

    if args.relay {
        // Relay mode: spawn child with piped stdin/stdout, bridge all 4 streams
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let child_stdin = child.stdin.take().unwrap();
        let child_stdout = child.stdout.take().unwrap();
        let mut relay = NrzszRelay::new(
            Box::new(io::stdin()),
            Box::new(io::stdout()),
            Box::new(child_stdin),
            Box::new(child_stdout),
        );
        relay.set_tunnel_connector(move |port: u16| {
            std::net::TcpStream::connect(("127.0.0.1", port))
                .map_err(|e| anyhow::anyhow!("Relay tunnel connect to {} failed: {}", port, e))
        });
        if args.tracelog {
            relay.set_trace_logger(nrzsz_core::tracelog::TraceLogger::new());
        }
        relay.run()?;
        child.wait()?;
        return Ok(());
    }

    // Filter mode: spawn child with piped stdout, set stdin raw, wrap both
    // Matches Go's TrzszMain: spawn pty, set stdin raw, wrap input + output
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut filter = NrzszFilter::new(Config::default());
    filter.read_config();
    filter.set_tunnel_connector(move |port: u16| {
        std::net::TcpStream::connect(("127.0.0.1", port))
            .map_err(|e| anyhow::anyhow!("Tunnel connect to {} failed: {}", port, e))
    });
    if !config.default_download_path.is_empty() {
        filter.set_default_download_path(config.default_download_path.clone());
    }
    if let Some(dir) = &args.download_dir {
        filter.set_default_download_path(dir.clone());
    }
    if !args.upload_files.is_empty() {
        filter.set_upload_files(args.upload_files.clone());
    }
    filter.set_enable_osc52(args.osc52);
    filter.set_enable_zmodem(args.zmodem);
    filter.set_zmodem_auto_rename(args.rename);
    filter.start()?;
    let stopped = filter.stopped_flag();
    let is_transferring = filter.is_transferring_flag();

    // Shared writer to child stdin: used by both wrapInput and drag-upload.
    let child_stdin = child.stdin.take().unwrap();
    let child_stdin_writer: Arc<Mutex<Box<dyn Write + Send>>> =
        Arc::new(Mutex::new(Box::new(child_stdin)));
    filter.set_server_in(child_stdin_writer.clone());

    // Shared stdout writer: used by wrapOutput and by zmodem progress output.
    let stdout_writer: Arc<Mutex<Box<dyn Write + Send>>> =
        Arc::new(Mutex::new(Box::new(io::stdout())));
    filter.set_client_out(stdout_writer.clone());

    let filter = std::sync::Arc::new(std::sync::Mutex::new(filter));


    // Spawn two threads: stdin→child stdin (wrapInput), child stdout→terminal (wrapOutput)
    let child_stdout = child.stdout.take().unwrap();

    // Handle SIGINT (Ctrl+C) → stop transferring files
    // Handle SIGTERM → terminate child
    let stopped_sig = stopped.clone();
    let child_id = child.id();
    ctrlc::set_handler(move || {
        stopped_sig.store(true, Ordering::SeqCst);
        unsafe {
            libc::kill(child_id as i32, libc::SIGTERM);
        }
    }).ok();

    // Thread 1: wrapInput — reads real stdin, forwards to child stdin
    let input_stopped = stopped.clone();
    let input_filter = filter.clone();
    let input_handle = std::thread::spawn(move || {
        wrap_input(io::stdin(), child_stdin_writer, input_stopped, input_filter)
    });

    // Thread 2: wrapOutput — reads child stdout, processes through filter, writes to terminal
    // Go's wrapOutput: detect triggers, handle OSC52, forward to client
    let output_stopped = stopped.clone();
    let output_is_transferring = is_transferring.clone();
    let output_filter = filter.clone();
    let output_handle = std::thread::spawn(move || {
        wrap_output(child_stdout, stdout_writer, output_filter, output_stopped, output_is_transferring)
    });






    // Wait for output thread (signals child exit)
    let _ = output_handle.join();
    // Signal input thread to stop
    stopped.store(true, Ordering::SeqCst);
    let _ = input_handle.join();
    child.wait()?;
    Ok(())
}


/// wrapInput: Read from client stdin and write to server stdin.
/// Matches Go's TrzszFilter.wrapInput() — detect drag file, forward to server.
fn wrap_input(
    mut client_in: impl Read,
    server_in: Arc<Mutex<Box<dyn Write + Send>>>,
    stopped: Arc<std::sync::atomic::AtomicBool>,
    filter: std::sync::Arc<std::sync::Mutex<NrzszFilter>>,
) {
    let mut buf = [0u8; 32 * 1024];
    loop {
        if stopped.load(Ordering::SeqCst) {
            break;
        }
        match client_in.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                // Process input through the filter (detects drag files, sends trz cmd).
                let to_forward = {
                    let f = filter.lock().unwrap();
                    f.process_input(&buf[..n])
                };
                if to_forward.is_empty() {
                    continue;
                }
                {
                    let mut w = server_in.lock().unwrap();
                    if w.write_all(&to_forward).is_err() {
                        break;
                    }
                    w.flush().ok();
                }
            }
            Err(_) => break,
        }
    }
}

/// wrapOutput: Read from server stdout, process through filter, write to client terminal.
/// Matches Go's TrzszFilter.wrapOutput() — detect triggers, OSC52, skipUploadCommand, forward to client.
fn wrap_output(
    mut server_out: impl Read,
    client_out: Arc<Mutex<Box<dyn Write + Send>>>,
    filter: std::sync::Arc<std::sync::Mutex<NrzszFilter>>,
    stopped: Arc<std::sync::atomic::AtomicBool>,
    is_transferring: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut buf = [0u8; 32 * 1024];

    loop {
        if stopped.load(Ordering::SeqCst) {
            break;
        }
        match server_out.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if is_transferring.load(Ordering::SeqCst) {
                    continue;
                }
                // Skip upload command echo (matching Go's skipUploadCommand)
                let output = {
                    let mut f = filter.lock().unwrap();
                    if f.should_skip_output(&buf[..n]) {
                        continue;
                    }
                    match f.process_output(&buf[..n]) {
                        Ok(o) => o,
                        Err(_) => break,
                    }
                };
                if !output.is_empty() {
                    if client_out.lock().unwrap().write_all(&output).is_err() {
                        break;
                    }
                    client_out.lock().unwrap().flush().ok();
                }
            }
            Err(_) => break,
        }
    }
}

fn print_help() {
    println!("usage: nrzsz [-h] [-v] [-r] [-t] [-d] [-z] [-o] [-u file] [-D dir] command line");
    println!();
    println!("Wrapping command line to support nrzsz (nsz / nrz).");
    println!();
    println!("positional arguments:");
    println!("  command line       the original command line");
    println!();
    println!("optional arguments:");
    println!("  -h, --help         show this help message and exit");
    println!("  -v, --version      show version number and exit");
    println!("  -r, --relay        run as a nrzsz relay server");
    println!("  -t, --tracelog     enable trace log for debugging");
    println!("  -d, --dragfile     enable drag file(s) to upload");
    println!("  -z, --zmodem       enable zmodem lrzsz (rz / sz)");
    println!("  -E, --rename       zmodem: rename if the target file exists");
    println!("  -u, --upload       upload file(s) to the remote (repeatable)");
    println!("  -D, --download-dir directory to save downloaded files");
}
