use std::io::Write;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
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
/// Parse a TRZSZ/NRZSZ:TRANSFER trigger line.
/// Accepts both the trzsz-go `::TRZSZ:` namespace and the legacy `::NRZSZ:` namespace.
pub fn parse_trigger(line: &str) -> Option<NrzszTrigger> {
    let line = line.trim();
    let line = line.strip_prefix("\x1b[s").unwrap_or(line);

    // Strip a tmux control-mode prefix: `%output <id> ` or
    // `%extended-output <id> 0 : `. The trigger line arrives wrapped by tmux
    // when running under `tmux -CC`; the pane id is needed to route the reply.
    let mut tmux_prefix = String::new();
    let mut tmux_pane_id = String::new();
    let rest = if let Some(r) = line.strip_prefix("%output") {
        let r = r.trim_start();
        let pane_id = r.split_whitespace().next().unwrap_or("").to_string();
        tmux_pane_id = pane_id.clone();
        tmux_prefix = format!("%output {} ", pane_id);
        r.split_whitespace().skip(1).collect::<Vec<_>>().join(" ")
    } else if let Some(r) = line.strip_prefix("%extended-output") {
        let r = r.trim_start();
        let pane_id = r.split_whitespace().next().unwrap_or("").to_string();
        tmux_pane_id = pane_id.clone();
        tmux_prefix = format!("%extended-output {} 0 : ", pane_id);
        r.split_whitespace().skip(2).collect::<Vec<_>>().join(" ")
    } else {
        line.to_string()
    };
    let rest = rest.trim();

    let is_trigger = rest.contains("::TRZSZ:TRANSFER:") || rest.contains("::NRZSZ:TRANSFER:");
    if !is_trigger {
        return None;
    }

    let parts: Vec<&str> = rest.split(':').collect();
    if parts.len() < 7
        || !(parts[2] == "TRZSZ" || parts[2] == "NRZSZ")
        || parts[3] != "TRANSFER"
    {
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
        full_line: rest.to_string(),
        tmux_prefix,
        tmux_pane_id,
    })
}

/// Regex constants for nrzsz-detector.
const NRZSZ_REGEX_STR: &str = "::(?:TRZSZ|NRZSZ):TRANSFER:([SRDF]):(\\d+\\.\\d+\\.\\d+)(:\\d+)?(:\\d+)?";
const UNIQUE_ID_REGEX_STR: &str = "::(?:TRZSZ|NRZSZ):TRANSFER:[SRDF]:\\d+\\.\\d+\\.\\d+:(\\d{13}\\d*)";
const TMUX_CC_REGEX_STR: &str = "((%output( %\\d+ ))|(%extended-output( %\\d+ )\\d+ .*: )).*::(?:TRZSZ|NRZSZ):TRANSFER:";



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
        // Find the end of the trigger line (either namespace)
        let ns_idx = buf.windows(20).position(|w| w == b"::TRZSZ:TRANSFER:")
            .or_else(|| buf.windows(20).position(|w| w == b"::NRZSZ:TRANSFER:"));
        if let Some(idx) = ns_idx {
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
        let mut idx = match text.rfind("::TRZSZ:TRANSFER:").or_else(|| text.rfind("::NRZSZ:TRANSFER:")) {
            Some(i) => i,
            None => return (output.to_vec(), None),
        };

        let mut buf = output.to_vec();

        if self.relay && self.tmux {
            buf = self.rewrite_unique_id(&buf);
            // Re-find after rewriting
            let text2 = String::from_utf8_lossy(&buf);
            if let Some(new_idx) = text2.rfind("::TRZSZ:TRANSFER:").or_else(|| text2.rfind("::NRZSZ:TRANSFER:")) {
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
            buf = buf_str.replace("TRZSZ", "TRZSZGO").replace("NRZSZ", "NRZSZGO").into_bytes();
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
    upload_command_is_not_trz: bool,
    one_time_upload_files: Arc<std::sync::Mutex<Vec<String>>>,
    one_time_upload_result: Arc<std::sync::Mutex<Option<std::sync::mpsc::Sender<Result<()>>>>>,
    skip_upload_command: Arc<AtomicBool>,
    current_upload_command: Arc<std::sync::Mutex<String>>,
    interrupting: Arc<AtomicBool>,
    zmodem: Option<Arc<crate::zmodem::ZmodemState>>,
    zmodem_auto_rename: bool,
    client_out: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    server_in: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    active_stop: Arc<std::sync::Mutex<Option<Arc<crate::transfer::TransferStop>>>>,
    drag_files: Arc<std::sync::Mutex<Vec<String>>>,
}

impl NrzszFilter {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            trigger_regex: Regex::new(r"(?:TRZSZ|NRZSZ):TRANSFER").unwrap(),
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
            upload_command_is_not_trz: false,
            one_time_upload_files: Arc::new(std::sync::Mutex::new(Vec::new())),
            one_time_upload_result: Arc::new(std::sync::Mutex::new(None)),
            skip_upload_command: Arc::new(AtomicBool::new(false)),
            current_upload_command: Arc::new(std::sync::Mutex::new(String::new())),
            interrupting: Arc::new(AtomicBool::new(false)),
            zmodem: None,
            zmodem_auto_rename: false,
            client_out: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            server_in: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            active_stop: Arc::new(std::sync::Mutex::new(None)),
            drag_files: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    pub fn set_zmodem_auto_rename(&mut self, enable: bool) {
        self.zmodem_auto_rename = enable;
    }

    pub fn set_server_in(&mut self, writer: Arc<Mutex<Box<dyn std::io::Write + Send>>>) {
        self.server_in = writer;
    }

    pub fn set_client_out(&mut self, writer: Arc<Mutex<Box<dyn std::io::Write + Send>>>) {
        self.client_out = writer;
    }

    
    #[cfg(test)]
    pub(crate) fn osc52_enabled(&self) -> bool { self.enable_osc52 }
    #[cfg(test)]
    pub(crate) fn zmodem_enabled(&self) -> bool { self.enable_zmodem }
    #[cfg(test)]
    pub(crate) fn dragfile_enabled(&self) -> bool { self.enable_dragfile }

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

    pub fn set_drag_files(&mut self, files: Vec<String>) {
        let mut guard = self.drag_files.lock().unwrap();
        *guard = files;
    }

    pub fn set_terminal_columns(&mut self, columns: i32) {
        self.terminal_columns = columns;
    }

    pub fn set_progress_color_pair(&mut self, color_pair: String) {
        self.progress_color_pair = color_pair;
        // Populate the transfer Config so the gradient reaches the progress bar
        // (matching Go's newTextProgressBar, which splits on whitespace).
        let colors: Vec<&str> = self.progress_color_pair.split_whitespace().collect();
        if colors.len() == 2 {
            self.config.progress_color_pair = (colors[0].to_string(), colors[1].to_string());
        }
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
        // Go: uploadCommandIsNotTrz is true when the command's base name is not
        // `trz`/`trz.exe` (filter.go SetDragFileUploadCommand). Compute before
        // moving `command` into the field.
        let base = std::path::Path::new(&command)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        self.upload_command_is_not_trz = !base.is_empty() && base != "trz" && base != "trz.exe";
        self.drag_file_upload_command = command;
    }

    pub fn reset_terminal(&self) {
        if let Some(ref func) = self.redraw_screen_func {
            func();
        }
    }

    pub fn one_time_upload(&mut self, files: Vec<String>) -> std::sync::mpsc::Receiver<Result<()>> {
        let (tx, rx) = std::sync::mpsc::channel();
        *self.one_time_upload_files.lock().unwrap() = files;
        *self.one_time_upload_result.lock().unwrap() = Some(tx);
        // Go's OneTimeUpload: a 10s watchdog that reports "the upload did not
        // start" if the result channel is still pending (trzsz not installed /
        // no trigger seen).
        let result_slot = self.one_time_upload_result.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(10));
            let tx = {
                let mut guard = result_slot.lock().unwrap();
                guard.take()
            };
            if let Some(tx) = tx {
                let _ = tx.send(Err(anyhow!(
                    "the upload did not start, possibly because trzsz is not installed or trz is not found on the server"
                )));
            }
        });
        rx
    }

    /// Resolve the pending one-time upload result, if any. Called by the transfer
    /// thread once an R/D/F upload completes (Go's `setOneTimeUploadResult`).
    pub fn set_one_time_upload_result(&self, result: Result<()>) {
        if let Some(tx) = self.one_time_upload_result.lock().unwrap().take() {
            let _ = tx.send(result);
        }
    }

    /// Read configuration from ~/.trzsz.conf (Go-compatible) or ~/.nrzsz.conf.
    /// Go's TrzszFilter.readTrzszConfig() reads ~/.trzsz.conf; we prefer it for
    /// cross-tool compatibility and fall back to our own ~/.nrzsz.conf.
    pub fn read_config(&mut self) {
        let home = match std::env::var("HOME") {
            Ok(h) => h,
            Err(_) => return,
        };
        let trzsz_path = std::path::PathBuf::from(&home).join(".trzsz.conf");
        let nrzsz_path = std::path::PathBuf::from(&home).join(".nrzsz.conf");
        let content = match std::fs::read_to_string(&trzsz_path) {
            Ok(c) => c,
            Err(_) => match std::fs::read_to_string(&nrzsz_path) {
                Ok(c) => c,
                Err(_) => return,
            },
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
                        // Go shlex-splits a space-separated list of dirs (filter.go SetAllowedAutoUploadDirs).
                        let dirs: Vec<String> = value.split_whitespace().map(|s| s.to_string()).collect();
                        let _ = self.set_allowed_auto_upload_dirs(&dirs);
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
                    "zmodemautorename" => {
                        match value.to_lowercase().as_str() {
                            "on" | "true" | "yes" | "1" => self.set_zmodem_auto_rename(true),
                            "off" | "false" | "no" | "0" => self.set_zmodem_auto_rename(false),
                            _ => debug!("ZmodemAutoRename {:?} is invalid", value),
                        }
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
        
        // Verify the server hello line-at-a-time (a fixed-size read can leave
        // a stray newline that desyncs the next protocol frame).
        let response = crate::tunnel::read_line_from_stream(&mut stream)?.trim().to_string();
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

        // Zmodem passthrough: route server output to an active transfer, or
        // detect the ZMODEM init sequence and launch the local rz/sz client.
        if self.zmodem.is_some() {
            let keep = {
                let z = self.zmodem.as_ref().unwrap();
                z.handle_server_output(input)
            };
            if !keep {
                self.zmodem = None;
            }
            return Ok(input.to_vec());
        }
        if self.enable_zmodem {
            if let Some(zmodem) = crate::zmodem::detect_zmodem(
                input,
                self.zmodem_auto_rename,
                self.server_in.clone(),
                self.client_out.clone(),
            ) {
                debug!("Detected ZMODEM init (upload={})", zmodem.is_upload());
                let state = zmodem.state.clone();
                let choose_upload = {
                    let upload_files = self.upload_files.clone();
                    let one_time = self.one_time_upload_files.lock().unwrap().clone();
                    let default_upload = self.default_upload_path.clone();
                    let drag_cmd = self.drag_file_upload_command.clone();
                    let allowed = self.allowed_auto_upload_dirs.clone();
                    move || -> Result<Vec<String>> {
                        // Use pre-set upload files, else show a file dialog.
                        if !upload_files.is_empty() {
                            return Ok(upload_files.clone());
                        }
                        if !one_time.is_empty() {
                            return Ok(one_time.clone());
                        }
                        crate::zmodem::choose_upload_files(drag_cmd.clone(), allowed.clone(), default_upload.clone())
                    }
                };
                let choose_download = {
                    let default_download = self.default_download_path.clone();
                    move || -> Result<String> {
                        if !default_download.is_empty() {
                            return Ok(default_download.clone());
                        }
                        crate::zmodem::choose_download_path()
                    }
                };
                let interrupting = self.interrupting.clone();
                let callback = self.transfer_state_callback.clone();
                let state2 = state.clone();
                std::thread::spawn(move || {
                    if let Some(ref cb) = callback {
                        cb(true);
                    }
                    crate::zmodem::handle_zmodem_event(state2, choose_upload, choose_download);
                    if let Some(ref cb) = callback {
                        cb(false);
                    }
                    interrupting.store(false, Ordering::SeqCst);
                });
                self.zmodem = Some(state);
                return Ok(input.to_vec());
            }
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
            active_stop: self.active_stop.clone(),
            one_time_files: self.one_time_upload_files.clone(),
            one_time_result: self.one_time_upload_result.clone(),
            drag_files: self.drag_files.clone(),
        }
    }
    
    /// Stop the in-flight transfer (keep already-received files). No-op if no
    /// transfer is running. Matches Go's `filter.StopTransferringFiles(false)`.
    pub fn stop_transferring_files(&self) {
        self.stop_transferring(false);
    }

    /// Stop the in-flight transfer, optionally deleting files created so far.
    /// No-op if no transfer is running. The wrapper itself keeps running (Go's
    /// `filter.StopTransferringFiles` stops only the active transfer).
    pub fn stop_transferring(&self, stop_and_delete: bool) {
        if let Some(handle) = self.active_stop.lock().unwrap().clone() {
            handle.stop(stop_and_delete);
        }
    }

    /// Process client input (stdin). Detects drag files, skipUploadCommand, and interrupting.
    /// Returns the data that should be forwarded to the server.
    pub fn process_input(&self, buf: &[u8]) -> Vec<u8> {
        if buf.is_empty() {
            return buf.to_vec();
        }

        // Zmodem passthrough: forward Ctrl+C to the active transfer to cancel it.
        if let Some(ref z) = self.zmodem {
            if buf.contains(&0x03) {
                z.stop_transferring_files();
            }
            return buf.to_vec();
        }

        if self.is_transferring.load(Ordering::SeqCst) {
            // A local Ctrl+C stops the transfer in place instead of being
            // forwarded to the remote (Go's handleClientInput -> handleInputCtrlC).
            // Raw (non-tmux) mode: a bare 0x03 byte. Tmux `-CC` control mode:
            // a `send -t %N 0x3` control command that must be decoded first.
            let raw_ctrl = buf.contains(&0x03);
            let tmux_cc_ctrl = !raw_ctrl
                && buf.first() == Some(&b's')
                && {
                    let (decoded, _) = crate::tmuxcc::decode_tmux_input(buf);
                    decoded.iter().any(|&b| b == 0x03)
                };
            if raw_ctrl || tmux_cc_ctrl {
                self.stop_transferring(false);
                // Emit tmux-CC %begin/%end ack so progress UI desyncs (matching Go's tmuxcc.go:198-206)
                if tmux_cc_ctrl {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let ack = format!("%begin {} 1 1\r\n%end {} 1 1\r\n", now, now);
                    if let Ok(mut w) = self.client_out.lock() {
                        let _ = w.write_all(ack.as_bytes());
                        let _ = w.flush();
                    }
                }
            }
            // Raw: drop the Ctrl+C byte, forward the rest. Tmux-CC: the
            // `send 0x3` command is a local stop, so it is consumed, not
            // forwarded to the server.
            if tmux_cc_ctrl {
                return Vec::new();
            }
            if raw_ctrl {
                return buf.iter().filter(|&&b| b != 0x03).copied().collect::<Vec<u8>>();
            }
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
                // Store drag files so upload_files_to can pick them up (matching Go's addDragFiles)
                let mut guard = self.drag_files.lock().unwrap();
                *guard = drag_info.files.clone();
                let has_dir = drag_info.has_dir;
                let tmux_pane_id = drag_info.tmux_pane_id.clone();
                let tmux_blocks = drag_info.tmux_blocks;
                let upload_cmd = self.drag_file_upload_command.clone();
                let cmd_not_trz = self.upload_command_is_not_trz;
                let is_transferring = self.is_transferring.clone();
                let interrupting = self.interrupting.clone();
                let skip = self.skip_upload_command.clone();
                let current_cmd = self.current_upload_command.clone();
                let server_in = self.server_in.clone();

                if !self.is_transferring.load(Ordering::SeqCst) {
                    std::thread::spawn(move || {
                        upload_drag_files(&drag_info.files, has_dir, &upload_cmd, cmd_not_trz,
                                          &is_transferring, &interrupting, &skip, &current_cmd,
                                          server_in, &tmux_pane_id, tmux_blocks);
                    });
                }
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
        if !self.one_time_upload_files.lock().unwrap().is_empty() {
            return Some(self.one_time_upload_files.lock().unwrap().clone());
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
    active_stop: Arc<std::sync::Mutex<Option<Arc<crate::transfer::TransferStop>>>>,
    one_time_files: Arc<std::sync::Mutex<Vec<String>>>,
    one_time_result: Arc<std::sync::Mutex<Option<std::sync::mpsc::Sender<Result<()>>>>>,
    drag_files: Arc<std::sync::Mutex<Vec<String>>>,
}

impl TransferCtx {
    fn run_transfer(&self, trigger: &NrzszTrigger) -> Result<()> {
        debug!("run_transfer mode={}", trigger.mode);

        // In tmux `-CC` control mode, all terminal output (progress bar, final
        // messages, stop prompts) written to process stdout must be wrapped in
        // tmux `%output <pane> ` messages. The transfer code writes to
        // `std::io::stdout()` directly (transfer.rs), so we redirect fd 1
        // through an encoder thread for the whole transfer (matching Go's
        // per-write `encodeTmuxOutput` in progress.go and transfer.go).
        let _tmux_redirect = if trigger.tmux_prefix.is_empty() {
            None
        } else {
            match crate::tmuxcc::TmuxOutputRedirect::start(&trigger.tmux_prefix) {
                Ok(r) => Some(r),
                Err(e) => {
                    debug!("tmux-CC stdout redirect failed, continuing without it: {}", e);
                    None
                }
            }
        };

        let connector = self.connector.as_ref()
            .ok_or_else(|| anyhow!("No tunnel connector set"))?;
        let mut stream = connector(trigger.port)?;
        let (client_hello, server_hello) = get_hello_constant(&trigger.unique_id, trigger.port);
        
        stream.write_all(client_hello.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        
        // Read the server hello line by line (byte-at-a-time until `\n`), so we
        // never over-read into the next frame. A fixed `read` into a buffer can
        // consume part of the following protocol message, desyncing the stream.
        let response = crate::tunnel::read_line_from_stream(&mut stream)?;
        let response = response.trim().to_string();
        if response != server_hello {
            return Err(anyhow!("Tunnel: unexpected server hello: {:?}", response));
        }
        
        let stdin_pipe = Box::new(stream.try_clone()?);
        let stdout_pipe = Box::new(stream);
        // Register a fresh stop handle so the wrapper (SIGINT / tmux-CC Ctrl+C) can
        // stop this session in place (Go's filter.StopTransferringFiles -> stopTransferringFiles).
        let stop = crate::transfer::TransferStop::new();
        *self.active_stop.lock().unwrap() = Some(stop.clone());
        let mut session = TransferSession::new_with_stop(self.config.clone(), stdin_pipe, stdout_pipe, stop);
        
        let is_upload = match trigger.mode {
            'R' | 'D' | 'F' => true,
            _ => false,
        };
        let result = match trigger.mode {
            'S' => self.download_files_from(&mut session, trigger),
            'R' | 'D' | 'F' => self.upload_files_to(&mut session, trigger),
            _ => {
                eprintln!("Unsupported trigger mode: {}", trigger.mode);
                Ok(())
            }
        };

        // Resolve the pending one-time upload result (Go's setOneTimeUploadResult):
        // the receiver gets Ok(()) or the upload error. The 10s watchdog in
        // one_time_upload() has already resolved the "never started" case.
        if is_upload {
            if let Some(tx) = self.one_time_result.lock().unwrap().take() {
                let _ = tx.send(result);
            }
        }

        // Clear the active handle once the session is finished.
        *self.active_stop.lock().unwrap() = None;
        self.is_transferring.store(false, Ordering::SeqCst);
        Ok(())
    }
    
    fn download_files_from(&self, session: &mut TransferSession, trigger: &NrzszTrigger) -> Result<()> {
        // Client is the receiver (server nsz is sending)
        let remote_is_windows = crate::protocol::is_windows_unique_id(&trigger.unique_id);
        session.send_action(true, &trigger.version, remote_is_windows, true)?;
        let dest = if self.default_download_path.is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        } else {
            PathBuf::from(&self.default_download_path)
        };
        
        let local_list = session.recv_files(&dest, self.config.quiet)?;
        let msg = TransferSession::format_saved_files(local_list.len(), &dest, &local_list);
        session.client_exit(&msg)?;
        Ok(())
    }
    
    fn upload_files_to(&self, session: &mut TransferSession, trigger: &NrzszTrigger) -> Result<()> {
        // Client is the sender (server nrz is receiving)
        let remote_is_windows = crate::protocol::is_windows_unique_id(&trigger.unique_id);
        session.send_action(true, &trigger.version, remote_is_windows, true)?;
        let config = session.recv_config()?;
        
        // Mode semantics (matching Go's handleTrzsz):
        //   'S' = download, 'R' = upload (choose files), 'D' = upload directory, 'F' = upload specified files
        let directory = trigger.mode == 'D';
        let files_specified = trigger.mode == 'F';
        // Go's chooseUploadPaths returns oneTimeUploadFiles first, then -u files,
        // then (in F mode) the server's client_files. One-time files are consumed
        // and cleared here (Go resets filter.oneTimeUploadFiles after reading).
        let one_time = {
            let mut guard = self.one_time_files.lock().unwrap();
            std::mem::take(&mut *guard)
        };
        let files = if files_specified {
            if !one_time.is_empty() {
                one_time
            } else if !self.drag_files.lock().unwrap().is_empty() {
                // Drag files take priority (matching Go's chooseUploadPaths which checks dragFiles)
                self.drag_files.lock().unwrap().clone()
            } else if !self.upload_files.is_empty() {
                // 'F' mode: the client (wrapper) provides the actual files via -u.
                self.check_auto_upload_allowed(&self.upload_files)?;
                self.upload_files.clone()
            } else if !config.client_files.is_empty() {
                self.check_auto_upload_allowed(&config.client_files)?;
                config.client_files.clone()
            } else {
                return Err(anyhow!("No files specified to upload"));
            }
        } else if !one_time.is_empty() {
            one_time
        } else if !self.drag_files.lock().unwrap().is_empty() {
            // Drag files in R/D mode too
            self.drag_files.lock().unwrap().clone()
        } else if !self.upload_files.is_empty() {
            // R/D mode: client explicitly chose files via -u; no allowed-dir gate
            // (matches Go, where -u in R/D mode bypasses checkAutoUploadAllowed).
            self.upload_files.clone()
        } else if !config.client_files.is_empty() {
            self.check_auto_upload_allowed(&config.client_files)?;
            config.client_files.clone()
        } else {
            return Err(anyhow!("No upload files specified"));
        };
        
        // For 'F' mode with client files, auto-detect directory if any path is a dir
        let directory = if files_specified {
            files.iter().any(|f| std::path::Path::new(f).is_dir())
        } else {
            directory
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
        // Go errors when no allowed dirs are configured (filter.go checkAutoUploadAllowed).
        if self.allowed_auto_upload_dirs.is_empty() {
            return Err(anyhow!("Auto upload is disabled: no allowed upload directories configured"));
        }
        for file in files {
            let path = std::path::Path::new(file);
            // Go requires absolute paths for auto upload.
            if !path.is_absolute() {
                return Err(anyhow!("File path must be absolute: {}", file));
            }
            let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            let canonical_str = canonical.to_string_lossy().to_string();
            let allowed = self.allowed_auto_upload_dirs.iter().any(|dir| {
                is_path_within_dir(&canonical_str, dir)
            });
            if !allowed {
                return Err(anyhow!("File is outside the allowed auto upload directories: {}", file));
            }
        }
        Ok(())
    }
}

/// Check whether `abs_path` is inside `abs_dir` (either the dir itself or a descendant).
/// Matching Go's isPathWithinDir: rejects `/a/bc2` when allowed dir is `/a/bc`.
fn is_path_within_dir(abs_path: &str, abs_dir: &str) -> bool {
    let dir = abs_dir.trim_end_matches('/');
    if dir.is_empty() {
        return false;
    }
    if abs_path == dir {
        return true;
    }
    if let Some(rest) = abs_path.strip_prefix(dir) {
        return rest.starts_with('/');
    }
    false
}







pub fn detect_nrzsz_trigger(output: &str) -> bool {
    output.contains("TRZSZ:TRANSFER") || output.contains("NRZSZ:TRANSFER")
}

/// Detect if user input contains drag-and-dropped file paths.
/// Returns DragFilesInfo with files list if drag detected, empty files if not.
pub fn detect_drag_files(buf: &[u8]) -> DragFilesInfo {
    if buf.len() < 2 {
        return DragFilesInfo::default();
    }

    let mut info = DragFilesInfo::default();

    // Decode tmux `-CC` control-mode input (matching Go's detectDragFiles):
    // a sequence of `send ...` commands with `;`/`\r` separators. The pane id
    // and block count are needed to emit the iTerm2 `%begin`/`%end` acks.
    let owned_input;
    let mut input: &[u8] = buf;
    if input[0] == b's' && input.len() > 15 && input.contains(&b';') {
        if let Some((decoded, pane_id)) = decode_tmux_drag_input(input) {
            if !pane_id.is_empty() {
                info.tmux_pane_id = pane_id;
                info.tmux_blocks = count_tmux_blocks(input);
            }
            owned_input = decoded;
            input = &owned_input;
            if input.len() < 2 {
                return info;
            }
        }
    }

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
/// Decode a tmux `-CC` control-mode drag input buffer into (payload, pane_id).
/// Matches Go's `decodeTmuxInput` for the drag path: extracts the `send -t %<pane>`
/// or `send -lt %<pane>` payloads and the pane id.
fn decode_tmux_drag_input(buf: &[u8]) -> Option<(Vec<u8>, String)> {
    let mut payload = Vec::new();
    let mut pane_id = String::new();
    let parts: Vec<&[u8]> = buf.split(|&b| b == b';' || b == b'\r').collect();
    for part in &parts {
        let cmd = String::from_utf8_lossy(part).trim().to_string();
        if cmd.len() < 13 || !cmd.starts_with("send ") {
            continue;
        }
        let rest = cmd.strip_prefix("send ").unwrap_or("");
        if let Some(after) = rest.strip_prefix("-lt %") {
            if let Some(pos) = after.find(' ') {
                if pane_id.is_empty() {
                    pane_id = format!("%{}", &after[..pos]);
                }
                payload.extend_from_slice(&after[pos + 1..].as_bytes());
            }
        } else if let Some(after) = rest.strip_prefix("-t %") {
            if let Some(pos) = after.find(' ') {
                if pane_id.is_empty() {
                    pane_id = format!("%{}", &after[..pos]);
                }
                // Hex-encoded payload (0xNN ...) — decode literal bytes.
                let hex_part = &after[pos + 1..];
                let mut bytes = Vec::new();
                for tok in hex_part.split_whitespace() {
                    if let Some(hex) = tok.strip_prefix("0x") {
                        if let Ok(v) = u8::from_str_radix(hex, 16) {
                            bytes.push(v);
                        }
                    }
                }
                payload.extend_from_slice(&bytes);
            }
        }
    }
    if pane_id.is_empty() && payload.is_empty() {
        None
    } else {
        Some((payload, pane_id))
    }
}

/// Count the number of `;`/`\r` separators in a tmux control-mode input buffer
/// (matching Go's `tmuxBlocks` counting in detectDragFiles).
fn count_tmux_blocks(buf: &[u8]) -> usize {
    buf.iter().filter(|&&b| b == b';' || b == b'\r').count()
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
    _cmd_not_trz: bool,
    is_transferring: &Arc<AtomicBool>,
    interrupting: &Arc<AtomicBool>,
    skip_upload_command: &Arc<AtomicBool>,
    current_upload_command: &Arc<std::sync::Mutex<String>>,
    server_in: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    tmux_pane_id: &str,
    tmux_blocks: usize,
) {
    if is_transferring.load(Ordering::SeqCst) {
        return;
    }

    let _ = files;
    interrupting.store(true, Ordering::SeqCst);

    // In tmux `-CC` control mode, interrupt the current pane and emit the
    // iTerm2 `%begin`/`%end` progress acks (matching Go's uploadDragFiles):
    // `%begin <now> 1 1\r\n%end <now> 1 1\r\n` written (tmuxBlocks - 1) times.
    if !tmux_pane_id.is_empty() {
        if tmux_blocks > 1 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let ack = format!("%begin {} 1 1\r\n%end {} 1 1\r\n", now, now);
            for _ in 1..tmux_blocks {
                let _ = std::io::Write::write_all(&mut std::io::stderr(), ack.as_bytes());
            }
        }
        let mut w = server_in.lock().unwrap();
        let cmd = format!("send -t {} 0x3\r", tmux_pane_id);
        let _ = w.write_all(cmd.as_bytes());
        let _ = w.flush();
        drop(w);
        std::thread::sleep(std::time::Duration::from_millis(300));
    } else {
        // Non-tmux: interrupt with Ctrl-C (0x03), matching Go's uploadDragFiles.
        let mut w = server_in.lock().unwrap();
        let _ = w.write_all(&[0x03]);
        let _ = w.flush();
        drop(w);
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    interrupting.store(false, Ordering::SeqCst);

    std::thread::sleep(std::time::Duration::from_millis(200));
    skip_upload_command.store(true, Ordering::SeqCst);
    let mut command = if upload_command.is_empty() {
        "trz".to_string()
    } else {
        upload_command.to_string()
    };
    // Check if command is trz-like by shlex-splitting and checking basename (matching Go's filter.go:238-244)
    let first_token = upload_command.split_whitespace().next().unwrap_or(&upload_command);
    let cmd_basename = std::path::Path::new(first_token)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let cmd_not_trz_fixed = !cmd_basename.contains("trz");
    if has_dir && cmd_not_trz_fixed {
        command = format!("{} -d", command);
    }

    *current_upload_command.lock().unwrap() = command.clone();
    debug!("Upload drag files command: {}", command);

    // Send the trz command to server stdin (matching Go's writeAll(filter.serverIn, buffer)).
    let mut buffer = command.clone().into_bytes();
    buffer.push(b'\r');
    // In tmux -CC control mode, hex-encode the command and wrap it in a
    // `send -t %<pane>` so it routes to the correct pane (Go convertToHexStrings).
    if !tmux_pane_id.is_empty() {
        let hex = crate::tmuxcc::convert_to_hex_strings(&buffer);
        let wrapped = format!("send -t %{} {}\r", tmux_pane_id, hex);
        buffer = wrapped.into_bytes();
    }
    let mut w = server_in.lock().unwrap();
    if w.write_all(&buffer).is_ok() {
        let _ = w.flush();
    }
}








#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_path_within_dir() {
        assert!(is_path_within_dir("/a/bc", "/a/bc"));
        assert!(is_path_within_dir("/a/bc/file", "/a/bc"));
        assert!(is_path_within_dir("/a/bc/dir/file.txt", "/a/bc"));
        assert!(!is_path_within_dir("/a/bc2", "/a/bc"));
        assert!(!is_path_within_dir("/a/bc2/file", "/a/bc"));
        assert!(!is_path_within_dir("/a", "/a/bc"));
        assert!(!is_path_within_dir("/a/bc", "/"));
    }

    #[test]
    fn test_parse_trigger_trzsz_namespace() {
        let line = "::TRZSZ:TRANSFER:D:1.2.0:1234567890123:9090";
        let trigger = parse_trigger(line).unwrap();
        assert_eq!(trigger.mode, 'D');
        assert_eq!(trigger.version, "1.2.0");
        assert_eq!(trigger.unique_id, "1234567890123");
        assert_eq!(trigger.port, 9090);
    }

    #[test]
    fn test_parse_trigger_nrzsz_legacy_namespace() {
        let line = "::NRZSZ:TRANSFER:R:0.1.0:9876543210987:8080";
        let trigger = parse_trigger(line).unwrap();
        assert_eq!(trigger.mode, 'R');
        assert_eq!(trigger.port, 8080);
    }

    #[test]
    fn test_parse_trigger_unknown_namespace() {
        assert!(parse_trigger("::OTHER:TRANSFER:S:1.0:123:456").is_none());
    }

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
        let filter = NrzszFilter::new(Config::default());
        filter.skip_upload_command.store(true, Ordering::SeqCst);
        *filter.current_upload_command.lock().unwrap() = "trz -d".to_string();
        assert!(filter.should_skip_output(b"trz -d\r\n"));
        assert!(!filter.skip_upload_command.load(Ordering::SeqCst));
    }

    #[test]
    fn test_should_skip_output_no_match() {
        let filter = NrzszFilter::new(Config::default());
        filter.skip_upload_command.store(true, Ordering::SeqCst);
        *filter.current_upload_command.lock().unwrap() = "trz".to_string();
        assert!(!filter.should_skip_output(b"other output\r\n"));
    }

    #[test]
    fn test_process_input_drag_uploads_command() {
        // Create a real file so `detect_file_path` (fs::metadata, matching Go's
        // os.Stat) recognizes the dragged path.
        let dir = std::env::temp_dir().join(format!("nrzsz_drag_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file_path = dir.join("a.txt");
        std::fs::write(&file_path, b"drag").unwrap();
        let mut filter = NrzszFilter::new(Config::default());
        filter.enable_dragfile = true;
        // Shared buffer writer so we can inspect what drag upload writes.
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let writer_buf = buf.clone();
        let writer: Arc<Mutex<Box<dyn std::io::Write + Send>>> = Arc::new(Mutex::new(Box::new(
            SharedBufferWriter { buf: writer_buf },
        )));
        filter.server_in = writer;

        let paste = format!("\x1b[200~{}\x1b[201~", file_path.display()).into_bytes();
        let result = filter.process_input(&paste);
        // Drag detected → consume input, send "trz\r" to server stdin.
        assert!(result.is_empty());
        std::thread::sleep(std::time::Duration::from_millis(800));
        let guard = buf.lock().unwrap();
        let s = String::from_utf8_lossy(&guard);
        // Non-tmux: Ctrl-C interrupt (0x03) then the command (Go uploadDragFiles).
        assert_eq!(s, "\u{3}trz\r");
        let _ = std::fs::remove_file(&file_path);
    }

    #[test]
    fn test_process_input_drag_uploads_command_with_dir() {
        // Real directory so the drag is detected with `has_dir` (Go os.Stat).
        let dir = std::env::temp_dir().join(format!("nrzsz_drag_dir_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut filter = NrzszFilter::new(Config::default());
        filter.enable_dragfile = true;
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let writer_buf = buf.clone();
        let writer: Arc<Mutex<Box<dyn std::io::Write + Send>>> = Arc::new(Mutex::new(Box::new(
            SharedBufferWriter { buf: writer_buf },
        )));
        filter.server_in = writer;

        let paste = format!("\x1b[200~{}\x1b[201~", dir.display()).into_bytes();
        let result = filter.process_input(&paste);
        assert!(result.is_empty());
        std::thread::sleep(std::time::Duration::from_millis(800));
        let guard = buf.lock().unwrap();
        let s = String::from_utf8_lossy(&guard);
        // Non-tmux: Ctrl-C interrupt (0x03) then the command with -d.
        assert_eq!(s, "\u{3}trz -d\r");
    }

    /// Simple writer that appends to a shared Vec<u8>.
    struct SharedBufferWriter {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl std::io::Write for SharedBufferWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.buf.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
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
    fn test_one_time_upload_slot_plumbing() {
        // P1: one_time_upload parks a pending result slot; the transfer thread
        // resolves it (run_transfer sends the result), or the 10s watchdog in
        // one_time_upload() fires first if no trigger ever arrived.
        let mut filter = NrzszFilter::new(Config::default());
        let rx = filter.one_time_upload(vec!["/tmp/nrzsz_t/f.txt".to_string()]);
        assert_eq!(
            filter.one_time_upload_files.lock().unwrap().len(),
            1,
            "one-time files are shared with the transfer thread"
        );
        assert!(filter.one_time_upload_result.lock().unwrap().is_some());
        // Resolve as run_transfer does after an R/D/F upload completes.
        filter.set_one_time_upload_result(Ok(()));
        assert!(filter.one_time_upload_result.lock().unwrap().is_none());
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn test_process_input_ctrl_c_during_transfer_stops() {
        // P4: a local Ctrl+C during a live transfer stops the session in place
        // instead of being forwarded to the remote (Go handleInputCtrlC).
        let filter = NrzszFilter::new(Config::default());
        filter.is_transferring.store(true, Ordering::SeqCst);
        // Register an active stop handle (run_transfer does this at session start).
        *filter.active_stop.lock().unwrap() =
            Some(crate::transfer::TransferStop::new());

        // Raw mode: the 0x03 byte is dropped, other input is still forwarded,
        // and the running session's stop handle is marked stopped.
        assert_eq!(filter.process_input(b"ab\x03cd"), b"abcd");
        let handle = filter.active_stop.lock().unwrap().as_ref().unwrap().clone();
        assert!(handle.is_stopped());

        // Tmux-CC mode: a `send -t %1 0x3` command is consumed, not forwarded.
        let cc = b"send -t %1 0x3\r";
        assert!(filter.process_input(cc).is_empty());
    }

    #[test]
    fn test_process_input_transferring() {
        let filter = NrzszFilter::new(Config::default());
        filter.is_transferring.store(true, Ordering::SeqCst);
        let result = filter.process_input(b"hello");
        assert_eq!(result, b"hello");
    }

    #[test]
    fn test_tunnel_hello_read_does_not_overread() {
        // Regression: the client's server-hello read must not consume bytes of
        // the following protocol frame. A fixed `read(&mut [u8; 100])` can
        // return `server_hello\n` coalesced with the next frame's bytes, which
        // were then trimmed away, desyncing the stream (intermittent
        // "Invalid format: " / blank-line failures).
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            // Write hello and the next frame in a single write (coalesced).
            let mut buf = Vec::new();
            buf.extend_from_slice(b"::TRZSZ::SERVER::HELLO::abc:1234\n");
            buf.extend_from_slice(b"#CFG:abcdefgh\n");
            conn.write_all(&buf).unwrap();
            // Server then waits for the client's next frame and echoes it.
            let mut read_buf = [0u8; 64];
            let n = conn.read(&mut read_buf).unwrap();
            conn.write_all(&read_buf[..n]).unwrap();
            conn.flush().unwrap();
        });

        let mut client = std::net::TcpStream::connect(addr).unwrap();
        let hello = crate::tunnel::read_line_from_stream(&mut client).unwrap();
        assert_eq!(hello.trim(), "::TRZSZ::SERVER::HELLO::abc:1234");
        // The next frame must still be readable (not consumed by the hello read).
        let mut next = [0u8; 16];
        let n = client.read(&mut next).unwrap();
        assert_eq!(&next[..n], b"#CFG:abcdefgh\n");

        // Client sends the next frame; server echoes it back.
        client.write_all(b"#DATA:1234\n").unwrap();
        let mut echoed = [0u8; 16];
        let n = client.read(&mut echoed).unwrap();
        assert_eq!(&echoed[..n], b"#DATA:1234\n");
        let _ = server.join();
    }
}