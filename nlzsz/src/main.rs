use std::env;
use std::process::{Command, Stdio};
use anyhow::Result;
use clap::Parser;
use nlzsz_core::config::NlzszConfig;

#[derive(Parser, Debug)]
#[command(name = "nlzsz", about = "nlzsz compatible wrapper for file transfer")]
struct Args {
    /// Show help message
    #[arg(short, long)]
    help: bool,

    /// Show version number
    #[arg(short, long)]
    version: bool,

    /// Run as a nlzsz relay server
    #[arg(short, long)]
    relay: bool,

    /// Enable trace log for debugging
    #[arg(short = 't', long)]
    tracelog: bool,

    /// Enable drag file(s) to upload
    #[arg(short = 'd', long)]
    dragfile: bool,

    /// Enable zmodem lrzsz (rz / sz)
    #[arg(short = 'z', long)]
    zmodem: bool,

    /// Enable clipboard integration via OSC52
    #[arg(short = 'o', long)]
    osc52: bool,

    /// Command line to execute
    command: Vec<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.help {
        print_help();
        return Ok(());
    }

    if args.version {
        println!("nlzsz {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // Initialize logging if trace log is enabled
    if args.tracelog {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug"))
            .init();
    }

    // Load configuration
    let config = NlzszConfig::load().unwrap_or_default();

    // Set environment variables based on args
    if args.relay {
        env::set_var("NLZSZ_RELAY", "1");
    }
    if args.dragfile {
        env::set_var("NLZSZ_DRAGFILE", "1");
    }
    if args.zmodem {
        env::set_var("NLZSZ_ZMODEM", "1");
    }
    if args.osc52 {
        env::set_var("NLZSZ_OSC52", "1");
    }

    // If no command provided, show help
    if args.command.is_empty() {
        print_help();
        return Ok(());
    }

    // Execute the wrapped command
    let mut cmd = Command::new(&args.command[0]);
    if args.command.len() > 1 {
        cmd.args(&args.command[1..]);
    }

    // Set environment for nlzsz detection
    cmd.env("NLZSZ", "1");
    cmd.env("NLZSZ_CWD", env::current_dir()?);

    // Set default paths from config
    if !config.default_upload_path.is_empty() {
        cmd.env("NLZSZ_DEFAULT_UPLOAD_PATH", &config.default_upload_path);
    }
    if !config.default_download_path.is_empty() {
        cmd.env("NLZSZ_DEFAULT_DOWNLOAD_PATH", &config.default_download_path);
    }

    let status = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    std::process::exit(status.code().unwrap_or(1));
}

fn print_help() {
    println!("usage: nlzsz [-h] [-v] [-r] [-t] [-d] [-z] [-o] command line");
    println!();
    println!("Wrapping command line to support nlzsz (nsz / nrz).");
    println!();
    println!("positional arguments:");
    println!("  command line       the original command line");
    println!();
    println!("optional arguments:");
    println!("  -h, --help         show this help message and exit");
    println!("  -v, --version      show version number and exit");
    println!("  -r, --relay        run as a nlzsz relay server");
    println!("  -t, --tracelog     enable trace log for debugging");
    println!("  -d, --dragfile     enable drag file(s) to upload");
    println!("  -z, --zmodem       enable zmodem lrzsz (rz / sz)");
    println!("  -o, --osc52        enable clipboard integration");
}
