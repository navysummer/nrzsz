use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use anyhow::Result;
use clap::{Parser, ValueHint};
use tokio::signal;

use nlzsz_core::protocol::{Config, CompressType};
use nlzsz_core::transfer::TransferSession;
use nlzsz_core::utils::*;
use nlzsz_core::tmux::*;
use nlzsz_core::progress::print_transfer_result;

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
            eprint!("\nDo you want to stop and delete the transferred files? (y/N): ");
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
    
    // Send action (without knowing remote OS yet)
    session.send_action(true, "0.1.0", false)?;
    
    // Receive action
    let action = session.recv_action()?;
    
    // Detect remote OS from action
    let remote_is_windows = action.newline.as_deref() == Some("!\n");
    
    // Re-send action with correct remote OS info
    session.send_action(true, "0.1.0", remote_is_windows)?;
    
    // Send config
    session.send_config(&action)?;
    
    // Receive config
    let _remote_config = session.recv_config()?;
    
    // Check paths readable
    let file_list = nlzsz_core::transfer::check_paths_readable(&args.files, args.directory || args.recursive)?;
    
    // Send files
    let remote_list = session.send_files(&file_list, args.quiet)?;
    
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
    
    // Exit
    let msg = format!("Saved {} {} to {}", 
        remote_list.len(), 
        if remote_list.len() > 1 { "files" } else { "file" },
        remote_list.join(", "));
    session.client_exit(&msg)?;
    
    Ok(())
}