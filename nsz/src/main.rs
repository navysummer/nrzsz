use std::io;
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
use nrzsz_core::progress::print_transfer_result;

#[derive(Parser)]
#[command(name = "nsz")]
#[command(about = "Send file(s), similar to sz and compatible with tmux.")]
#[command(version)]
struct Args {
    /// File(s) to be sent
    #[arg(value_hint = ValueHint::FilePath)]
    files: Vec<String>,
    
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
    
    /// Resume interrupted file transfer (not yet supported by core)
    #[arg(long)]
    resume: bool,
    
    /// Protect existing files: skip files that already exist (not yet supported by core)
    #[arg(short = 'p', long)]
    protect: bool,
    
    /// Window size (N bytes) for transfer (not yet supported by core)
    #[arg(short = 'w', long = "windowsize")]
    windowsize: Option<u32>,
    
    /// Overwrite if source is newer, otherwise skip (approximated as overwrite)
    #[arg(short = 'Y', long = "overwrite-or-skip")]
    overwrite_or_skip: bool,
    
    /// Force rename if file already exists
    #[arg(short = 'E', long)]
    rename: bool,
    
    /// Append to existing files (not yet supported by core)
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
    
    if args.files.is_empty() {
        eprintln!("nsz: no files specified");
        std::process::exit(1);
    }
    
    // Fork to background if requested (matching Go's TszMain)
    if args.fork {
        if nrzsz_core::fork::fork_to_background()? {
            return Ok(());
        }
    }
    
    
    let mut config = Config {
        quiet: args.quiet || args.fork,
        binary: args.binary && !args.ascii,
        directory: args.directory || args.recursive,
        overwrite: (args.overwrite || args.overwrite_or_skip) && !args.rename,
        compress: CompressType::parse(&args.compress),
        fork: if args.fork { Some(true) } else { None },
        ..Config::default()
    };
    
    // ASCII mode: force text mode (newline conversion) and disable binary
    if args.ascii {
        config.binary = false;
        config.newline = "\n".to_string();
    }
    
    // Flags that need core support not yet present
    if args.protect {
        eprintln!("nsz: warning: -p/--protect is not yet supported by nrzsz-core");
    }
    if args.windowsize.is_some() {
        eprintln!("nsz: warning: -w/--windowsize is not yet supported by nrzsz-core");
    }
    if args.append {
        eprintln!("nsz: warning: -+/--append is not yet supported by nrzsz-core");
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
    // Format: \x1b[s::NRZSZ:TRANSFER:S:VERSION:UNIQUEID:PORT\r\n
    // UNIQUEID is zero-padded to 13 digits (matching nrzsz-go)
    let unique_id_num: u64 = unique_id.parse().unwrap_or(0);
    print!("\x1b[s::TRZSZ:TRANSFER:S:{}:{:013}:{}\r\n", version, unique_id_num, port);
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
    
    // Set up signal handling
    let stopped = Arc::new(AtomicBool::new(false));
    let stopped_clone = stopped.clone();
    
    tokio::spawn(async move {
        // Handle SIGINT (Ctrl+C) and SIGTERM (kill), matching Go's handleServerSignal
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
        // Stop the transfer
        stopped_clone.store(true, Ordering::SeqCst);
        nrzsz_core::transfer::set_stopped(true);
    });
    
    // Receive action from client (nrzsz-go: recvAction first, no sendAction back)
    let action = session.recv_action()?;
    
    // Check client capabilities (matching nrzsz-go behavior)
    if args.binary && !action.support_binary.unwrap_or(true) {
        return Err(anyhow!("Binary mode not supported by client"));
    }
    if args.directory && !action.support_dir {
        return Err(anyhow!("Directory mode not supported by client"));
    }
    if !action.confirm {
        session.server_exit("Cancelled")?;
        return Ok(());
    }
    
    // Send config to client
    session.send_config(&action)?;
    
    // Check paths readable
    let file_list = nrzsz_core::transfer::check_paths_readable(&args.files, args.directory || args.recursive)?;
    
    // Send files
    let _remote_list = session.send_files(&file_list, args.quiet)?;
    
    // Print results
    for file in &file_list {
        if !file.is_dir {
            let size = std::fs::metadata(&file.abs_path)?.len();
            print_transfer_result(
                file.path_name.last().unwrap(),
                true,
                size,
            )?;
        }
    }
    
    // Exit - receive EXIT summary from the remote (nrz)
    session.recv_exit()?;
    
    Ok(())
}