use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use anyhow::{Result, anyhow};
use clap::{Parser, ValueHint};
#[cfg(not(unix))]
use tokio::signal;

use nrzsz_core::protocol::{Config, CompressType};
use nrzsz_core::transfer::TransferSession;
use nrzsz_core::utils::*;
use nrzsz_core::tmux::*;

#[derive(Parser)]
#[command(name = "nrz")]
#[command(about = "Receive file(s), similar to rz and compatible with tmux.")]
#[command(version)]
struct Args {
    /// Path to save file(s). Default: current directory
    #[arg(value_hint = ValueHint::DirPath)]
    path: Option<String>,
    
    /// Absolute path of a file or directory on the client to upload (repeatable)
    #[arg(short = 'F', long = "file", value_hint = ValueHint::FilePath)]
    file: Vec<String>,
    
    /// Quiet (hide progress bar)
    #[arg(short, long)]
    quiet: bool,
    
    /// Yes, overwrite existing file(s)
    #[arg(short, long)]
    overwrite: bool,
    
    /// Binary transfer mode, faster for binary files
    #[arg(short, long)]
    binary: bool,
    
    /// Escape all known control characters
    #[arg(short, long)]
    escape: bool,
    
    /// Transfer directories and files
    #[arg(short, long)]
    directory: bool,
    
    /// Transfer directories and files, same as -d
    #[arg(short, long)]
    recursive: bool,
    
    /// Background transfer (implies -q)
    #[arg(short, long)]
    fork: bool,
    
    /// Compression type (auto/yes/no)
    #[arg(short, long, default_value = "auto")]
    compress: String,
    
    /// Max buffer chunk size (1K<=N<=1G). Default: 10M
    #[arg(short = 'B', long = "bufsize")]
    bufsize: Option<String>,
    
    /// Timeout (N seconds) for each buffer chunk. N <= 0 means never timeout. Default: 20
    #[arg(short = 't', long = "timeout")]
    timeout: Option<u32>,
    
    /// ASCII transfer mode (text mode; forces binary off)
    #[arg(short = 'a', long)]
    ascii: bool,
    
    /// Resume interrupted file transfer
    #[arg(long)]
    resume: bool,
    
    /// Protect existing files: skip files that already exist
    #[arg(short = 'p', long)]
    protect: bool,
    
    /// Window size (N bytes) for transfer, capped by -B
    #[arg(short = 'w', long = "windowsize")]
    windowsize: Option<u32>,
    
    /// Overwrite if source is newer, otherwise skip
    #[arg(short = 'Y', long = "overwrite-or-skip")]
    overwrite_or_skip: bool,
    
    /// Force rename if file already exists
    #[arg(short = 'E', long)]
    rename: bool,
    
    /// Append to existing files
    #[arg(short = '+', long)]
    append: bool,
}

fn parse_size(size_str: &str) -> Result<usize> {
    let size_str = size_str.trim().to_lowercase();
    
    if size_str.ends_with("gb") {
        let num: usize = size_str.trim_end_matches("gb").parse()?;
        Ok(num * 1024 * 1024 * 1024)
    } else if size_str.ends_with("mb") {
        let num: usize = size_str.trim_end_matches("mb").parse()?;
        Ok(num * 1024 * 1024)
    } else if size_str.ends_with("kb") {
        let num: usize = size_str.trim_end_matches("kb").parse()?;
        Ok(num * 1024)
    } else if size_str.ends_with("b") {
        let num: usize = size_str.trim_end_matches("b").parse()?;
        Ok(num)
    } else if size_str.ends_with("g") {
        let num: usize = size_str.trim_end_matches("g").parse()?;
        Ok(num * 1024 * 1024 * 1024)
    } else if size_str.ends_with("m") {
        let num: usize = size_str.trim_end_matches("m").parse()?;
        Ok(num * 1024 * 1024)
    } else if size_str.ends_with("k") {
        let num: usize = size_str.trim_end_matches("k").parse()?;
        Ok(num * 1024)
    } else {
        let num: usize = size_str.parse()?;
        Ok(num)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    
    let args = Args::parse();
    
    // Fork to background if requested (matching Go's TrzMain)
    if args.fork {
        if nrzsz_core::fork::fork_to_background()? {
            return Ok(());
        }
    }
    
    let dest_path = match &args.path {
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir()?,
    };
    
    if !dest_path.exists() {
        return Err(anyhow!("Not a directory: {}", dest_path.display()));
    }
    
    if !dest_path.is_dir() {
        return Err(anyhow!("Not a directory: {}", dest_path.display()));
    }
    
    let mut config = Config {
        quiet: args.quiet || args.fork,
        binary: args.binary && !args.ascii,
        directory: args.directory || args.recursive,
        overwrite: args.overwrite && !args.rename,
        compress: CompressType::parse(&args.compress),
        fork: if args.fork { Some(true) } else { None },
        ..Config::default()
    };
    
    // ASCII mode: force text mode (newline conversion) and disable binary
    if args.ascii {
        config.binary = false;
        config.newline = "\n".to_string();
    }
    
    // lrzsz-style conflict-handling flags
    config.protect = args.protect;
    config.append = args.append;
    config.overwrite_newer = args.overwrite_or_skip;
    if let Some(ws) = args.windowsize {
        config.window_size = Some(ws);
    }
    
    // Client-specified files to upload (matching Go's -F/--file -> client_files)
    if !args.file.is_empty() {
        config.client_files = args.file.clone();
    }
    
    
    if let Some(bufsize) = &args.bufsize {
        config.max_buf_size = parse_size(bufsize)?;
    }
    
    if let Some(timeout) = args.timeout {
        config.timeout = timeout;
    }
    
    if args.escape {
        config.escape_chars = get_escape_chars(true);
    }
    
    // Detect tmux
    let tmux_info = detect_tmux()?;
    if let Some(info) = &tmux_info {
        config.tmux_pane_width = info.pane_width;
        // In tmux normal mode, disable binary mode (nrzsz-go behavior)
        // and set tmux_output_junk
        if info.mode == TmuxMode::NormalMode {
            config.binary = false;
            config.tmux_output_junk = true;
            
            // Print tmux column cleanup sequence for small columns
            let columns = nrzsz_core::terminal::get_terminal_columns();
            if columns > 0 && columns < 40 {
                print!("\n\n\x1b[2A\x1b[0J");
            } else {
                print!("\n\x1b[1A\x1b[0J");
            }
            io::stdout().flush()?;
        }
    }
    
    // On Windows, disable binary mode (nrzsz-go behavior)
    #[cfg(windows)]
    {
        config.binary = false;
    }
    
    // Print trigger line for client detection
    let unique_id = nrzsz_core::protocol::generate_unique_id();
    let version = env!("CARGO_PKG_VERSION");
    
    // Start TCP listener for tunnel connection
    let listener = std::net::TcpListener::bind("0.0.0.0:0")?;
    let port = listener.local_addr()?.port();
    
    // Print trigger for client to connect via TCP tunnel
    // Format: \x1b[s::NRZSZ:TRANSFER:R:VERSION:UNIQUEID:PORT\r\n
    // UNIQUEID is zero-padded to 13 digits (matching nrzsz-go)
    let unique_id_num: u64 = unique_id.parse().unwrap_or(0);
    // Trigger mode (matching Go's TrzMain): F if client files specified, D if directory, else R
    let mode = if !args.file.is_empty() {
        'F'
    } else if config.directory {
        'D'
    } else {
        'R'
    };
    print!("\x1b[s::TRZSZ:TRANSFER:{}:{}:{:013}:{}\r\n", mode, version, unique_id_num, port);
    use std::io::Write as _;
    io::stdout().flush()?;
    
    // Accept TCP connection from client (blocks until client connects)
    let (mut tcp_stream, _) = listener.accept()?;
    
    // Perform the tunnel handshake (verify client hello, respond with server hello)
    nrzsz_core::tunnel::server_tunnel_handshake(&mut tcp_stream, &unique_id, port)?;
    
    // Use TCP stream for stdin/stdout communication
    let stdin_pipe = tcp_stream.try_clone()?;
    let stdout_pipe = tcp_stream.try_clone()?;
    
    let mut session = TransferSession::new(
        config.clone(),
        Box::new(stdin_pipe),
        Box::new(stdout_pipe),
    );
    
    // Set up signal handling (matching Go's handleServerSignal: stop, keep files)
    let stopped = Arc::new(AtomicBool::new(false));
    let stopped_clone = stopped.clone();
    
    tokio::spawn(async move {
        // Handle SIGINT (Ctrl+C) and SIGTERM, matching Go's handleServerSignal:
        // stop the transfer but keep files already received (no interactive delete
        // prompt — that path blocks the async runtime and diverges from Go).
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
            let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
            tokio::select! {
                _ = sigint.recv() => {}
                _ = sigterm.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            signal::ctrl_c().await.ok();
        }
        
        // Stop the transfer; received files are kept on disk.
        stopped_clone.store(true, Ordering::SeqCst);
        nrzsz_core::transfer::set_stopped(true);
    });
    
    // Receive action from client (nrzsz-go: recvAction first, no sendAction back)
    let action = session.recv_action()?;
    
    // Check client capabilities (matching nrzsz-go behavior)
    if args.binary && !action.support_binary.unwrap_or(true) {
        return Err(anyhow!("Binary mode not supported by client"));
    }
    if args.fork && !action.fork.unwrap_or(false) {
        return Err(anyhow!("Fork mode not supported by client"));
    }
    if args.directory && !action.support_dir {
        return Err(anyhow!("Directory mode not supported by client"));
    }
    if !action.confirm {
        session.server_exit("Cancelled")?;
        return Ok(());
    }
    
    // Send config to server
    session.send_config(&action)?;
    
    // Receive files
    let local_list = session.recv_files(&dest_path, args.quiet)?;
    
    
    // Print results
    for file_name in &local_list {
        let file_path = dest_path.join(file_name);
        if file_path.exists() {
            let size = std::fs::metadata(&file_path)?.len();
            nrzsz_core::progress::print_transfer_result(file_name, true, size)?;
        }
    }
    
    // Exit
    let msg = format!("Saved {} {} to {}", 
        local_list.len(), 
        if local_list.len() > 1 { "files" } else { "file" },
        dest_path.display());
    session.client_exit(&msg)?;
    
    Ok(())
}