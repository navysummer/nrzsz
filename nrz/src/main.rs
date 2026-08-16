use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use anyhow::{Result, anyhow};
use clap::{Parser, ValueHint};
use tokio::signal;

use nlzsz_core::protocol::{Config, CompressType};
use nlzsz_core::transfer::TransferSession;
use nlzsz_core::utils::*;
use nlzsz_core::tmux::*;

#[derive(Parser)]
#[command(name = "nrz")]
#[command(about = "Receive file(s), similar to rz and compatible with tmux.")]
#[command(version)]
struct Args {
    /// Path to save file(s). Default: current directory
    #[arg(value_hint = ValueHint::DirPath)]
    path: Option<String>,
    
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
    
    let mut config = Config::default();
    config.quiet = args.quiet || args.fork;
    config.binary = args.binary;
    config.directory = args.directory || args.recursive;
    config.overwrite = args.overwrite;
    config.compress = CompressType::parse(&args.compress);
    config.fork = args.fork;
    
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
        // In tmux normal mode, force binary mode
        if info.mode == TmuxMode::NormalMode {
            config.binary = true;
        }
    }
    
    // On Windows, force binary mode
    #[cfg(windows)]
    {
        config.binary = true;
    }
    
    let stdin = io::stdin();
    let stdout = io::stdout();
    
    let mut session = TransferSession::new(
        config.clone(),
        Box::new(stdin.lock()),
        Box::new(stdout.lock()),
    );
    
    // Set up signal handling
    let stopped = Arc::new(AtomicBool::new(false));
    let stopped_clone = stopped.clone();
    let created_files_clone = Arc::new(std::sync::Mutex::new(Vec::<std::path::PathBuf>::new()));
    let created_files_for_signal = created_files_clone.clone();
    
    tokio::spawn(async move {
        signal::ctrl_c().await.ok();
        
        // Show stop confirmation dialog
        #[cfg(unix)]
        {
            use std::io::{self, Write};
            eprint!("\nDo you want to stop and delete the received files? (y/N): ");
            io::stderr().flush().ok();
            
            let mut input = String::new();
            if io::stdin().read_line(&mut input).is_ok() {
                let input = input.trim().to_lowercase();
                if input == "y" || input == "yes" {
                    // Delete created files
                    if let Ok(files) = created_files_for_signal.lock() {
                        for file in files.iter().rev() {
                            let _ = std::fs::remove_file(file);
                        }
                    }
                }
            }
        }
        
        stopped_clone.store(true, Ordering::SeqCst);
        nlzsz_core::transfer::set_stopped(true);
    });
    
    // Receive action
    let action = session.recv_action()?;
    
    // Detect remote OS from action
    let remote_is_windows = action.newline.as_deref() == Some("!\n");
    
    // Send action with correct remote OS info
    session.send_action(true, "0.1.0", remote_is_windows)?;
    
    // Receive config
    let _remote_config = session.recv_config()?;
    
    // Send config
    session.send_config(&action)?;
    
    // Receive files
    let local_list = session.recv_files(&dest_path, args.quiet)?;
    
    // Print results
    for file_name in &local_list {
        let file_path = dest_path.join(file_name);
        if file_path.exists() {
            let size = std::fs::metadata(&file_path)?.len();
            nlzsz_core::progress::print_transfer_result(file_name, true, size)?;
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