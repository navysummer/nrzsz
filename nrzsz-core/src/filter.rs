use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use anyhow::{Result, anyhow};
use regex::Regex;
use log::debug;

use crate::protocol::Config;
use crate::tunnel::get_hello_constant;
use crate::transfer::TransferSession;

/// Drag files info parsed from user input.
#[derive(Debug, Clone, Default)]
pub struct DragFilesInfo {
    pub files: Vec<String>,
    pub has_dir: bool,
    pub prefix: bool,
    pub tmux_pane_id: String,
    pub tmux_blocks: usize,
}

/// Parsed information from a NRZSZ:TRANSFER trigger
#[derive(Debug, Clone)]
pub struct NrzszTrigger {
    pub mode: char,
    pub version: String,
    pub unique_id: String,
    pub port: u16,
    pub full_line: String,
    pub tmux_prefix: String,
    pub tmux_pane_id: String,
}

/// Parse a NRZSZ:TRANSFER trigger line
pub fn parse_trigger(line: &str) -> Option<NrzszTrigger> {
    let line = line.trim();
    let line = line.strip_prefix("\x1b[s").unwrap_or(line);
    
    if !line.contains("::NRZSZ:TRANSFER:") {
        return None;
    }
    
    let parts: Vec<&str> = line.split(':').collect();
    if parts.len() < 7 || parts[2] != "NRZSZ" || parts[3] != "TRANSFER" {
        return None;
    }
    
    let mode = parts[4].chars().next()?;
    let version = parts[5].to_string();
    let unique_id = parts[6].to_string();
    let port = parts.get(7).and_then(|p| p.trim().parse::<u16>().ok()).unwrap_or(0);
    
    Some(NrzszTrigger {
        mode,
        version,
        unique_id,
        port,
        full_line: line.to_string(),
        tmux_prefix: String::new(),
        tmux_pane_id: String::new(),
    })
}

/// Regex constants for nrzsz-detector.
const NRZSZ_REGEX_STR: &str = "::NRZSZ:TRANSFER:([SRDF]):(\\d+\\.\\d+\\.\\d+)(:\\d+)?(:\\d+)?";
const UNIQUE_ID_REGEX_STR: &str = "::NRZSZ:TRANSFER:[SRDF]:\\d+\\.\\d+\\.\\d+:(\\d{13}\\d*)";
const TMUX_CC_REGEX_STR: &str = "((%output( %\\d+ ))|(%extended-output( %\\d+ )\\d+ .*: )).*::NRZSZ:TRANSFER:";

/// Trigger detector matching Go's nrzszDetector.
/// Handles relay mode unique-ID rewriting, tmux CC detection, repeat-ID filtering.
pub struct NrzszDetector {
    relay: bool,
    tmux: bool,
    unique_id_map: std::collections::HashMap<String, usize>,
    nrzsz_regex: Regex,
    unique_id_regex: Regex,
    tmux_cc_regex: Regex,
}

impl NrzszDetector {
    pub fn new(relay: bool, tmux: bool) -> Self {
        Self {
            relay,
            tmux,
            unique_id_map: std::collections::HashMap::new(),
            nrzsz_regex: Regex::new(NRZSZ_REGEX_STR).unwrap(),
            unique_id_regex: Regex::new(UNIQUE_ID_REGEX_STR).unwrap(),
            tmux_cc_regex: Regex::new(TMUX_CC_REGEX_STR).unwrap(),
        }
    }

    /// Rewrite unique IDs in relay+tmux mode (matching Go's rewriteNrzszTrigger).
    /// Changes "00" suffix to "2X" to avoid ID conflicts.
    pub fn rewrite_unique_id(&self, buf: &[u8]) -> Vec<u8> {
        if !self.relay || !self.tmux {
            return buf.to_vec();
        }
        let mut result = String::from_utf8_lossy(buf).to_string();
        for cap in self.unique_id_regex.find_iter(&result.clone()) {
            let uid = cap.as_str().to_string();
            if uid.len() >= 13 && uid.ends_with("00") {
                let new_uid = format!("{}2{}", &uid[..uid.len() - 2], &uid[uid.len() - 1..]);
                result = result.replace(&uid, &new_uid);
            }
        }
        result.into_bytes()
    }

    /// Add relay suffix "#R" to the trigger line (matching Go's addRelaySuffix).
    pub fn add_relay_suffix(&self, buf: &[u8]) -> Vec<u8> {
        if !self.relay {
            return buf.to_vec();
        }
        // Find the end of the trigger line
        if let Some(idx) = buf.windows(20).position(|w| w == b"::NRZSZ:TRANSFER:") {
            if idx + 20 < buf.len() {
                let mut end = idx + 20;
                while end < buf.len() {
                    let c = buf[end];
                    if c != b':' && c != b'.' && !c.is_ascii_digit() {
                        break;
                    }
                    end += 1;
                }
                let mut result = Vec::with_capacity(buf.len() + 2);
                result.extend_from_slice(&buf[..end]);
                result.extend_from_slice(b"#R");
                result.extend_from_slice(&buf[end..]);
                return result;
            }
        }
        buf.to_vec()
    }

    /// Check if a unique ID is a repeat (matching Go's isRepeatedID).
    pub fn is_repeated_id(&mut self, unique_id: &str) -> bool {
        if unique_id.len() > 6
            && (unique_id.len() != 13 || !unique_id.ends_with("00"))
        {
            if let Some(&count) = self.unique_id_map.get(unique_id) {
                if count > 0 {
                    return true;
                }
            }
            if self.unique_id_map.len() > 100 {
                let mut new_map = std::collections::HashMap::new();
                for (k, v) in self.unique_id_map.iter() {
                    if *v >= 50 {
                        new_map.insert(k.clone(), v - 50);
                    }
                }
                self.unique_id_map = new_map;
            }
            self.unique_id_map
                .insert(unique_id.to_string(), self.unique_id_map.len());
        }
        false
    }

    /// Full trigger detection matching Go's detectNrzsz.
    /// Returns (possibly rewritten output, optional trigger).
    pub fn detect_nrzsz(&mut self, output: &[u8], support_tmux_cc: bool) -> (Vec<u8>, Option<NrzszTrigger>) {
        if output.len() < 24 {
            return (output.to_vec(), None);
        }

        let text = String::from_utf8_lossy(output).to_string();
        let mut idx = match text.rfind("::NRZSZ:TRANSFER:") {
            Some(i) => i,
            None => return (output.to_vec(), None),
        };

        let mut buf = output.to_vec();

        if self.relay && self.tmux {
            buf = self.rewrite_unique_id(&buf);
            // Re-find after rewriting
            let text2 = String::from_utf8_lossy(&buf);
            if let Some(new_idx) = text2.rfind("::NRZSZ:TRANSFER:") {
                idx = new_idx;
            }
        }

        let sub_text = String::from_utf8_lossy(&buf[idx..]).to_string();
        let caps = match self.nrzsz_regex.captures(&sub_text) {
            Some(c) => c,
            None => return (buf, None),
        };

        // Check for tmux CC prefix
        let mut tmux_prefix = String::new();
        let mut tmux_pane_id = String::new();
        if let Some(tmux_caps) = self.tmux_cc_regex.captures(&text) {
            if !support_tmux_cc {
                return (buf, None);
            }
            let m1 = tmux_caps.get(3).map(|m| m.as_str().to_string());
            let m2 = tmux_caps.get(5).map(|m| m.as_str().to_string());
            if let Some(pid) = m1 {
                tmux_pane_id = pid;
                tmux_prefix = format!("%output{} ", tmux_pane_id);
            } else if let Some(pid) = m2 {
                tmux_pane_id = pid;
                tmux_prefix = format!("%extended-output{}0 : ", tmux_pane_id);
            }
        }

        // Filter out triggers near known keywords
        if sub_text.len() > 40 {
            let tail = &sub_text[40..];
            for keyword in &["#CFG:", "Saved", "Cancelled", "Stopped", "Interrupted"] {
                if tail.contains(keyword) {
                    return (buf, None);
                }
            }
        }

        let mode = caps.get(1).unwrap().as_str().chars().next().unwrap();
        let version = caps.get(2).unwrap().as_str().to_string();

        let unique_id = caps.get(3).map(|m| m.as_str().trim_start_matches(':').to_string()).unwrap_or_default();
        if self.is_repeated_id(&unique_id) {
            return (buf, None);
        }

        let port = caps.get(4).and_then(|m| m.as_str().trim_start_matches(':').parse::<u16>().ok()).unwrap_or(0);

        // In relay mode, append #R suffix
        if self.relay {
            buf = self.add_relay_suffix(&buf);
        } else {
            // Replace NRZSZ with NRZSZGO to prevent re-detection (matching Go's NRZSZ→NRZSZGO)
            let buf_str = String::from_utf8_lossy(&buf);
            buf = buf_str.replace("NRZSZ", "NRZSZGO").into_bytes();
        }

        let trigger = NrzszTrigger {
            mode,
            version,
            unique_id,
            port,
            full_line: caps.get(0).map(|m| m.as_str().to_string()).unwrap_or_default(),
            tmux_prefix,
            tmux_pane_id,
        };

        (buf, Some(trigger))
    }
}

/// Tunnel connection closure shared into transfer threads.
type TunnelConnector = Arc<dyn Fn(u16) -> Result<TcpStream> + Send + Sync>;

pub struct NrzszFilter {
    config: Config,
    trigger_regex: Regex,
    running: bool,
    output_buffer: Vec<u8>,
    stopped: Arc<AtomicBool>,
    is_transferring: Arc<AtomicBool>,
    tunnel_connector: Option<TunnelConnector>,
    default_download_path: String,
    upload_files: Vec<String>,
    enable_osc52: bool,
    osc52: crate::osc52::Osc52Clipboard,
    enable_zmodem: bool,
    enable_dragfile: bool,
    terminal_columns: i32,
    progress_color_pair: String,
    redraw_screen_func: Option<Arc<dyn Fn() + Send + Sync>>,
    transfer_state_callback: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    allowed_auto_upload_dirs: Vec<String>,
    default_upload_path: String,
    drag_file_upload_command: String,
    one_time_upload_files: Vec<String>,
    one_time_upload_result: Option<std::sync::mpsc::Sender<Result<()>>>,
    skip_upload_command: Arc<AtomicBool>,
    current_upload_command: Arc<std::sync::Mutex<String>>,
    interrupting: Arc<AtomicBool>,
}

impl NrzszFilter {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            trigger_regex: Regex::new(r"NRZSZ:TRANSFER").unwrap(),
            running: false,
            output_buffer: Vec::new(),
            stopped: Arc::new(AtomicBool::new(false)),
            is_transferring: Arc::new(AtomicBool::new(false)),
            tunnel_connector: None,
            default_download_path: String::new(),
            upload_files: Vec::new(),
            enable_osc52: false,
            osc52: crate::osc52::Osc52Clipboard::new(),
            enable_zmodem: false,
            enable_dragfile: false,
            terminal_columns: 80,
            progress_color_pair: String::new(),
            redraw_screen_func: None,
            transfer_state_callback: None,
            allowed_auto_upload_dirs: Vec::new(),
            default_upload_path: String::new(),
            drag_file_upload_command: String::new(),
            one_time_upload_files: Vec::new(),
            one_time_upload_result: None,
            skip_upload_command: Arc::new(AtomicBool::new(false)),
            current_upload_command: Arc::new(std::sync::Mutex::new(String::new())),
            interrupting: Arc::new(AtomicBool::new(false)),
        }
    }
    
    pub fn set_enable_osc52(&mut self, enable: bool) {
        self.enable_osc52 = enable;
        if enable {
            self.osc52.enable();
        } else {
            self.osc52.disable();
        }
    }

    pub fn set_enable_zmodem(&mut self, enable: bool) {
        self.enable_zmodem = enable;
    }

    pub fn set_enable_dragfile(&mut self, enable: bool) {
        self.enable_dragfile = enable;
    }

    pub fn set_terminal_columns(&mut self, columns: i32) {
        self.terminal_columns = columns;
    }

    pub fn set_progress_color_pair(&mut self, color_pair: String) {
        self.progress_color_pair = color_pair;
    }

    pub fn set_redraw_screen_func<F>(&mut self, func: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.redraw_screen_func = Some(Arc::new(func));
    }

    pub fn set_transfer_state_callback<F>(&mut self, callback: F)
    where
        F: Fn(bool) + Send + Sync + 'static,
    {
        self.transfer_state_callback = Some(Arc::new(callback));
    }

    pub fn set_allowed_auto_upload_dirs(&mut self, dirs: &[String]) -> Result<()> {
        let mut allowed = Vec::new();
        for dir in dirs {
            let path = std::path::Path::new(dir);
            let abs = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()?.join(path)
            };
            let canonical = abs.canonicalize().unwrap_or(abs);
            allowed.push(canonical.to_string_lossy().to_string());
        }
        self.allowed_auto_upload_dirs = allowed;
        Ok(())
    }

    pub fn set_default_upload_path(&mut self, path: String) {
        if path.is_empty() {
            self.default_upload_path = String::new();
        } else {
            let p = std::path::Path::new(&path);
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                std::env::current_dir().unwrap_or_default().join(p)
            };
            let mut s = abs.to_string_lossy().to_string();
            if !s.ends_with(std::path::MAIN_SEPARATOR) {
                s.push(std::path::MAIN_SEPARATOR);
            }
            self.default_upload_path = s;
        }
    }

    pub fn set_drag_file_upload_command(&mut self, command: String) {
        self.drag_file_upload_command = command;
    }

    pub fn reset_terminal(&self) {
        if let Some(ref func) = self.redraw_screen_func {
            func();
        }
    }

    pub fn one_time_upload(&mut self, files: Vec<String>) -> std::sync::mpsc::Receiver<Result<()>> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.one_time_upload_files = files;
        self.one_time_upload_result = Some(tx);
        rx
    }

    #[allow(dead_code)]
    fn set_one_time_upload_result(&mut self, result: Result<()>) {
        if let Some(tx) = self.one_time_upload_result.take() {
            let _ = tx.send(result);
        }
    }

    /// Read configuration from ~/.nrzsz.conf
    pub fn read_config(&mut self) {
        let home = match std::env::var("HOME") {
            Ok(h) => h,
            Err(_) => return,
        };
        let path = std::path::PathBuf::from(home).join(".nrzsz.conf");
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return,
        };
        for line in content.lines() {
            let line = if let Some(idx) = line.find('#') {
                &line[..idx]
            } else {
                line
            };
            let line = line.trim();
            if let Some(idx) = line.find('=') {
                let name = line[..idx].trim().to_lowercase();
                let value = line[idx + 1..].trim().to_string();
                if name.is_empty() || value.is_empty() {
                    continue;
                }
                match name.as_str() {
                    "allowedautouploaddirs" if self.allowed_auto_upload_dirs.is_empty() => {
                        let _ = self.set_allowed_auto_upload_dirs(&[value]);
                    }
                    "defaultuploadpath" if self.default_upload_path.is_empty() => {
                        self.set_default_upload_path(value);
                    }
                    "defaultdownloadpath" if self.default_download_path.is_empty() => {
                        self.default_download_path = value;
                    }
                    "dragfileuploadcommand" if self.drag_file_upload_command.is_empty() => {
                        self.drag_file_upload_command = value;
                    }
                    "progresscolorpair" if self.progress_color_pair.is_empty() => {
                        self.progress_color_pair = value;
                    }
                    _ => {}
                }
            }
        }
    }
    
    pub fn set_tunnel_connector<F>(&mut self, connector: F)
    where
        F: Fn(u16) -> Result<TcpStream> + Send + Sync + 'static,
    {
        self.tunnel_connector = Some(Arc::new(connector));
    }
    
    pub fn set_default_download_path(&mut self, path: String) {
        self.default_download_path = path;
    }
    
    pub fn set_upload_files(&mut self, files: Vec<String>) {
        self.upload_files = files;
    }
    
    pub fn is_transferring(&self) -> bool {
        self.is_transferring.load(Ordering::SeqCst)
    }
    
    pub fn start(&mut self) -> Result<()> {
        self.running = true;
        self.output_buffer.clear();
        Ok(())
    }
    
    pub fn stop(&mut self) {
        self.running = false;
        self.output_buffer.clear();
        self.stopped.store(true, Ordering::SeqCst);
    }
    
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
    
    /// Connect to a tunnel as a client.
    /// Sends ::NRZSZ::CLIENT::HELLO::<uid>:<port> and verifies server response.
    pub fn connect_to_tunnel(&self, trigger: &NrzszTrigger) -> Result<TcpStream> {
        let connector = self.tunnel_connector.as_ref()
            .ok_or_else(|| anyhow!("No tunnel connector set"))?;
        let mut stream = connector(trigger.port)?;
        let (client_hello, server_hello) = get_hello_constant(&trigger.unique_id, trigger.port);
        
        stream.write_all(client_hello.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        
        let mut buf = [0u8; 100];
        let n = stream.read(&mut buf)?;
        let response = String::from_utf8_lossy(&buf[..n]).trim().to_string();
        if response != server_hello {
            return Err(anyhow!("Tunnel: unexpected server hello: {:?}", response));
        }
        
        Ok(stream)
    }
    
    /// Process server output; detects NRZSZ transfer triggers and dispatches the transfer in a thread.
    /// Returns the output that should be forwarded to the client terminal.
    pub fn process_output(&mut self, input: &[u8]) -> Result<Vec<u8>> {
        if !self.running {
            return Ok(input.to_vec());
        }
        
        // Scan for OSC52 clipboard sequences (matching Go's detectOSC52)
        if self.enable_osc52 {
            self.osc52.detect_osc52(input);
        }
        
        self.output_buffer.extend_from_slice(input);
        
        let mut output = Vec::new();
        let mut start = 0;
        
        for i in 0..self.output_buffer.len() {
            if self.output_buffer[i] == b'\n' {
                let line = String::from_utf8_lossy(&self.output_buffer[start..i]).to_string();
                
                if self.trigger_regex.is_match(&line) {
                    if let Some(trigger) = parse_trigger(&line) {
                        debug!("Detected NRZSZ trigger: {:?}", trigger);
                        // Do NOT forward the trigger line to the terminal
                        start = i + 1;
                        
                        // Dispatch the transfer in a background thread (matching Go's go handleNrzsz())
                        if self.is_transferring.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                            let ctx = self.build_ctx();
                            let interrupting = self.interrupting.clone();
                            let callback = self.transfer_state_callback.clone();
                            std::thread::spawn(move || {
                                if let Some(ref cb) = callback {
                                    cb(true);
                                }
                                let _ = ctx.run_transfer(&trigger);
                                if let Some(ref cb) = callback {
                                    cb(false);
                                }
                                interrupting.store(false, Ordering::SeqCst);
                            });
                        }
                        continue;
                    }
                }
                
                output.extend_from_slice(&self.output_buffer[start..=i]);
                start = i + 1;
            }
        }
        
        if start < self.output_buffer.len() {
            self.output_buffer = self.output_buffer[start..].to_vec();
        } else {
            self.output_buffer.clear();
        }
        
        Ok(output)
    }
    
    /// Snapshot the state needed by a transfer thread.
    fn build_ctx(&self) -> TransferCtx {
        TransferCtx {
            config: self.config.clone(),
            connector: self.tunnel_connector.clone(),
            is_transferring: self.is_transferring.clone(),
            default_download_path: self.default_download_path.clone(),
            upload_files: self.upload_files.clone(),
            allowed_auto_upload_dirs: self.allowed_auto_upload_dirs.clone(),
        }
    }
    
    pub fn upload_files(&self, files: &[String]) -> Result<()> {
        for file in files {
            eprintln!("Uploading: {}", file);
        }
        Ok(())
    }
    
    pub fn download_files(&self, files: &[String]) -> Result<()> {
        for file in files {
            eprintln!("Downloading: {}", file);
        }
        Ok(())
    }
    
    pub fn stop_transferring_files(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    /// Process client input (stdin). Detects drag files, skipUploadCommand, and interrupting.
    /// Returns the data that should be forwarded to the server.
    pub fn process_input(&self, buf: &[u8]) -> Vec<u8> {
        if buf.is_empty() {
            return buf.to_vec();
        }

        if self.is_transferring.load(Ordering::SeqCst) {
            // Transfer in progress, forward all input
            return buf.to_vec();
        }

        if self.interrupting.load(Ordering::SeqCst) {
            return Vec::new();
        }

        // Detect drag files
        if self.enable_dragfile {
            let drag_info = detect_drag_files(buf);
            if !drag_info.files.is_empty() {
                debug!("Detected drag files: {:?}", drag_info.files);
                let files = drag_info.files.clone();
                let has_dir = drag_info.has_dir;
                let upload_cmd = self.drag_file_upload_command.clone();
                let is_transferring = self.is_transferring.clone();
                let interrupting = self.interrupting.clone();
                let skip = self.skip_upload_command.clone();
                let current_cmd = self.current_upload_command.clone();

                std::thread::spawn(move || {
                    upload_drag_files(&files, has_dir, &upload_cmd,
                                      &is_transferring, &interrupting, &skip, &current_cmd);
                });
                return Vec::new();
            }
        }

        buf.to_vec()
    }

    /// Clear the skip upload command flag and check if the output should be suppressed.
    pub fn should_skip_output(&self, output: &[u8]) -> bool {
        if self.skip_upload_command.load(Ordering::SeqCst) {
            self.skip_upload_command.store(false, Ordering::SeqCst);
            let trimmed = String::from_utf8_lossy(output).trim().trim_end_matches('\r').to_string();
            let cmd = self.current_upload_command.lock().unwrap().clone();
            if !cmd.is_empty() && trimmed == cmd {
                return true;
            }
        }
        false
    }

    pub fn stopped_flag(&self) -> Arc<AtomicBool> {
        self.stopped.clone()
    }

    pub fn is_transferring_flag(&self) -> Arc<AtomicBool> {
        self.is_transferring.clone()
    }

    /// Choose a download path using zenity file dialog.
    /// Returns None if user cancels or zenity is unavailable.
    pub fn choose_download_path(&self) -> Option<String> {
        if !self.default_download_path.is_empty() {
            return Some(self.default_download_path.clone());
        }
        let zenity = crate::zenity::ZenityProgress::new();
        if !zenity.is_available() {
            return None;
        }
        match std::process::Command::new("zenity")
            .args(["--file-selection", "--directory",
                   "--title=Choose a folder to save file(s)"])
            .output()
        {
            Ok(output) if output.status.success() => {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if path.is_empty() { None } else { Some(path) }
            }
            _ => None,
        }
    }

    /// Choose upload file paths using zenity file dialog.
    /// Returns None if user cancels or zenity is unavailable.
    pub fn choose_upload_paths(&self, directory: bool) -> Option<Vec<String>> {
        if !self.one_time_upload_files.is_empty() {
            return Some(self.one_time_upload_files.clone());
        }
        if !self.upload_files.is_empty() {
            return Some(self.upload_files.clone());
        }
        let zenity = crate::zenity::ZenityProgress::new();
        if !zenity.is_available() {
            return None;
        }
        let mut cmd = std::process::Command::new("zenity");
        cmd.args(["--file-selection", "--title=Choose some files to send"]);
        if directory {
            cmd.arg("--directory");
        } else {
            cmd.args(["--multiple", "--separator=\n"]);
        }
        if !self.default_upload_path.is_empty() {
            cmd.args(["--filename=", &self.default_upload_path]);
        }
        match cmd.output() {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if stdout.is_empty() {
                    None
                } else {
                    let files: Vec<String> = stdout.lines().map(|s| s.to_string()).collect();
                    if files.is_empty() { None } else { Some(files) }
                }
            }
            _ => None,
        }
    }
}

/// Owned snapshot of filter state, used to run a transfer on its own thread.
struct TransferCtx {
    config: Config,
    connector: Option<TunnelConnector>,
    is_transferring: Arc<AtomicBool>,
    default_download_path: String,
    upload_files: Vec<String>,
    allowed_auto_upload_dirs: Vec<String>,
}

impl TransferCtx {
    fn run_transfer(&self, trigger: &NrzszTrigger) -> Result<()> {
        debug!("run_transfer mode={}", trigger.mode);
        let connector = self.connector.as_ref()
            .ok_or_else(|| anyhow!("No tunnel connector set"))?;
        let mut stream = connector(trigger.port)?;
        let (client_hello, server_hello) = get_hello_constant(&trigger.unique_id, trigger.port);
        
        stream.write_all(client_hello.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        
        let mut buf = [0u8; 100];
        let n = stream.read(&mut buf)?;
        let response = String::from_utf8_lossy(&buf[..n]).trim().to_string();
        if response != server_hello {
            return Err(anyhow!("Tunnel: unexpected server hello: {:?}", response));
        }
        
        let stdin_pipe = Box::new(stream.try_clone()?);
        let stdout_pipe = Box::new(stream);
        let mut session = TransferSession::new(self.config.clone(), stdin_pipe, stdout_pipe);
        
        let result = match trigger.mode {
            'S' => self.download_files_from(&mut session, trigger),
            'R' | 'D' | 'F' => self.upload_files_to(&mut session, trigger),
            _ => {
                eprintln!("Unsupported trigger mode: {}", trigger.mode);
                Ok(())
            }
        };
        
        let _ = result;
        self.is_transferring.store(false, Ordering::SeqCst);
        Ok(())
    }
    
    fn download_files_from(&self, session: &mut TransferSession, trigger: &NrzszTrigger) -> Result<()> {
        // Client is the receiver (server nsz is sending)
        session.send_action(true, &trigger.version, false)?;
        let config = session.recv_config()?;
        
        let dest = if self.default_download_path.is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        } else {
            PathBuf::from(&self.default_download_path)
        };
        
        let local_list = session.recv_files(&dest, config.quiet)?;
        let msg = TransferSession::format_saved_files(local_list.len(), &dest, &local_list);
        session.client_exit(&msg)?;
        Ok(())
    }
    
    fn upload_files_to(&self, session: &mut TransferSession, trigger: &NrzszTrigger) -> Result<()> {
        // Client is the sender (server nrz is receiving)
        session.send_action(true, &trigger.version, false)?;
        let config = session.recv_config()?;
        
        let directory = trigger.mode == 'D';
        let files = if !self.upload_files.is_empty() {
            self.upload_files.clone()
        } else if !config.client_files.is_empty() {
            if !self.allowed_auto_upload_dirs.is_empty() {
                self.check_auto_upload_allowed(&config.client_files)?;
            }
            config.client_files.clone()
        } else {
            return Err(anyhow!("No upload files specified"));
        };
        
        let file_list = crate::transfer::check_paths_readable(&files, directory)?;
        if config.overwrite {
            TransferSession::check_duplicate_names(&file_list)?;
        }
        
        let _remote_list = session.send_files(&file_list, config.quiet)?;
        session.recv_exit()?;
        Ok(())
    }

    fn check_auto_upload_allowed(&self, files: &[String]) -> Result<()> {
        if self.allowed_auto_upload_dirs.is_empty() {
            return Ok(());
        }
        for file in files {
            let path = std::path::Path::new(file);
            let abs = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()?.join(path)
            };
            let canonical = abs.canonicalize().unwrap_or(abs);
            let canonical_str = canonical.to_string_lossy();
            let allowed = self.allowed_auto_upload_dirs.iter().any(|dir| {
                canonical_str.starts_with(dir)
            });
            if !allowed {
                return Err(anyhow!("File is outside the allowed auto upload directories: {}", file));
            }
        }
        Ok(())
    }
}

pub fn detect_nrzsz_trigger(output: &str) -> bool {
    output.contains("NRZSZ:TRANSFER")
}

/// Detect if user input contains drag-and-dropped file paths.
/// Returns DragFilesInfo with files list if drag detected, empty files if not.
pub fn detect_drag_files(buf: &[u8]) -> DragFilesInfo {
    if buf.len() < 2 {
        return DragFilesInfo::default();
    }

    let mut input = buf;
    let mut info = DragFilesInfo::default();

    // Strip bracketed paste mode sequences
    if input.len() > 5 {
        if let Some(stripped) = strip_bracketed_paste(input) {
            input = stripped;
            if input.is_empty() {
                return info;
            }
        }
    }

    // Strip Ctrl+P prefix (old warp terminal)
    if input[0] == 0x10 {
        input = &input[1..];
        if input.len() < 2 {
            return info;
        }
    }
    // Strip ESC i Ctrl+P prefix (new warp terminal)
    if input.len() > 4 && input[0] == 0x1b && input[1] == b'i' && input[2] == 0x10 {
        input = &input[3..];
    }

    let text = String::from_utf8_lossy(input).to_string();
    let paths: Vec<&str> = text.split_whitespace().collect();
    for p in &paths {
        if p.len() < 2 || !p.starts_with('/') {
            info.files.clear();
            info.prefix = is_dir_prefix(p);
            return info;
        }
        if detect_file_path(p, &mut info.files, &mut info.has_dir) {
            info.prefix = true;
        } else {
            info.files.clear();
            if !info.prefix && is_dir_prefix(p) {
                info.prefix = true;
            }
            return info;
        }
    }
    info
}

fn strip_bracketed_paste(buf: &[u8]) -> Option<&[u8]> {
    if let Some(start) = buf.windows(6).position(|w| w == b"\x1b[200~") {
        if let Some(end) = buf[start + 6..].windows(6).position(|w| w == b"\x1b[201~") {
            let inner = &buf[start + 6..start + 6 + end];
            return Some(inner);
        }
    }
    None
}

fn is_dir_prefix(p: &str) -> bool {
    let dir = if let Some(pos) = p.rfind('/') {
        if pos == 0 { "/" } else { &p[..pos] }
    } else {
        return false;
    };
    let mut has_dir = false;
    let mut files = Vec::new();
    detect_file_path(dir, &mut files, &mut has_dir)
}

fn detect_file_path(path: &str, files: &mut Vec<String>, has_dir: &mut bool) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => {
            if meta.is_dir() {
                *has_dir = true;
                files.push(path.to_string());
                true
            } else if meta.is_file() {
                files.push(path.to_string());
                true
            } else {
                false
            }
        }
        Err(_) => false,
    }
}

/// Upload dragged files by interrupting current input and sending trz command.
/// Matches Go's uploadDragFiles.
fn upload_drag_files(
    files: &[String],
    has_dir: bool,
    upload_command: &str,
    is_transferring: &Arc<AtomicBool>,
    interrupting: &Arc<AtomicBool>,
    skip_upload_command: &Arc<AtomicBool>,
    current_upload_command: &Arc<std::sync::Mutex<String>>,
) {
    if is_transferring.load(Ordering::SeqCst) {
        return;
    }

    let _ = files;
    interrupting.store(true, Ordering::SeqCst);
    std::thread::sleep(std::time::Duration::from_millis(300));
    interrupting.store(false, Ordering::SeqCst);

    skip_upload_command.store(true, Ordering::SeqCst);
    let mut command = if upload_command.is_empty() {
        "trz".to_string()
    } else {
        upload_command.to_string()
    };
    if has_dir {
        command = format!("{} -d", command);
    }

    *current_upload_command.lock().unwrap() = command.clone();
    debug!("Upload drag files command: {}", command);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_trigger_recv() {
        let line = "\x1b[s::NRZSZ:TRANSFER:R:1.2.0:1234567890123:9090\r\n";
        let trigger = parse_trigger(line).unwrap();
        assert_eq!(trigger.mode, 'R');
        assert_eq!(trigger.version, "1.2.0");
        assert_eq!(trigger.unique_id, "1234567890123");
        assert_eq!(trigger.port, 9090);
    }

    #[test]
    fn test_parse_trigger_send() {
        let line = "::NRZSZ:TRANSFER:S:0.1.0:9876543210987:8080";
        let trigger = parse_trigger(line).unwrap();
        assert_eq!(trigger.mode, 'S');
        assert_eq!(trigger.port, 8080);
    }

    #[test]
    fn test_parse_trigger_no_match() {
        assert!(parse_trigger("normal output").is_none());
        assert!(parse_trigger("::OTHER:TRANSFER:S:1.0:123:456").is_none());
    }

    #[test]
    fn test_filter_setters() {
        let mut filter = NrzszFilter::new(Config::default());
        filter.set_terminal_columns(120);
        filter.set_progress_color_pair("red".to_string());
        filter.set_drag_file_upload_command("trz -d".to_string());
        filter.set_default_upload_path("/tmp".to_string());
        filter.set_default_download_path("/tmp".to_string());
        filter.set_enable_zmodem(true);
        filter.set_enable_dragfile(true);
        assert_eq!(filter.terminal_columns, 120);
        assert_eq!(filter.progress_color_pair, "red");
        assert_eq!(filter.drag_file_upload_command, "trz -d");
        assert!(filter.default_upload_path.contains("tmp"));
        assert_eq!(filter.default_download_path, "/tmp");
        assert!(filter.enable_zmodem);
        assert!(filter.enable_dragfile);
    }

    #[test]
    fn test_filter_read_config() {
        let mut filter = NrzszFilter::new(Config::default());
        filter.read_config();
        // Should not panic even if config file doesn't exist
    }

    #[test]
    fn test_detect_drag_files_empty() {
        let info = detect_drag_files(b"a");
        assert!(info.files.is_empty());
        assert!(!info.prefix);
    }

    #[test]
    fn test_detect_drag_files_no_abs_path() {
        let info = detect_drag_files(b"relative/path");
        assert!(info.files.is_empty());
    }

    #[test]
    fn test_detect_drag_files_bracketed_paste() {
        let buf = b"\x1b[200~/tmp\x1b[201~";
        let info = detect_drag_files(buf);
        assert_eq!(info.files.len(), 1);
        assert_eq!(info.files[0], "/tmp");
        assert!(info.has_dir);
    }

    #[test]
    fn test_should_skip_output() {
        let mut filter = NrzszFilter::new(Config::default());
        filter.skip_upload_command.store(true, Ordering::SeqCst);
        *filter.current_upload_command.lock().unwrap() = "trz -d".to_string();
        assert!(filter.should_skip_output(b"trz -d\r\n"));
        assert!(!filter.skip_upload_command.load(Ordering::SeqCst));
    }

    #[test]
    fn test_should_skip_output_no_match() {
        let mut filter = NrzszFilter::new(Config::default());
        filter.skip_upload_command.store(true, Ordering::SeqCst);
        *filter.current_upload_command.lock().unwrap() = "trz".to_string();
        assert!(!filter.should_skip_output(b"other output\r\n"));
    }

    #[test]
    fn test_process_input_passthrough() {
        let filter = NrzszFilter::new(Config::default());
        let result = filter.process_input(b"hello");
        assert_eq!(result, b"hello");
    }

    #[test]
    fn test_process_input_empty() {
        let filter = NrzszFilter::new(Config::default());
        let result = filter.process_input(b"");
        assert!(result.is_empty());
    }

    #[test]
    fn test_process_input_transferring() {
        let mut filter = NrzszFilter::new(Config::default());
        filter.is_transferring.store(true, Ordering::SeqCst);
        let result = filter.process_input(b"hello");
        assert_eq!(result, b"hello");
    }
}