use std::io::{Read, Write};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicI64, Ordering};
use std::sync::Arc;
use anyhow::{Result, anyhow};

use crate::protocol::{Config, FileInfo, DirectoryInfo, PROTOCOL_VERSION};
use crate::utils::*;
use crate::progress::TransferProgress;
use crate::buffer::{NrzszBuffer, BufferReader, start_stdin_reader};

static STOPPED: AtomicBool = AtomicBool::new(false);

pub fn set_stopped(stopped: bool) {
    STOPPED.store(stopped, Ordering::SeqCst);
}

pub fn is_stopped() -> bool {
    STOPPED.load(Ordering::SeqCst)
}

pub struct TransferSession {
    config: Config,
    stdin: Box<dyn Read>,
    stdout: Box<dyn Write>,
    _buffer: Arc<NrzszBuffer>,
    created_files: Vec<PathBuf>,
    stopped: Arc<AtomicBool>,
    stop_and_delete: Arc<AtomicBool>,
    pausing: Arc<AtomicBool>,
    pause_idx: Arc<AtomicU32>,
    pause_begin_time: Arc<AtomicI64>,
    term_reseted: Arc<AtomicBool>,
    last_chunk_times: Vec<std::time::Duration>,
    #[cfg(unix)]
    original_tmux_interval: Option<String>,
    negotiated_protocol: u32,
    remote_capabilities: Option<crate::protocol::ClientCapabilities>,
    // Map of source path_id -> chosen local top-level name, so files from the same
    // source directory share one destination dir (matching Go's fileNameMap).
    file_name_map: std::collections::HashMap<u32, String>,
}

/// Shared stop control for a transfer session. The wrapper (SIGINT, tmux-CC
/// Ctrl+C) holds a clone and can stop a running session in place: set the
/// stopped / stop-and-delete flags and wake any thread blocked on a protocol
/// read (matching Go's `trzszTransfer.stopTransferringFiles(stopAndDelete)`,
/// which the filter reaches via `filter.StopTransferringFiles`).
pub struct TransferStop {
    stopped: Arc<AtomicBool>,
    stop_and_delete: Arc<AtomicBool>,
    buffer: Arc<NrzszBuffer>,
}

impl TransferStop {
    pub fn new() -> Arc<Self> {
        let buffer = Arc::new(NrzszBuffer::new());
        Arc::new(Self {
            stopped: Arc::new(AtomicBool::new(false)),
            stop_and_delete: Arc::new(AtomicBool::new(false)),
            buffer,
        })
    }

    pub fn stop(&self, stop_and_delete: bool) {
        if self.stopped.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return;
        }
        self.stop_and_delete.store(stop_and_delete, Ordering::SeqCst);
        self.buffer.stop_buffer();
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst) || is_stopped()
    }

    pub fn is_stop_and_delete(&self) -> bool {
        self.stop_and_delete.load(Ordering::SeqCst)
    }

    pub fn buffer(&self) -> &Arc<NrzszBuffer> {
        &self.buffer
    }


    /// The shared stopped flag (same Arc the session checks in `is_stopped`/`check_stop`).
    pub fn stopped_flag(&self) -> Arc<AtomicBool> {
        self.stopped.clone()
    }

    /// The shared stop-and-delete flag.
    pub fn stop_and_delete_flag(&self) -> Arc<AtomicBool> {
        self.stop_and_delete.clone()
    }
}

impl Default for TransferStop {
    fn default() -> Self {
        Self {
            stopped: Arc::new(AtomicBool::new(false)),
            stop_and_delete: Arc::new(AtomicBool::new(false)),
            buffer: Arc::new(NrzszBuffer::new()),
        }
    }
}

impl TransferSession {
    pub fn new(config: Config, stdin: Box<dyn Read + Send + 'static>, stdout: Box<dyn Write>) -> Self {
        Self::new_with_stop(config, stdin, stdout, TransferStop::new())
    }

    /// Build a session that shares its stop/buffer state with `stop`, so the
    /// wrapper (SIGINT / tmux-CC Ctrl+C) can stop a running session in place by
    /// calling `stop.stop(...)` on the same handle (Go's StopTransferringFiles).
    pub fn new_with_stop(config: Config, stdin: Box<dyn Read + Send + 'static>, stdout: Box<dyn Write>, stop: Arc<TransferStop>) -> Self {
        let buffer = stop.buffer().clone();
        buffer.set_new_timeout(std::time::Duration::from_secs(config.timeout as u64));
        let buffer_reader = Box::new(BufferReader::new(buffer.clone()));
        start_stdin_reader(stdin, buffer.clone());
        let stopped = stop.stopped_flag();
        let stop_and_delete = stop.stop_and_delete_flag();
        Self {
            config,
            stdin: buffer_reader,
            stdout,
            _buffer: buffer,
            created_files: Vec::new(),
            stopped,
            stop_and_delete,
            pausing: Arc::new(AtomicBool::new(false)),
            pause_idx: Arc::new(AtomicU32::new(0)),
            pause_begin_time: Arc::new(AtomicI64::new(0)),
            term_reseted: Arc::new(AtomicBool::new(false)),
            last_chunk_times: Vec::with_capacity(10),
            #[cfg(unix)]
            original_tmux_interval: None,
            negotiated_protocol: PROTOCOL_VERSION,
            remote_capabilities: None,
            file_name_map: std::collections::HashMap::new(),
        }
    }
    
    pub fn set_stopped(&self, stopped: bool) {
        self.stopped.store(stopped, Ordering::SeqCst);
        set_stopped(stopped);
    }
    
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst) || is_stopped()
    }
    
    pub fn check_stop(&self) -> Result<()> {
        if self.stop_and_delete.load(Ordering::SeqCst) {
            return Err(anyhow!("Transfer stopped and delete"));
        }
        if self.stopped.load(Ordering::SeqCst) || is_stopped() {
            return Err(anyhow!("Transfer stopped"));
        }
        Ok(())
    }
    
    pub fn is_pausing(&self) -> bool {
        self.pausing.load(Ordering::SeqCst)
    }
    
    pub fn pause_transferring_files(&self) {
        self.pausing.store(true, Ordering::SeqCst);
        if self.pause_begin_time.load(Ordering::SeqCst) == 0 {
            self.pause_idx.fetch_add(1, Ordering::SeqCst);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            self.pause_begin_time.compare_exchange(0, now, Ordering::SeqCst, Ordering::SeqCst).ok();
        }
    }
    
    pub fn resume_transferring_files(&self) {
        self.pause_begin_time.store(0, Ordering::SeqCst);
        self.pausing.store(false, Ordering::SeqCst);
        // Note: caller should reset buffer timeout if applicable
    }
    
    pub fn stop_transferring_files(&self, stop_and_delete: bool) {
        if self.stopped.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return;
        }
        self.stop_and_delete.store(stop_and_delete, Ordering::SeqCst);
        // Wake any thread blocked on a protocol read (Go's stopBuffer).
        self._buffer.stop_buffer();
    }
    
    pub fn is_stop_and_delete(&self) -> bool {
        self.stop_and_delete.load(Ordering::SeqCst)
    }
    pub fn get_pause_idx(&self) -> u32 {
        self.pause_idx.load(Ordering::SeqCst)
    }
    
    /// Record the time taken for a data chunk transfer
    pub fn set_last_chunk_time(&mut self, duration: std::time::Duration) {
        if self.last_chunk_times.len() >= 10 {
            self.last_chunk_times.remove(0);
        }
        self.last_chunk_times.push(duration);
    }
    
    /// Get the maximum chunk time from the last N chunks (for adaptive timeout)
    pub fn get_max_chunk_time(&self) -> std::time::Duration {
        self.last_chunk_times.iter().copied().max().unwrap_or(std::time::Duration::from_millis(500))
    }
    
    /// V3 checkStopAndPause: spin while paused, sending pause markers, then check stop
    pub fn check_stop_and_pause(&mut self, typ: &str) -> Result<()> {
        if self.negotiated_protocol >= crate::protocol::PROTOCOL_V3 {
            while self.is_pausing() {
                self.check_stop()?;
                // Send pause marker: #DATA:=\n
                let marker = format!("#{}:={}", typ, self.config.newline);
                self.stdout.write_all(marker.as_bytes())?;
                self.stdout.flush()?;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        self.check_stop()
    }
    
    pub fn get_created_files(&self) -> &[PathBuf] {
        &self.created_files
    }
    
    #[cfg(unix)]
    pub fn save_tmux_status_interval(&mut self) {
        if let Ok(interval) = crate::tmux::get_tmux_status_interval() {
            self.original_tmux_interval = Some(interval);
            // Set status interval to 0 during transfer to prevent tmux from refreshing
            let _ = crate::tmux::set_tmux_status_interval("0");
        }
    }
    
    #[cfg(unix)]
    pub fn restore_tmux_status_interval(&mut self) {
        if let Some(interval) = &self.original_tmux_interval {
            let _ = crate::tmux::set_tmux_status_interval(interval);
            self.original_tmux_interval = None;
        }
    }
    
    pub fn get_negotiated_protocol(&self) -> u32 {
        self.negotiated_protocol
    }
    
    pub fn get_remote_capabilities(&self) -> Option<&crate::protocol::ClientCapabilities> {
        self.remote_capabilities.as_ref()
    }
    
    pub fn can_use_binary(&self) -> bool {
        self.config.binary && self.remote_capabilities.as_ref()
            .is_none_or(|caps| caps.support_binary)
    }
    
    pub fn can_use_compress(&self) -> bool {
        self.remote_capabilities.as_ref()
            .is_none_or(|caps| caps.support_compress)
    }
    
    pub fn send_action(&mut self, confirm: bool, version: &str, remote_is_windows: bool, tunnel_connected: bool) -> Result<()> {
        let action = crate::protocol::Action {
            lang: "rust".to_string(),
            confirm,
            version: version.to_string(),
            support_dir: true,
            protocol: PROTOCOL_VERSION,
            newline: "\n".to_string(),
            // Go: TunnelConnected is advertised only when the tunnel is connected.
            tunnel: Some(tunnel_connected),
            // Go: SupportFork is advertised only when the tunnel is connected.
            fork: Some(tunnel_connected),
            tmuxcc: None,
            support_binary: Some(true),
        };
        
        
        let mut action = action;
        
        // Disable binary for Windows
        #[cfg(windows)]
        {
            action.support_binary = Some(false);
        }
        
        // Disable binary for Windows remote
        if remote_is_windows {
            action.newline = "!\n".to_string();
            action.support_binary = Some(false);
        }
        
        // Disable binary for tmux normal mode (no tunnel)
        #[cfg(unix)]
        {
            if let Ok(Some(tmux_info)) = crate::tmux::detect_tmux() {
                if tmux_info.mode == crate::tmux::TmuxMode::NormalMode {
                    action.support_binary = Some(false);
                }
            }
        }
        
        let action_json = serde_json::to_string(&action)?;
        send_string(&mut self.stdout, "ACT", &action_json, &self.config.newline)?;
        Ok(())
    }
    
    pub fn recv_action(&mut self) -> Result<crate::protocol::Action> {
        let mut buf = Vec::new();
        let action_str = if self.config.tmux_output_junk {
            recv_string_may_has_junk(&mut self.stdin, "ACT", &mut buf, &self.config.newline)?
        } else {
            recv_string(&mut self.stdin, "ACT", &mut buf, &self.config.newline)?
        };
        let action: crate::protocol::Action = serde_json::from_str(&action_str)?;
        
        // Update newline from action
        self.config.newline = action.newline.clone();
        // Apply negotiated timeout to the receive buffer
        self._buffer.set_new_timeout(std::time::Duration::from_secs(self.config.timeout as u64));
        // Enable VT100 stripping for Windows protocol (newline = "!\n")
        self._buffer.set_strip_vt100(self.config.newline == "!\n");
        
        // Negotiate protocol version
        self.negotiated_protocol = crate::protocol::negotiate_protocol_version(
            PROTOCOL_VERSION,
            action.protocol,
        );
        
        // Extract client capabilities
        self.remote_capabilities = Some(crate::protocol::ClientCapabilities::from_action(&action));
        
        Ok(action)
    }
    
    pub fn send_config(&mut self, action: &crate::protocol::Action) -> Result<()> {
        let remote_caps = crate::protocol::ClientCapabilities::from_action(action);
        
        // Build config map (matching nrzsz-go's map-based approach)
        let mut cfg_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
        cfg_map.insert("lang".to_string(), serde_json::Value::String("rust".to_string()));
        
        if self.config.quiet {
            cfg_map.insert("quiet".to_string(), serde_json::Value::Bool(true));
        }
        
        // Binary mode: only when tunnel connected OR binary mode enabled
        // escape_chars only when binary mode enabled and not tunnel connected
        let use_tunnel = action.tunnel.unwrap_or(false);
        if use_tunnel {
            cfg_map.insert("binary".to_string(), serde_json::Value::Bool(true));
        } else if remote_caps.support_binary && self.config.binary {
            cfg_map.insert("binary".to_string(), serde_json::Value::Bool(true));
            cfg_map.insert("escape_chars".to_string(), serde_json::to_value(&self.config.escape_chars)?);
        }
        
        if self.config.directory && remote_caps.support_directory {
            cfg_map.insert("directory".to_string(), serde_json::Value::Bool(true));
        }
        
        cfg_map.insert("bufsize".to_string(), serde_json::Value::Number(serde_json::Number::from(self.config.max_buf_size as u64)));
        cfg_map.insert("timeout".to_string(), serde_json::Value::Number(serde_json::Number::from(self.config.timeout)));
        
        if self.config.overwrite {
            cfg_map.insert("overwrite".to_string(), serde_json::Value::Bool(true));
        }
        if self.config.protect {
            cfg_map.insert("protect".to_string(), serde_json::Value::Bool(true));
        }
        if self.config.append {
            cfg_map.insert("append".to_string(), serde_json::Value::Bool(true));
        }
        if self.config.overwrite_newer {
            cfg_map.insert("overwrite_newer".to_string(), serde_json::Value::Bool(true));
        }
        if let Some(ws) = self.config.window_size {
            cfg_map.insert("window_size".to_string(), serde_json::Value::Number(serde_json::Number::from(ws)));
        }
        
        
        // tmux_output_junk
        #[cfg(unix)]
        {
            if let Ok(Some(tmux_info)) = crate::tmux::detect_tmux() {
                if tmux_info.mode == crate::tmux::TmuxMode::NormalMode {
                    cfg_map.insert("tmux_output_junk".to_string(), serde_json::Value::Bool(true));
                }
                if tmux_info.pane_width > 0 {
                    cfg_map.insert("tmux_pane_width".to_string(), serde_json::Value::Number(serde_json::Number::from(tmux_info.pane_width)));
                }
            }
        }
        // If not in tmux, use terminal columns if tmux integration was requested
        if !cfg_map.contains_key("tmux_pane_width") && action.tmuxcc.unwrap_or(false) {
            let columns = crate::terminal::get_terminal_columns();
            if columns > 0 {
                cfg_map.insert("tmux_pane_width".to_string(), serde_json::Value::Number(serde_json::Number::from(columns)));
            }
        }
        
        // Protocol version
        if action.protocol > 0 {
            let proto = action.protocol.min(PROTOCOL_VERSION);
            cfg_map.insert("protocol".to_string(), serde_json::Value::Number(serde_json::Number::from(proto)));
            self.negotiated_protocol = proto;
        }
        
        // Compress: only send if not Auto
        if self.config.compress != crate::protocol::CompressType::Auto {
            let compress_val = match self.config.compress {
                crate::protocol::CompressType::Yes => 1,
                crate::protocol::CompressType::No => 2,
                crate::protocol::CompressType::Auto => 0,
            };
            cfg_map.insert("compress".to_string(), serde_json::Value::Number(serde_json::Number::from(compress_val)));
        }
        
        // Fork: only if tunnel connected and fork is set
        if use_tunnel && self.config.fork.unwrap_or(false) {
            cfg_map.insert("fork".to_string(), serde_json::Value::Bool(true));
        }
        
        // Client files (used for one-time auto-upload from the client)
        if !self.config.client_files.is_empty() {
            cfg_map.insert(
                "client_files".to_string(),
                serde_json::to_value(&self.config.client_files)?,
            );
        }
        
        let config_json = serde_json::to_string(&cfg_map)?;
        // The sender advertises its effective transfer config (e.g. binary
        // forced by the tunnel). Mirror Go's sendConfig, which unmarshals the
        // sent cfgMap back into transferConfig so the local side uses the same
        // settings it advertised.
        if let Ok(cfg) = serde_json::from_str::<Config>(&config_json) {
            let newline = self.config.newline.clone();
            self.config = cfg;
            self.config.newline = newline;
        }
        send_string(&mut self.stdout, "CFG", &config_json, &self.config.newline)?;
        Ok(())
    }
    
    pub fn recv_config(&mut self) -> Result<Config> {
        let mut buf = Vec::new();
        let config_str = if self.config.tmux_output_junk {
            recv_string_may_has_junk(&mut self.stdin, "CFG", &mut buf, &self.config.newline)?
        } else {
            recv_string(&mut self.stdin, "CFG", &mut buf, &self.config.newline)?
        };
        let config: Config = serde_json::from_str(&config_str)?;
        self.config = config.clone();
        // Apply configured timeout to the receive buffer
        self._buffer.set_new_timeout(std::time::Duration::from_secs(self.config.timeout as u64));
        // Enable VT100 stripping for Windows protocol (newline = "!\n")
        self._buffer.set_strip_vt100(self.config.newline == "!\n");
        Ok(config)
    }
    
    pub fn client_exit(&mut self, msg: &str) -> Result<()> {
        send_string(&mut self.stdout, "EXIT", msg, &self.config.newline)?;
        Ok(())
    }
    
    /// Receive EXIT message from the remote side (the side that saved files).
    /// The EXIT message is zlib+base64 encoded (matching Go's sendString).
    pub fn recv_exit(&mut self) -> Result<String> {
        let mut buf = Vec::new();
        let msg = recv_string(&mut self.stdin, "EXIT", &mut buf, &self.config.newline)?;
        self.server_exit(&msg)?;
        Ok(msg)
    }
    
    /// Drain any pending input from stdin before sending errors
    /// This prevents stale data from interfering with error message parsing
    pub fn clean_input(&mut self, timeout: std::time::Duration) -> Result<()> {
        self.set_stopped(true);
        self._buffer.drain_buffer();
        std::thread::sleep(timeout);
        Ok(())
    }
    
    pub fn server_exit(&mut self, msg: &str) -> Result<()> {
        // Only reset the terminal once (matching Go's termReseted.CompareAndSwap)
        if self.term_reseted.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return Ok(());
        }
        
        // Write to the real terminal (stdout), not the transfer stream
        let mut term_out = std::io::stdout();
        // Restore cursor + clear from cursor down (Go: \x1b[u\x1b[0J)
        term_out.write_all(b"\x1b[u\x1b[0J")?;
        term_out.write_all(msg.as_bytes())?;
        term_out.write_all(b"\r\n")?;
        // Show cursor
        term_out.write_all(b"\x1b[?25h")?;
        term_out.flush()?;
        // Refresh tmux client if in tmux normal mode
        if self.config.tmux_output_junk {
            let _ = std::process::Command::new("tmux")
                .arg("refresh-client")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
        Ok(())
    }
    
    
    /// Check if a received line is an error message (#fail:, #FAIL:, #EXIT:)
    /// Returns Ok(Some(error)) if error, Ok(None) if not an error
    pub fn check_line_for_error(&self, line: &str) -> Option<crate::error::NrzszError> {
        if !line.starts_with('#') {
            return None;
        }
        if let Some((msg, err_type)) = crate::error::parse_error_type(line) {
            let trace = err_type == crate::error::ErrorType::Fail && line.starts_with("#FAIL");
            Some(crate::error::NrzszError::new(msg, err_type, trace))
        } else {
            None
        }
    }
    
    /// Delete any files/directories created during the transfer
    /// Returns list of deleted file paths
    pub fn delete_created_files(&mut self) -> Result<Vec<String>> {
        let mut deleted = Vec::new();
        for path in self.created_files.iter().rev() {
            if path.exists() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(path, std::fs::Permissions::from_mode(0o777));
                }
                if path.is_dir() {
                    let result = fs::remove_dir_all(path);
                    if result.is_ok() {
                        deleted.push(path.display().to_string());
                    }
                } else {
                    let result = fs::remove_file(path);
                    if result.is_ok() {
                        deleted.push(path.display().to_string());
                    }
                }
            }
        }
        Ok(deleted)
    }
    
    /// Client error handling - sends error to server
    /// If trace is true, sends "FAIL" (with trace); otherwise sends "fail" (no trace)
    /// If stop_and_delete is true and error type is "fail", includes deleted files list
    pub fn client_error_with_delete(&mut self, ex: &anyhow::Error, trace: bool, stop_and_delete: bool) -> Result<()> {
        self.clean_input(std::time::Duration::from_secs(1))?;
        
        // Skip sending if remote already exited or failed
        if crate::error::is_remote_exit(ex) || crate::error::is_remote_fail(ex) {
            return Ok(());
        }
        
        let err_msg = ex.to_string();
        let err_type = if trace { "FAIL" } else { "fail" };
        
        if stop_and_delete && !trace {
            let deleted = self.delete_created_files()?;
            if !deleted.is_empty() {
                let msg = format!("{}:{}", err_msg, deleted.join(","));
                send_string(&mut self.stdout, "fail", &msg, &self.config.newline)?;
                return Ok(());
            }
        }
        
        send_string(&mut self.stdout, err_type, &err_msg, &self.config.newline)?;
        Ok(())
    }
    
    pub fn client_error(&mut self, ex: &anyhow::Error, trace: bool) -> Result<()> {
        self.clean_input(std::time::Duration::from_secs(1))?;
        
        // Skip sending if remote already exited or failed
        if crate::error::is_remote_exit(ex) || crate::error::is_remote_fail(ex) {
            return Ok(());
        }
        
        let err_msg = ex.to_string();
        let err_type = if trace { "FAIL" } else { "fail" };
        send_string(&mut self.stdout, err_type, &err_msg, &self.config.newline)?;
        Ok(())
    }
    
    pub fn server_error(&mut self, ex: &anyhow::Error, trace: bool) -> Result<()> {
        self.clean_input(std::time::Duration::from_secs(1))?;
        
        // Delete files on stop-and-delete
        if crate::error::is_stop_and_delete(ex) {
            let _ = self.delete_created_files()?;
        }
        
        // Skip sending if remote already exited or failed
        if crate::error::is_remote_exit(ex) || crate::error::is_remote_fail(ex) {
            return self.server_exit(&ex.to_string());
        }
        
        let err_msg = ex.to_string();
        let err_type = if trace { "FAIL" } else { "fail" };
        send_string(&mut self.stdout, err_type, &err_msg, &self.config.newline)?;
        self.server_exit(&err_msg)
    }
    
    pub fn send_file_num(&mut self, num: usize) -> Result<()> {
        send_integer(&mut self.stdout, "NUM", num as i64, &self.config.newline)?;
        check_integer(&mut self.stdin, num as i64, &mut Vec::new(), &self.config.newline)?;
        Ok(())
    }
    
    pub fn recv_file_num(&mut self) -> Result<usize> {
        let mut buf = Vec::new();
        let num = recv_integer(&mut self.stdin, "NUM", &mut buf, &self.config.newline)? as usize;
        send_integer(&mut self.stdout, "SUCC", num as i64, &self.config.newline)?;
        Ok(num)
    }
    
    /// Check if a directory path is writable
    pub fn check_path_writable(path: &Path) -> Result<()> {
        if path.exists() {
            // Check if we can write to the directory
            let test_file = path.join(".nrzsz_write_test");
            match File::create(&test_file) {
                Ok(_) => {
                    let _ = fs::remove_file(&test_file);
                    Ok(())
                }
                Err(e) => Err(anyhow!("Cannot write to {}: {}", path.display(), e)),
            }
        } else {
            // Check if parent directory is writable
            if let Some(parent) = path.parent() {
                if parent.exists() {
                    return Self::check_path_writable(parent);
                }
            }
            Err(anyhow!("Directory does not exist: {}", path.display()))
        }
    }
    
    /// Check for duplicate file names in the list
    pub fn check_duplicate_names(file_list: &[FileInfo]) -> Result<()> {
        let mut names = std::collections::HashSet::new();
        for file in file_list {
            if let Some(name) = file.path_name.last() {
                if !names.insert(name.clone()) {
                    return Err(anyhow!("Duplicate file name: {}", name));
                }
            }
        }
        Ok(())
    }
    
    /// Check max filename length (255 bytes)
    pub fn check_max_name_length(name: &str) -> Result<()> {
        if name.len() > 255 {
            return Err(anyhow!("File name too long ({} bytes, max 255)", name.len()));
        }
        Ok(())
    }
    
    /// Format saved files message with individual file listing
    pub fn format_saved_files(count: usize, path: &Path, files: &[String]) -> String {
        let mut msg = format!("Saved {} file(s) to {}", count, path.display());
        for file in files {
            msg.push_str(&format!("\r\n- {}", file));
        }
        msg
    }

    // V3+ prefix hash exchange for resume support
    const PREFIX_HASH_STEP: u64 = 10 * 1024 * 1024; // 10MB chunks for prefix hash
    
    /// Send prefix hash for resume (sender side, V3+).
    /// Matches Go's sendPrefixHash wire behavior:
    /// - sends cumulative MD5 hashes at 10MB steps up to `file_size`
    /// - receiver acks each hash while still matching, then stops acking after the first mismatch
    /// - sender always finishes all hashes + sends Over
    ///
    /// Returns the matched step (bytes that are identical on both sides).
    pub fn send_prefix_hash(&mut self, file: &mut File, file_size: u64) -> Result<u64> {
        if file_size == 0 {
            return Ok(0);
        }
        
        let mut md5_hash = md5::Context::new();
        let mut step: u64 = 0;
        let mut match_step: u64 = 0;
        let mut waiting_ack = true;
        let mut buf = vec![0u8; Self::PREFIX_HASH_STEP as usize];
        
        while step < file_size {
            // Read up to PREFIX_HASH_STEP bytes (exact, so steps are aligned)
            let m = ((file_size - step) as usize).min(Self::PREFIX_HASH_STEP as usize);
            let mut n_total = 0;
            while n_total < m {
                let n = file.read(&mut buf[n_total..m])?;
                if n == 0 {
                    break;
                }
                n_total += n;
            }
            if n_total == 0 {
                break;
            }
            
            md5_hash.consume(&buf[..n_total]);
            step += n_total as u64;
            
            let hash_str = format!("{:x}", md5_hash.clone().compute());
            let prefix_hash = crate::protocol::PrefixHash {
                step: step as i64,
                hash: hash_str,
                over: false,
            };
            let hash_json = serde_json::to_string(&prefix_hash)?;
            send_string(&mut self.stdout, "HASH", &hash_json, &self.config.newline)?;
            
            if waiting_ack {
                // Receive ack
                let mut ack_buf = Vec::new();
                let ack_str = recv_string(&mut self.stdin, "SUCC", &mut ack_buf, &self.config.newline)?;
                let ack: crate::protocol::PrefixHashAck = serde_json::from_str(&ack_str)?;
                
                if ack.matched {
                    match_step = ack.step.max(0) as u64;
                    // Whole prefix matched -> done (Go returns when matchStep == size)
                    if match_step >= file_size {
                        break;
                    }
                } else {
                    // Receiver stopped matching; keep sending hashes but no longer wait for acks
                    waiting_ack = false;
                }
            }
        }
        
        // Send "over" hash to signal end of hash exchange (no ack expected)
        let prefix_hash = crate::protocol::PrefixHash {
            step: step as i64,
            hash: String::new(),
            over: true,
        };
        let hash_json = serde_json::to_string(&prefix_hash)?;
        send_string(&mut self.stdout, "HASH", &hash_json, &self.config.newline)?;
        
        Ok(match_step)
    }
    
    /// Receive prefix hash for resume (receiver side, V3+).
    /// Matches Go's recvPrefixHash wire behavior:
    /// - reads cumulative MD5 hashes, compares against existing file content
    /// - acks each hash while matching; after first mismatch stops acking and just skips hashes
    /// - On Over, seeks and truncates the file to the matched step
    pub fn recv_prefix_hash(&mut self, file: &mut File, _file_size: u64) -> Result<u64> {
        let mut hasher = md5::Context::new();
        let mut match_step: u64 = 0;
        let mut comparing = true;
        let mut buf = vec![0u8; Self::PREFIX_HASH_STEP as usize];
        
        loop {
            let mut ack_buf = Vec::new();
            let hash_str = recv_string(&mut self.stdin, "HASH", &mut ack_buf, &self.config.newline)?;
            let prefix_hash: crate::protocol::PrefixHash = serde_json::from_str(&hash_str)?;
            
            if prefix_hash.over {
                // Sender finished — no ack for Over
                break;
            }
            
            if !comparing {
                // Sender keeps sending hashes but we no longer compare or ack
                continue;
            }
            
            // Read the incremental bytes: hash.Step - matchStep
            let target_step = prefix_hash.step.max(0) as u64;
            if target_step > match_step {
                let need = (target_step - match_step) as usize;
                let mut n_total = 0;
                while n_total < need {
                    let read_buf = if need > buf.len() {
                        // need can exceed buffer for the first 10MB chunk; resize once
                        buf.resize(need, 0);
                        &mut buf[n_total..]
                    } else {
                        &mut buf[n_total..need]
                    };
                    let n = file.read(read_buf)?;
                    if n == 0 {
                        break;
                    }
                    n_total += n;
                }
                if n_total > 0 {
                    hasher.consume(&buf[..n_total]);
                    match_step += n_total as u64;
                }
            }
            
            let our_hash = format!("{:x}", hasher.clone().compute());
            let matched = our_hash == prefix_hash.hash;
            if !matched {
                comparing = false;
            } else {
                match_step = target_step;
            }
            
            let ack = crate::protocol::PrefixHashAck {
                step: prefix_hash.step,
                matched,
            };
            let ack_json = serde_json::to_string(&ack)?;
            send_string(&mut self.stdout, "SUCC", &ack_json, &self.config.newline)?;
        }
        
        // Seek and truncate the file to the matched step (Go behavior)
        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(match_step))?;
        file.set_len(match_step)?;
        
        Ok(match_step)
    }
    
    pub fn send_file_name(&mut self, file: &FileInfo) -> Result<String> {
        let name = file.path_name.last().unwrap().clone();
        
        // Validate filename length (255 characters max)
        if name.len() > 255 {
            return Err(anyhow!("Filename too long: {} (max 255 characters)", name));
        }
        
        if self.config.directory {
            let file_json = serde_json::to_string(file)?;
            send_string(&mut self.stdout, "NAME", &file_json, &self.config.newline)?;
        } else {
            send_string(&mut self.stdout, "NAME", &name, &self.config.newline)?;
        }
        
        let mut buf = Vec::new();
        let remote_name = recv_string(&mut self.stdin, "SUCC", &mut buf, &self.config.newline)?;
        Ok(remote_name)
    }
    
    pub fn recv_file_name(&mut self, dest_path: &Path) -> Result<(Option<File>, String, String)> {
        if self.config.directory {
            let mut buf = Vec::new();
            let file_json = recv_string(&mut self.stdin, "NAME", &mut buf, &self.config.newline)?;
            let file_info: DirectoryInfo = serde_json::from_str(&file_json)?;
            
            // Validate filename length (255 characters max)
            if file_info.path_name.last().is_some_and(|name| name.len() > 255) {
                return Err(anyhow!("Filename too long (max 255 characters)"));
            }
            
            let (file, local_name, file_name) = self.create_dir_or_file(dest_path, &file_info)?;
            
            send_string(&mut self.stdout, "SUCC", &local_name, &self.config.newline)?;
            Ok((file, local_name, file_name))
        } else {
            let mut buf = Vec::new();
            let file_name = recv_string(&mut self.stdin, "NAME", &mut buf, &self.config.newline)?;
            
            // Validate filename length (255 characters max)
            if file_name.len() > 255 {
                return Err(anyhow!("Filename too long: {} (max 255 characters)", file_name));
            }
            
            let (file, local_name) = self.create_file(dest_path, &file_name)?;
            
            send_string(&mut self.stdout, "SUCC", &local_name, &self.config.newline)?;
            Ok((Some(file), local_name, file_name))
        }
    }
    
    /// V3+ file name exchange with prefix hash for resume support
    /// Sends full FileInfo JSON (matching Go's sendFileNameV3 - always JSON in V3),
    /// receives TargetFile JSON, then does prefix hash exchange
    /// Returns (remaining file size after resume prefix hash, skipped).
    /// `skipped` is true when the receiver refused the file (protect /
    /// not-newer); the caller must not send SIZE/DATA/MD5 for it.
    pub fn send_file_name_v3(&mut self, file: &FileInfo) -> Result<(u64, bool)> {
        // Send full file info as JSON (Go's sendFileNameV3 always marshals sourceFile)
        let file_json = serde_json::to_string(file)?;
        send_string(&mut self.stdout, "NAME", &file_json, &self.config.newline)?;
        
        // Receive target file (name + size for resume / append)
        let mut buf = Vec::new();
        let target_json = recv_string(&mut self.stdin, "SUCC", &mut buf, &self.config.newline)?;
        let target: crate::protocol::TargetFile = serde_json::from_str(&target_json)?;
        
        // Receiver skipped the file (protect / not-newer): no data follows.
        if target.skip {
            return Ok((0, true));
        }
        
        // Append mode: the receiver opened the existing file and will append
        // every byte we send. Send the whole source from offset 0.
        if target.append {
            return Ok((file.size, false));
        }
        
        
        // Prefix hash exchange for resume support
        if target.size > 0 && file.size > 0 {
            if self.negotiated_protocol < crate::protocol::PROTOCOL_V4 {
                // V3: send SIZE without ack (original file size) — matching nrzsz-go
                send_integer(&mut self.stdout, "SIZE", file.size as i64, &self.config.newline)?;
            }
            
            let mut f = File::open(Path::new(&file.abs_path))?;
            // Go uses min(srcFile.Size, tgtFile.Size) for the hash comparison range
            let hash_size = file.size.min(target.size.max(0) as u64);
            let matched_step = self.send_prefix_hash(&mut f, hash_size)?;
            
            // Seek file to matched position for resume
            if matched_step > 0 {
                use std::io::Seek;
                f.seek(std::io::SeekFrom::Start(matched_step))?;
            }
            
            Ok((file.size - matched_step, false))
        } else {
            Ok((file.size, false))
        }
    }
    
    /// V3+ file name exchange with prefix hash for resume
    /// Receives full FileInfo JSON (Go's recvFileNameV3 always parses sourceFile JSON),
    /// creates file, sends TargetFile JSON, then does prefix hash exchange
    pub fn recv_file_name_v3(&mut self, dest_path: &Path) -> Result<(Option<File>, String, String, u64)> {
        let mut buf = Vec::new();
        let file_json = recv_string(&mut self.stdin, "NAME", &mut buf, &self.config.newline)?;
        let src_file: FileInfo = serde_json::from_str(&file_json)?;
        
        // Validate filename length (255 characters max)
        if src_file.path_name.last().is_some_and(|name| name.len() > 255) {
            return Err(anyhow!("Filename too long (max 255 characters)"));
        }
        
        // Compute the destination local name (same selection as create_dir_or_file_v3)
        let local_name = if self.config.overwrite {
            src_file.path_name[0].clone()
        } else if let Some(existing) = self.file_name_map.get(&src_file.path_id) {
            existing.clone()
        } else {
            let name = get_new_name(dest_path, &src_file.path_name[0])?;
            self.file_name_map.insert(src_file.path_id, name.clone());
            name
        };
        let file_name = src_file.path_name.last().unwrap().clone();
        
        // Determine the natural destination path (the name the user chose, not
        // any rename) — used for protect / newer / append existence checks.
        let natural_name = src_file.path_name[0].clone();
        let natural_path = if src_file.path_name.len() > 1 {
            dest_path.join(&natural_name)
                .join(src_file.path_name[1..src_file.path_name.len()-1].iter().collect::<PathBuf>())
                .join(&file_name)
        } else {
            dest_path.join(&natural_name)
        };
        
        // ---- Skip decision: protect (`-p`) and overwrite-newer (`-Y`) ----
        let dest_exists = natural_path.exists() && !src_file.is_dir;
        if dest_exists {
            if self.config.protect {
                // lrzsz `-p`/`ZF1_ZMPROT`: skip if destination exists.
                let target = crate::protocol::TargetFile {
                    name: local_name.clone(),
                    size: 0,
                    skip: true,
                    append: false,
                };
                send_string(&mut self.stdout, "SUCC", &serde_json::to_string(&target)?, &self.config.newline)?;
                return Ok((None, local_name, file_name, 0));
            }
            if self.config.overwrite_newer {
                // lrzsz `-Y`/`Lskipnocor`: overwrite only if the source is newer.
                let dest_mtime = fs::metadata(&natural_path).ok().and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as i64);
                let src_mtime = src_file.mtime_ns;
                // Skip when destination is at least as new as the source.
                let should_skip = match (dest_mtime, src_mtime) {
                    (Some(d), Some(s)) => d >= s,
                    _ => true, // no mtime info: treat as not-newer, skip
                };
                if should_skip {
                    let target = crate::protocol::TargetFile {
                        name: local_name.clone(),
                        size: 0,
                        skip: true,
                        append: false,
                    };
                    send_string(&mut self.stdout, "SUCC", &serde_json::to_string(&target)?, &self.config.newline)?;
                    return Ok((None, local_name, file_name, 0));
                }
                // Source is newer: overwrite at the natural path (truncate).
                self.config.overwrite = true;
            }
        }
        
        // ---- Append decision (`-+`/`ZF1_ZMAPND`) ----
        if self.config.append && dest_exists {
            let append_offset = fs::metadata(&natural_path).map(|m| m.len()).unwrap_or(0);
            let mut open_opts = fs::OpenOptions::new();
            open_opts.read(true).write(true).append(true);
            let file = open_opts.open(&natural_path)?;
            let target = crate::protocol::TargetFile {
                name: natural_name.clone(),
                size: append_offset as i64,
                skip: false,
                append: true,
            };
            send_string(&mut self.stdout, "SUCC", &serde_json::to_string(&target)?, &self.config.newline)?;
            // Sender sends only the tail (file.size - append_offset). No prefix hash.
            return Ok((Some(file), natural_name, file_name, src_file.size as u64));
        }
        
        
        // ---- Normal create (without truncation for resume support) ----
        let (mut file, local_name, file_name) = self.create_dir_or_file_v3(dest_path, &src_file)?;
        
        // Get file size for target file response
        let mut tgt_size: u64 = 0;
        if let Some(ref f) = file {
            if let Ok(metadata) = f.metadata() {
                tgt_size = metadata.len();
            }
        }
        
        // Send target file JSON (name + size)
        let target = crate::protocol::TargetFile {
            name: local_name.clone(),
            size: tgt_size as i64,
            skip: false,
            append: false,
        };
        let target_json = serde_json::to_string(&target)?;
        send_string(&mut self.stdout, "SUCC", &target_json, &self.config.newline)?;
        
        // Do prefix hash exchange if file exists and size > 0
        let remaining_size = if let Some(ref mut f) = file {
            if tgt_size > 0 && src_file.size > 0 {
                if self.negotiated_protocol < crate::protocol::PROTOCOL_V4 {
                    // V3: receive SIZE without ack (original file size) — matching nrzsz-go
                    let _original_size = recv_integer(&mut self.stdin, "SIZE", &mut buf, &self.config.newline)?;
                }
                let matched_step = self.recv_prefix_hash(f, tgt_size)?;
                src_file.size as u64 - matched_step
            } else {
                src_file.size as u64
            }
        } else {
            0
        };
        
        Ok((file, local_name, file_name, remaining_size))
    }
    
    pub fn send_file_size(&mut self, file_path: &Path) -> Result<u64> {
        let metadata = fs::metadata(file_path)?;
        let size = metadata.len();
        send_integer(&mut self.stdout, "SIZE", size as i64, &self.config.newline)?;
        check_integer(&mut self.stdin, size as i64, &mut Vec::new(), &self.config.newline)?;
        Ok(size)
    }
    
    /// Send file size (remaining size after prefix hash match).
    /// In V3 resume mode, this is the remaining bytes after matching prefix.
    pub fn send_remaining_size(&mut self, size: u64) -> Result<()> {
        send_integer(&mut self.stdout, "SIZE", size as i64, &self.config.newline)?;
        check_integer(&mut self.stdin, size as i64, &mut Vec::new(), &self.config.newline)?;
        Ok(())
    }
    
    pub fn recv_file_size(&mut self) -> Result<u64> {
        let mut buf = Vec::new();
        let size = recv_integer(&mut self.stdin, "SIZE", &mut buf, &self.config.newline)? as u64;
        send_integer(&mut self.stdout, "SUCC", size as i64, &self.config.newline)?;
        Ok(size)
    }
    
    /// Determine if compression is fixed for a given file size (nrzsz-go compatible)
    /// Returns (is_fixed, should_compress)
    pub fn is_compress_fixed(&self, size: u64) -> (bool, bool) {
        if self.negotiated_protocol < crate::protocol::PROTOCOL_V3 {
            // V2: always fixed, compress if not binary
            return (true, !self.config.binary);
        }
        match &self.config.compress {
            crate::protocol::CompressType::Yes => (true, true),
            crate::protocol::CompressType::No => (true, false),
            crate::protocol::CompressType::Auto => {
                if size < 512 {
                    (true, false)
                } else if size < 128 * 1024 {
                    (true, true)
                } else {
                    (false, false) // need to detect
                }
            }
        }
    }
    
    /// Send compression flag for a file (V3+)
    /// Samples 3 positions (beginning, middle, end) matching Go's isCompressedFileContent.
    pub fn send_compress_flag(&mut self, file_path: &Path) -> Result<bool> {
        let metadata = fs::metadata(file_path)?;
        let size = metadata.len();
        
        let (fixed, compress) = self.is_compress_fixed(size);
        if fixed {
            return Ok(compress);
        }
        
        let block_size = 128 * 1024;
        let mut file = File::open(file_path)?;
        let mut compressed_count = 0;
        let mut positions = Vec::new();
        
        if size >= block_size as u64 {
            positions.push(0u64);
        }
        if size >= 2 * block_size as u64 {
            positions.push(size - block_size as u64);
        }
        if size >= 3 * block_size as u64 {
            positions.push((size / 2) - (block_size as u64 / 2));
        }
        
        for &pos in &positions {
            use std::io::{Seek, SeekFrom};
            file.seek(SeekFrom::Start(pos))?;
            let mut sample = vec![0u8; block_size];
            if file.read_exact(&mut sample).is_err() {
                continue;
            }
            let compressed = crate::compress::compress_data(&sample, crate::compress::CompressAlgo::Zstd)
                .unwrap_or_default();
            if compressed.len() > sample.len() * 98 / 100 {
                compressed_count += 1;
            }
        }
        
        let threshold = positions.len().max(1) / 2;
        let compress = compressed_count < threshold + 1;
        send_string(&mut self.stdout, "COMP", &compress.to_string(), &self.config.newline)?;
        Ok(compress)
    }
    
    /// Receive compression flag for a file (V3+)
    pub fn recv_compress_flag(&mut self, size: u64) -> Result<bool> {
        let (fixed, compress) = self.is_compress_fixed(size);
        if fixed {
            return Ok(compress);
        }
        
        let mut buf = Vec::new();
        let flag_str = recv_string(&mut self.stdin, "COMP", &mut buf, &self.config.newline)?;
        let compress = flag_str.trim().parse::<bool>()?;
        Ok(compress)
    }
    
    
    /// Effective buffer cap: `window_size` when set (lrzsz `-w`), otherwise
    /// `max_buf_size`. Never exceeds `max_buf_size`.
    fn effective_buf_cap(&self) -> usize {
        self.config.window_size
            .map(|w| w as usize)
            .filter(|w| *w > 0)
            .map(|w| w.min(self.config.max_buf_size))
            .unwrap_or(self.config.max_buf_size)
    }
    
    pub fn send_file_data(&mut self, file_path: &Path, size: u64, progress: &TransferProgress) -> Result<Vec<u8>> {
        let mut file = File::open(file_path)?;
        let mut buf_size: usize = 1024; // Start small for fast start
        let mut buf = vec![0u8; buf_size];
        let mut md5_hash = md5::Context::new();
        let mut sent = 0u64;
        
            // Check if remote supports compression before using it
        let compress_type = if self.can_use_compress() {
            match &self.config.compress {
                crate::protocol::CompressType::Auto => crate::compress::CompressAlgo::Zstd,
                crate::protocol::CompressType::Yes => crate::compress::CompressAlgo::Zstd,
                crate::protocol::CompressType::No => crate::compress::CompressAlgo::None,
            }
        } else {
            crate::compress::CompressAlgo::None
        };
        
        let buf_cap = self.effective_buf_cap();
        
        while sent < size {
            let chunk_start = std::time::Instant::now();
            
            let bytes_read = file.read(&mut buf)?;
            if bytes_read == 0 {
                break;
            }
            
            let data = &buf[..bytes_read];
            
            let (compressed_data, _was_compressed) = if compress_type != crate::compress::CompressAlgo::None {
                // The negotiated compression mode is the wire contract (Go's
                // pipelineEncodeData): once compression is on, every chunk is
                // compressed. Re-deciding profitability per chunk here would
                // desync the receiver, which trusts the mode/flag.
                (crate::compress::compress_data(data, compress_type)?, true)
            } else {
                (data.to_vec(), false)
            };
            
            if self.config.binary {
                let escaped = escape_data(&compressed_data, &self.config.escape_chars);
                send_line(&mut self.stdout, "DATA", &escaped.len().to_string(), &self.config.newline)?;
                self.stdout.write_all(&escaped)?;
            } else {
                send_binary(&mut self.stdout, "DATA", &compressed_data, &self.config.newline)?;
            }
            
            md5_hash.consume(data);
            
            // V1 ack: plain integer (length only)
            check_integer(&mut self.stdin, compressed_data.len() as i64, &mut Vec::new(), &self.config.newline)?;
            
            sent += bytes_read as u64;
            progress.update(sent);
            
            // Time-based buffer adaptation (matching nrzsz-go)
            let chunk_time = chunk_start.elapsed();
            self.set_last_chunk_time(chunk_time);
            if bytes_read == buf_size && chunk_time < std::time::Duration::from_millis(500) && buf_size < buf_cap {
                // Double buffer if chunk was full and fast
                buf_size = (buf_size * 2).min(buf_cap);
                buf.resize(buf_size, 0);
            } else if chunk_time >= std::time::Duration::from_secs(2) && buf_size > 1024 {
                // Reduce buffer if too slow
                buf_size = (buf_size / chunk_time.as_secs() as usize).max(1024);
                buf.resize(buf_size, 0);
            }
        }
        
        Ok(md5_hash.compute().to_vec())
    }
    
    pub fn recv_file_data(&mut self, file: &mut File, size: u64, progress: &TransferProgress) -> Result<Vec<u8>> {
        let mut md5_hash = md5::Context::new();
        let mut received = 0u64;
        
        // Check if remote supports compression before using it
        let mut compress_type = if self.can_use_compress() {
            match &self.config.compress {
                crate::protocol::CompressType::Auto => crate::compress::CompressAlgo::Zstd,
                crate::protocol::CompressType::Yes => crate::compress::CompressAlgo::Zstd,
                crate::protocol::CompressType::No => crate::compress::CompressAlgo::None,
            }
        } else {
            crate::compress::CompressAlgo::None
        };
        let mut first_chunk = true;
        
        while received < size {
            let compressed_data = if self.config.binary {
                let mut buf = Vec::new();
                let line = recv_line(&mut self.stdin, &mut buf, &self.config.newline)?;
                let idx = line.find(':').ok_or_else(|| anyhow!("Invalid format"))?;
                let prefix = &line[1..idx];
                if prefix != "DATA" {
                    return Err(anyhow!("Expected DATA, got {}", prefix));
                }
                let value = line[idx+1..].trim();
                // V3 pause marker: #DATA:=\n
                if value == "=" {
                    continue;
                }
                let len = value.parse::<usize>()?;
                let mut data_buf = vec![0u8; len];
                self.stdin.read_exact(&mut data_buf)?;
                unescape_data(&data_buf, &self.config.escape_chars)
            } else {
                let mut buf = Vec::new();
                recv_binary(&mut self.stdin, "DATA", &mut buf, &self.config.newline)?
            };
            
            if first_chunk && compress_type == crate::compress::CompressAlgo::None {
                compress_type = crate::compress::detect_compress_type(&compressed_data);
                first_chunk = false;
            }
            
            let data = if compress_type != crate::compress::CompressAlgo::None {
                crate::compress::decompress_data(&compressed_data, compress_type)?
            } else {
                compressed_data.clone()
            };
            
            file.write_all(&data)?;
            md5_hash.consume(&data);
            
            // V1 ack: plain integer (length only)
            send_integer(&mut self.stdout, "SUCC", compressed_data.len() as i64, &self.config.newline)?;
            
            // Track uncompressed data size
            received += data.len() as u64;
            progress.update(received);
        }
        
        Ok(md5_hash.compute().to_vec())
    }
    
    /// Send file data with explicit compression flag (V3+ compatible)
    #[allow(clippy::too_many_arguments)]
    pub fn send_file_data_with_compress(&mut self, file_path: &Path, size: u64, progress: &TransferProgress, use_compress: bool, start_offset: u64) -> Result<Vec<u8>> {
        let mut file = File::open(file_path)?;
        // Seek to the resume position (0 for a fresh transfer; matched_step for V3 resume)
        if start_offset > 0 {
            use std::io::Seek;
            file.seek(std::io::SeekFrom::Start(start_offset))?;
        }
        let mut buf_size: usize = 1024; // Start small for fast start
        let mut buf = vec![0u8; buf_size];
        let mut md5_hash = md5::Context::new();
        let mut sent = 0u64;
        
        let compress_type = if use_compress && self.can_use_compress() {
            match &self.config.compress {
                crate::protocol::CompressType::Auto => crate::compress::CompressAlgo::Zstd,
                crate::protocol::CompressType::Yes => crate::compress::CompressAlgo::Zstd,
                crate::protocol::CompressType::No => crate::compress::CompressAlgo::None,
            }
        } else {
            crate::compress::CompressAlgo::None
        };
        
        let buf_cap = self.effective_buf_cap();
        
        while sent < size {
            // V3: check stop/pause before each chunk (sends pause markers if pausing)
            if self.negotiated_protocol >= crate::protocol::PROTOCOL_V3 {
                self.check_stop_and_pause("DATA")?;
            }
            let chunk_start = std::time::Instant::now();
            
            let bytes_read = file.read(&mut buf)?;
            if bytes_read == 0 {
                break;
            }
            
            let data = &buf[..bytes_read];
            
            let (compressed_data, _was_compressed) = if compress_type != crate::compress::CompressAlgo::None {
                // The negotiated compression mode is the wire contract (Go's
                // pipelineEncodeData): once compression is on, every chunk is
                // compressed. Re-deciding profitability per chunk here would
                // desync the receiver, which trusts the mode/flag.
                (crate::compress::compress_data(data, compress_type)?, true)
            } else {
                (data.to_vec(), false)
            };
            
            if self.config.binary {
                let escaped = escape_data(&compressed_data, &self.config.escape_chars);
                send_line(&mut self.stdout, "DATA", &escaped.len().to_string(), &self.config.newline)?;
                self.stdout.write_all(&escaped)?;
            } else {
                send_binary(&mut self.stdout, "DATA", &compressed_data, &self.config.newline)?;
            }
            
            md5_hash.consume(data);
            
            // V2/V3 ack format: length/step
            check_pipeline_ack(&mut self.stdin, compressed_data.len() as i64, &mut Vec::new(), &self.config.newline)?;
            
            sent += bytes_read as u64;
            progress.update(sent);
            
            // Time-based buffer adaptation (matching nrzsz-go)
            let chunk_time = chunk_start.elapsed();
            self.set_last_chunk_time(chunk_time);
            if bytes_read == buf_size && chunk_time < std::time::Duration::from_millis(500) && buf_size < buf_cap {
                // Double buffer if chunk was full and fast
                buf_size = (buf_size * 2).min(buf_cap);
                buf.resize(buf_size, 0);
            } else if chunk_time >= std::time::Duration::from_secs(2) && buf_size > 1024 {
                // Reduce buffer if too slow
                buf_size = (buf_size / chunk_time.as_secs() as usize).max(1024);
                buf.resize(buf_size, 0);
            }
        }
        
        Ok(md5_hash.compute().to_vec())
    }
    
    /// Receive file data with explicit compression flag (V3+ compatible)
    pub fn recv_file_data_with_compress(&mut self, file: &mut File, size: u64, progress: &TransferProgress, use_compress: bool) -> Result<Vec<u8>> {
        let mut md5_hash = md5::Context::new();
        let mut received = 0u64;
        
        let mut compress_type = if use_compress && self.can_use_compress() {
            match &self.config.compress {
                crate::protocol::CompressType::Auto => crate::compress::CompressAlgo::Zstd,
                crate::protocol::CompressType::Yes => crate::compress::CompressAlgo::Zstd,
                crate::protocol::CompressType::No => crate::compress::CompressAlgo::None,
            }
        } else {
            crate::compress::CompressAlgo::None
        };
        let mut first_chunk = true;
        
        while received < size {
            // Check for stop
            if self.is_stopped() {
                return Err(anyhow!("Transfer stopped"));
            }
            
            let compressed_data = if self.config.binary {
                let mut buf = Vec::new();
                let line = recv_line(&mut self.stdin, &mut buf, &self.config.newline)?;
                let idx = line.find(':').ok_or_else(|| anyhow!("Invalid format"))?;
                let prefix = &line[1..idx];
                if prefix != "DATA" {
                    return Err(anyhow!("Expected DATA, got {}", prefix));
                }
                let value = line[idx+1..].trim();
                // V3 pause marker: #DATA:=\n
                if value == "=" {
                    continue;
                }
                let len = value.parse::<usize>()?;
                let mut data_buf = vec![0u8; len];
                self.stdin.read_exact(&mut data_buf)?;
                unescape_data(&data_buf, &self.config.escape_chars)
            } else {
                let mut buf = Vec::new();
                recv_binary(&mut self.stdin, "DATA", &mut buf, &self.config.newline)?
            };
            
            if first_chunk && compress_type == crate::compress::CompressAlgo::None {
                compress_type = crate::compress::detect_compress_type(&compressed_data);
                first_chunk = false;
            }
            
            let data = if compress_type != crate::compress::CompressAlgo::None {
                crate::compress::decompress_data(&compressed_data, compress_type)?
            } else {
                compressed_data.clone()
            };
            
            file.write_all(&data)?;
            md5_hash.consume(&data);
            
            // V2/V3 ack format: length/step
            send_pipeline_ack(&mut self.stdout, compressed_data.len() as i64, received as i64, &self.config.newline)?;
            
            // Track uncompressed data size
            received += data.len() as u64;
            progress.update(received);
        }
        
        Ok(md5_hash.compute().to_vec())
    }
    
    pub fn send_file_md5(&mut self, digest: &[u8]) -> Result<()> {
        send_binary(&mut self.stdout, "MD5", digest, &self.config.newline)?;
        let mut buf = Vec::new();
        check_binary(&mut self.stdin, digest, &mut buf, &self.config.newline)?;
        Ok(())
    }
    
    pub fn recv_file_md5(&mut self, digest: &[u8]) -> Result<()> {
        let mut buf = Vec::new();
        let expected = recv_binary(&mut self.stdin, "MD5", &mut buf, &self.config.newline)?;
        if digest != expected.as_slice() {
            return Err(anyhow!("MD5 check failed"));
        }
        send_binary(&mut self.stdout, "SUCC", digest, &self.config.newline)?;
        Ok(())
    }
    
    pub fn send_files(&mut self, file_list: &[FileInfo], quiet: bool) -> Result<Vec<String>> {
        // Save tmux status interval before transfer
        self.save_tmux_status_interval();
        
        self.send_file_num(file_list.len())?;
        
        let mut remote_list = Vec::new();
        
        for file in file_list {
            if self.negotiated_protocol >= crate::protocol::PROTOCOL_V3 {
                // V3: send full file info JSON, receive target file JSON, do prefix hash
                let (remaining_size, skipped) = self.send_file_name_v3(file)?;
                
                let remote_name = file.path_name.last().unwrap().clone();
                if !skipped && !remote_list.contains(&remote_name) {
                    remote_list.push(remote_name);
                }
                
                if file.is_dir {
                    continue;
                }
                
                // Receiver refused the file (protect / not-newer): no data follows.
                if skipped {
                    continue;
                }
                
                // V3: send SIZE with ack (remaining size after prefix hash)
                // Go sends srcSize - matchStep as the SIZE value
                let size = if remaining_size < file.size {
                    self.send_remaining_size(remaining_size)?;
                    remaining_size
                } else {
                    self.send_file_size(Path::new(&file.abs_path))?
                };
                
                // Use remaining_size for data transfer if prefix hash matched
                let data_size = if remaining_size < file.size { remaining_size } else { size };
                // Resume offset = file.size - data_size (matched_step when resuming, 0 otherwise)
                let start_offset = file.size.saturating_sub(data_size);
                
                let use_compress = self.send_compress_flag(Path::new(&file.abs_path))?;
                let display_name = file.path_name.join("/");
                let progress = TransferProgress::new_with_color(
                    data_size,
                    quiet,
                    Some(display_name),
                    self.config.color_pair_option(),
                );
                let md5 = self.send_file_data_with_compress(Path::new(&file.abs_path), data_size, &progress, use_compress, start_offset)?;
                self.send_file_md5(&md5)?;
                progress.finish("Done");
            } else {
                // V2: send filename
                let remote_name = self.send_file_name(file)?;
                if !remote_list.contains(&remote_name) {
                    remote_list.push(remote_name);
                }
                
                if file.is_dir {
                    continue;
                }
                
                // V2: send size and data
                let size = self.send_file_size(Path::new(&file.abs_path))?;
                
                let (fixed, compress) = self.is_compress_fixed(size);
                let use_compress = if fixed { compress } else { false };
                let progress = TransferProgress::new_with_color(
                    size,
                    quiet,
                    Some(file.path_name.join("/")),
                    self.config.color_pair_option(),
                );
                let md5 = self.send_file_data_with_compress(Path::new(&file.abs_path), size, &progress, use_compress, 0)?;
                self.send_file_md5(&md5)?;
                progress.finish("Done");
            }
        }
        
        // Restore tmux status interval after transfer
        self.restore_tmux_status_interval();
        
        Ok(remote_list)
    }
    
    pub fn recv_files(&mut self, dest_path: &Path, quiet: bool) -> Result<Vec<String>> {
        // Save tmux status interval before transfer
        self.save_tmux_status_interval();
        
        let num = self.recv_file_num()?;
        let mut local_list = Vec::new();
        
        for _ in 0..num {
            let (file, local_name, _file_name, _remaining_size) = if self.negotiated_protocol >= crate::protocol::PROTOCOL_V3 {
                // V3: receive full file info JSON, send target file JSON, do prefix hash
                self.recv_file_name_v3(dest_path)?
            } else {
                // V2: receive filename, remaining_size is placeholder
                let (f, ln, fn_) = self.recv_file_name(dest_path)?;
                (f, ln, fn_, 0)
            };
            
            if !local_list.contains(&local_name) {
                local_list.push(local_name.clone());
            }
            
            let mut file = match file {
                Some(f) => f,
                None => continue,
            };
            
            if self.negotiated_protocol >= crate::protocol::PROTOCOL_V3 {
                // V3: recv_file_name_v3 handles NAME, TARGET, SIZE (no ack), prefix hash
                // Now receive SIZE with ack (remaining size) — matching nrzsz-go recvFileSize
                let size = self.recv_file_size()?;
                let use_compress = self.recv_compress_flag(size)?;
                let progress = TransferProgress::new_with_color(
                    size,
                    quiet,
                    Some(local_name),
                    self.config.color_pair_option(),
                );
                let md5 = self.recv_file_data_with_compress(&mut file, size, &progress, use_compress)?;
                self.recv_file_md5(&md5)?;
                progress.finish("Done");
            } else {
                // V2: receive size and data
                let size = self.recv_file_size()?;
                let use_compress = self.recv_compress_flag(size)?;
                let progress = TransferProgress::new_with_color(
                    size,
                    quiet,
                    Some(local_name),
                    self.config.color_pair_option(),
                );
                let md5 = self.recv_file_data_with_compress(&mut file, size, &progress, use_compress)?;
                self.recv_file_md5(&md5)?;
                progress.finish("Done");
            }
        }
        
        // Restore tmux status interval after transfer
        self.restore_tmux_status_interval();
        
        Ok(local_list)
    }
    
    fn create_file(&mut self, dest_path: &Path, file_name: &str) -> Result<(File, String)> {
        let local_name = if self.config.overwrite {
            file_name.to_string()
        } else {
            get_new_name(dest_path, file_name)?
        };
        
        let full_path = dest_path.join(&local_name);
        let file = File::create(&full_path)?;
        self.created_files.push(full_path);
        
        Ok((file, local_name))
    }
    
    fn create_dir_or_file(&mut self, dest_path: &Path, file_info: &DirectoryInfo) -> Result<(Option<File>, String, String)> {
        let file_name = file_info.path_name.last().unwrap().clone();
        
        let local_name = if self.config.overwrite {
            file_info.path_name[0].clone()
        } else {
            // Reuse the chosen local name for the same source path_id (Go's fileNameMap),
            // so multiple files from one source directory land in the same dest directory.
            if let Some(existing) = self.file_name_map.get(&file_info.path_id) {
                existing.clone()
            } else {
                let name = get_new_name(dest_path, &file_info.path_name[0])?;
                self.file_name_map.insert(file_info.path_id, name.clone());
                name
            }
        };
        
        // Apply permissions if specified
        let apply_perm = |path: &Path, perm: Option<u32>| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(p) = perm {
                    if let Ok(meta) = fs::metadata(path) {
                        let mut perms = meta.permissions();
                        perms.set_mode(p | 0o700);
                        let _ = fs::set_permissions(path, perms);
                    }
                }
            }
            #[cfg(not(unix))]
            { let _ = path; let _ = perm; }
        };
        
        if file_info.path_name.len() > 1 {
            let parent_path = dest_path.join(&local_name).join(file_info.path_name[1..file_info.path_name.len()-1].iter().collect::<PathBuf>());
            fs::create_dir_all(&parent_path)?;
            self.created_files.push(parent_path.clone());
            
            let full_path = parent_path.join(&file_name);
            
            if file_info.is_dir {
                fs::create_dir_all(&full_path)?;
                self.created_files.push(full_path.clone());
                apply_perm(&full_path, file_info.perm);
                return Ok((None, local_name, file_name));
            }
            
            let file = File::create(&full_path)?;
            self.created_files.push(full_path.clone());
            apply_perm(&full_path, file_info.perm);
            Ok((Some(file), local_name, file_name))
        } else {
            let full_path = dest_path.join(&local_name);
            
            if file_info.is_dir {
                fs::create_dir_all(&full_path)?;
                self.created_files.push(full_path.clone());
                apply_perm(&full_path, file_info.perm);
                return Ok((None, local_name, file_name));
            }
            
            let file = File::create(&full_path)?;
            self.created_files.push(full_path.clone());
            apply_perm(&full_path, file_info.perm);
            Ok((Some(file), local_name, file_name))
        }
    }
    
    /// V3 file creation (without truncation for resume support)
    fn create_dir_or_file_v3(&mut self, dest_path: &Path, file_info: &FileInfo) -> Result<(Option<File>, String, String)> {
        let file_name = file_info.path_name.last().unwrap().clone();
        
        let local_name = if self.config.overwrite {
            file_info.path_name[0].clone()
        } else {
            // Reuse the chosen local name for the same source path_id (Go's fileNameMap),
            // so multiple files from one source directory land in the same dest directory.
            if let Some(existing) = self.file_name_map.get(&file_info.path_id) {
                existing.clone()
            } else {
                let name = get_new_name(dest_path, &file_info.path_name[0])?;
                self.file_name_map.insert(file_info.path_id, name.clone());
                name
            }
        };
        
        let apply_perm = |path: &Path, perm: Option<u32>| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(p) = perm {
                    if let Ok(meta) = fs::metadata(path) {
                        let mut perms = meta.permissions();
                        perms.set_mode(p | 0o700);
                        let _ = fs::set_permissions(path, perms);
                    }
                }
            }
            #[cfg(not(unix))]
            { let _ = path; let _ = perm; }
        };
        
        if file_info.path_name.len() > 1 {
            let parent_path = dest_path.join(&local_name).join(file_info.path_name[1..file_info.path_name.len()-1].iter().collect::<PathBuf>());
            fs::create_dir_all(&parent_path)?;
            self.created_files.push(parent_path.clone());
            
            let full_path = parent_path.join(&file_name);
            
            if file_info.is_dir {
                fs::create_dir_all(&full_path)?;
                self.created_files.push(full_path.clone());
                apply_perm(&full_path, file_info.perm);
                return Ok((None, local_name, file_name));
            }
            
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                let mut open_opts = fs::OpenOptions::new();
                open_opts.read(true).write(true).create(true).truncate(self.config.overwrite);
                if let Some(p) = file_info.perm {
                    open_opts.mode(p | 0o600);
                }
                let file = open_opts.open(&full_path)?;
                self.created_files.push(full_path.clone());
                apply_perm(&full_path, file_info.perm);
                Ok((Some(file), local_name, file_name))
            }
            #[cfg(not(unix))]
            {
                let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(self.config.overwrite).open(&full_path)?;
                self.created_files.push(full_path.clone());
                apply_perm(&full_path, file_info.perm);
                Ok((Some(file), local_name, file_name))
            }
        } else {
            let full_path = dest_path.join(&local_name);
            
            if file_info.is_dir {
                fs::create_dir_all(&full_path)?;
                self.created_files.push(full_path.clone());
                apply_perm(&full_path, file_info.perm);
                return Ok((None, local_name, file_name));
            }
            
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                let mut open_opts = fs::OpenOptions::new();
                open_opts.read(true).write(true).create(true).truncate(self.config.overwrite);
                if let Some(p) = file_info.perm {
                    open_opts.mode(p | 0o600);
                }
                let file = open_opts.open(&full_path)?;
                self.created_files.push(full_path.clone());
                apply_perm(&full_path, file_info.perm);
                Ok((Some(file), local_name, file_name))
            }
            #[cfg(not(unix))]
            {
                let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(self.config.overwrite).open(&full_path)?;
                self.created_files.push(full_path.clone());
                apply_perm(&full_path, file_info.perm);
                Ok((Some(file), local_name, file_name))
            }
        }
    }
}

fn get_new_name(path: &Path, name: &str) -> Result<String> {
    let full_path = path.join(name);
    if !full_path.exists() {
        return Ok(name.to_string());
    }
    
    for i in 0..1000 {
        let new_name = format!("{}.{}", name, i);
        if !path.join(&new_name).exists() {
            return Ok(new_name);
        }
    }
    
    Err(anyhow!("Failed to assign new file name"))
}

pub fn check_paths_readable(paths: &[String], directory: bool) -> Result<Vec<FileInfo>> {
    let mut file_list = Vec::new();
    
    for (i, path) in paths.iter().enumerate() {
        let abs_path = PathBuf::from(path).canonicalize()?;
        
        if !abs_path.exists() {
            return Err(anyhow!("No such file: {}", abs_path.display()));
        }
        
        let metadata = fs::metadata(&abs_path)?;
        
        if !directory && metadata.is_dir() {
            return Err(anyhow!("Is a directory: {}", abs_path.display()));
        }
        
        let mut visited_dirs = std::collections::HashSet::new();
        check_path_readable(i as u32, &abs_path, &mut file_list, &mut vec![abs_path.file_name().unwrap().to_string_lossy().to_string()], &mut visited_dirs)?;
    }
    
    Ok(file_list)
}

fn check_path_readable(
    path_id: u32,
    path: &Path,
    file_list: &mut Vec<FileInfo>,
    rel_path: &mut Vec<String>,
    visited_dirs: &mut std::collections::HashSet<PathBuf>,
) -> Result<()> {
    let metadata = fs::metadata(path)?;
    
    if !metadata.is_dir() {
        if !metadata.is_file() {
            return Err(anyhow!("Not a regular file: {}", path.display()));
        }
        
        // Check read permission (platform-specific)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o444 == 0 {
                return Err(anyhow!("No permission to read: {}", path.display()));
            }
        }
        
        #[cfg(windows)]
        {
            // On Windows, we can't easily check read permissions in the same way
            // We'll rely on the file open operation to fail if no permission
        }
        
        // Capture file permissions for transfer
        #[cfg(unix)]
        let perm = {
            use std::os::unix::fs::PermissionsExt;
            Some(metadata.permissions().mode() & 0o7777)
        };
        #[cfg(not(unix))]
        let perm: Option<u32> = None;
        
        file_list.push(FileInfo {
            path_id,
            abs_path: path.to_string_lossy().to_string(),
            path_name: rel_path.clone(),
            is_dir: false,
            archive: false,
            size: metadata.len(),
            perm,
            mtime_ns: mtime_nanos(&metadata),
        });
        
        
        return Ok(());
    }
    
    let real_path = fs::canonicalize(path)?;
    if visited_dirs.contains(&real_path) {
        return Err(anyhow!("Duplicate link: {}", path.display()));
    }
    visited_dirs.insert(real_path);
    
    // Capture directory permissions for transfer
    #[cfg(unix)]
    let dir_perm = {
        use std::os::unix::fs::PermissionsExt;
        Some(metadata.permissions().mode() & 0o7777)
    };
    #[cfg(not(unix))]
    let dir_perm: Option<u32> = None;
    
    file_list.push(FileInfo {
        path_id,
        abs_path: path.to_string_lossy().to_string(),
        path_name: rel_path.clone(),
        is_dir: true,
        archive: false,
        size: 0,
        perm: dir_perm,
        mtime_ns: None,
    });
    
    
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file_path = entry.path();
        rel_path.push(entry.file_name().to_string_lossy().to_string());
        check_path_readable(path_id, &file_path, file_list, rel_path, visited_dirs)?;
        rel_path.pop();
    }
    
    Ok(())
}

/// Modification time in nanoseconds since the Unix epoch, or `None` if
/// unavailable (used for the `-Y` overwrite-newer comparison).
fn mtime_nanos(metadata: &fs::Metadata) -> Option<i64> {
    metadata.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A stdin source that yields EOF immediately so the reader thread exits
    /// cleanly (the session under test only exercises the stop mechanism).
    struct EofStdin;
    impl Read for EofStdin {
        fn read(&mut self, _b: &mut [u8]) -> std::io::Result<usize> {
            Ok(0)
        }
    }

    /// The wrapper's shared stop handle must stop a running session in place:
    /// set the session's stopped flag and wake any blocked protocol read
    /// (regression test for filter.StopTransferringFiles reaching the session).
    #[test]
    fn shared_stop_handle_stops_session() {
        let stop = TransferStop::new();
        let _session = TransferSession::new_with_stop(
            crate::protocol::Config::default(),
            Box::new(EofStdin),
            Box::new(std::io::sink()),
            stop.clone(),
        );

        // Before: not stopped.
        assert!(!stop.stopped_flag().load(Ordering::SeqCst));
        assert!(!stop.is_stop_and_delete());

        // Wrapper stops the transfer (keep files) and wakes the shared buffer.
        stop.stop(false);
        assert!(stop.stopped_flag().load(Ordering::SeqCst));
        assert!(!stop.is_stop_and_delete());
        // A blocked protocol read on the shared buffer now unblocks with an error.
        assert!(stop
            .buffer()
            .read_line(false, Some(Duration::from_secs(1)))
            .is_err());

        // A second stop() is a no-op (the flag already won the CAS) and must not
        // clobber the stop-and-delete bit (Go: the first stop wins).
        stop.stop(true);
        assert!(
            !stop.is_stop_and_delete(),
            "second stop must not overwrite stop_and_delete"
        );
    }

    /// stop_and_delete propagates to the shared flag when it is the first stop.
    #[test]
    fn shared_stop_handle_delete_flag() {
        let stop = TransferStop::new();
        stop.stop(true);
        assert!(stop.is_stop_and_delete());
        assert!(stop.stopped_flag().load(Ordering::SeqCst));
    }
}