use std::path::{Path, PathBuf};
use anyhow::{Result, anyhow};

use crate::protocol::Config;
use crate::transfer::TransferSession;

/// Options for creating a filter or relay.
pub struct NrzszOptions {
    pub compress: Option<String>,
    pub overwrite: bool,
    pub binary: bool,
    pub escape: bool,
    pub directory: bool,
    pub fork: bool,
    pub bufsize: Option<String>,
    pub timeout: Option<u32>,
    pub quiet: bool,
    pub relay: bool,
    pub tracelog: bool,
    pub dragfile: bool,
    pub zmodem: bool,
    pub osc52: bool,
}

impl Default for NrzszOptions {
    fn default() -> Self {
        Self {
            compress: Some("auto".to_string()),
            overwrite: false,
            binary: false,
            escape: false,
            directory: false,
            fork: false,
            bufsize: Some("10M".to_string()),
            timeout: Some(20),
            quiet: false,
            relay: false,
            tracelog: false,
            dragfile: false,
            zmodem: false,
            osc52: false,
        }
    }
}

impl NrzszOptions {
    pub fn to_config(&self) -> Config {
        let compress = match self.compress.as_deref() {
            Some("yes") => crate::protocol::CompressType::Yes,
            Some("no") => crate::protocol::CompressType::No,
            _ => crate::protocol::CompressType::Auto,
        };
        let timeout = self.timeout.unwrap_or(20);
        let max_buf_size = self.bufsize.as_ref()
            .and_then(|b| parse_size(b))
            .unwrap_or(10 * 1024 * 1024) as usize;
        Config {
            quiet: self.quiet,
            binary: self.binary,
            overwrite: self.overwrite,
            directory: self.directory,
            fork: Some(self.fork),
            compress,
            timeout,
            max_buf_size,
            ..Config::default()
        }
    }
}

fn parse_size(s: &str) -> Option<i64> {
    let s = s.trim().to_lowercase();
    let (num, unit) = if let Some(pos) = s.find(|c: char| c.is_alphabetic()) {
        (&s[..pos], &s[pos..])
    } else {
        (s.as_str(), "")
    };
    let n: i64 = num.parse().ok()?;
    Some(match unit {
        "b" | "" => n,
        "k" | "kb" => n * 1024,
        "m" | "mb" => n * 1024 * 1024,
        "g" | "gb" => n * 1024 * 1024 * 1024,
        _ => n,
    })
}

#[derive(Debug, Clone)]
pub enum TransferState {
    Starting { total_files: usize },
    Transferring { current_file: String, progress: f64 },
    Completed { total_files: usize },
    Error { message: String },
}

pub type TransferStateCallback = Box<dyn Fn(TransferState) + Send + Sync>;
pub type RedrawScreenCallback = Box<dyn Fn() + Send + Sync>;

pub struct TransferStateHandler {
    callback: Option<TransferStateCallback>,
}

impl TransferStateHandler {
    pub fn new() -> Self { Self { callback: None } }
    pub fn set_callback(&mut self, cb: TransferStateCallback) { self.callback = Some(cb); }
    pub fn notify(&self, state: TransferState) {
        if let Some(ref cb) = self.callback { cb(state); }
    }
}

impl Default for TransferStateHandler {
    fn default() -> Self { Self::new() }
}

/// Upload files to a remote server via TCP tunnel.
pub fn upload_files(host: &str, port: u16, files: &[PathBuf], options: NrzszOptions) -> Result<()> {
    use std::io::Write;
    let config = options.to_config();
    let mut stream = std::net::TcpStream::connect((host, port))?;
    let unique_id = format!("{:013}", std::process::id() as u64);
    let uid_prefix = &unique_id[..unique_id.len()-2];
    let client_hello = format!("::NRZSZ::CLIENT::HELLO::{}:{}", uid_prefix, port);
    let server_hello = format!("::NRZSZ::SERVER::HELLO::{}:{}", uid_prefix, port);

    stream.write_all(client_hello.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut buf = [0u8; 200];
    use std::io::Read;
    let n = stream.read(&mut buf)?;
    let response = String::from_utf8_lossy(&buf[..n]).trim().to_string();
    if response != server_hello {
        return Err(anyhow!("Tunnel handshake failed: {:?}", response));
    }

    let stdin_pipe: Box<dyn std::io::Read + Send + 'static> = Box::new(stream.try_clone()?);
    let stdout_pipe: Box<dyn std::io::Write + 'static> = Box::new(stream);
    let mut session = TransferSession::new(config, stdin_pipe, stdout_pipe);

    let version_str = "1.2.0";
    session.send_action(true, version_str, false)?;
    let remote_config = session.recv_config()?;

    let file_list = crate::transfer::check_paths_readable(
        &files.iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>(),
        options.directory,
    )?;
    if remote_config.overwrite {
        TransferSession::check_duplicate_names(&file_list)?;
    }

    let quiet = remote_config.quiet;
    let _remote_names = session.send_files(&file_list, quiet)?;
    session.recv_exit()?;
    Ok(())
}

/// Download files from a remote server via TCP tunnel.
pub fn download_files(host: &str, port: u16, dest: &Path, options: NrzszOptions) -> Result<()> {
    use std::io::Write;
    let config = options.to_config();
    let mut stream = std::net::TcpStream::connect((host, port))?;
    let unique_id = format!("{:013}", std::process::id() as u64);
    let uid_prefix = &unique_id[..unique_id.len()-2];
    let client_hello = format!("::NRZSZ::CLIENT::HELLO::{}:{}", uid_prefix, port);
    let server_hello = format!("::NRZSZ::SERVER::HELLO::{}:{}", uid_prefix, port);

    stream.write_all(client_hello.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut buf = [0u8; 200];
    use std::io::Read;
    let n = stream.read(&mut buf)?;
    let response = String::from_utf8_lossy(&buf[..n]).trim().to_string();
    if response != server_hello {
        return Err(anyhow!("Tunnel handshake failed: {:?}", response));
    }

    let stdin_pipe: Box<dyn std::io::Read + Send + 'static> = Box::new(stream.try_clone()?);
    let stdout_pipe: Box<dyn std::io::Write + 'static> = Box::new(stream);
    let mut session = TransferSession::new(config, stdin_pipe, stdout_pipe);

    let version_str = "1.2.0";
    session.send_action(true, version_str, false)?;
    let remote_config = session.recv_config()?;

    let quiet = remote_config.quiet;
    let local_names = session.recv_files(dest, quiet)?;
    let msg = TransferSession::format_saved_files(local_names.len(), dest, &local_names);
    session.client_exit(&msg)?;
    Ok(())
}

/// Start a relay server bridging client and server streams.
pub fn start_relay(
    client_in: Box<dyn std::io::Read + Send>,
    client_out: Box<dyn std::io::Write + Send>,
    server_in: Box<dyn std::io::Write + Send>,
    server_out: Box<dyn std::io::Read + Send>,
) -> Result<()> {
    let mut relay = crate::relay::NrzszRelay::new(client_in, client_out, server_in, server_out);
    relay.set_tunnel_connector(|port: u16| {
        std::net::TcpStream::connect(("127.0.0.1", port))
            .map_err(|e| anyhow!("Relay tunnel connect to {} failed: {}", port, e))
    });
    relay.run()
}

pub fn one_time_upload(host: &str, port: u16, files: Vec<PathBuf>, options: NrzszOptions) -> Result<()> {
    upload_files(host, port, &files, options)
}

pub fn one_time_download(host: &str, port: u16, dest: PathBuf, options: NrzszOptions) -> Result<()> {
    download_files(host, port, &dest, options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nrzsz_options() {
        let options = NrzszOptions::default();
        assert_eq!(options.compress, Some("auto".to_string()));
        assert!(!options.overwrite);
    }

    #[test]
    fn test_options_to_config() {
        let options = NrzszOptions {
            quiet: true,
            binary: true,
            overwrite: true,
            compress: Some("yes".to_string()),
            timeout: Some(30),
            ..Default::default()
        };
        let config = options.to_config();
        assert!(config.quiet);
        assert!(config.binary);
        assert!(config.overwrite);
        assert_eq!(config.compress, crate::protocol::CompressType::Yes);
        assert_eq!(config.timeout, 30);
    }

    #[test]
    fn test_parse_size() {
        assert_eq!(parse_size("10M"), Some(10 * 1024 * 1024));
        assert_eq!(parse_size("1K"), Some(1024));
        assert_eq!(parse_size("1G"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size("1024"), Some(1024));
    }
}
