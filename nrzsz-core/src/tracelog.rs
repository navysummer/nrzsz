use std::fs::File;
use std::io::Write;
use std::sync::mpsc;
use std::sync::Mutex;
use std::sync::Arc;
use std::thread;

use crate::utils::encode_buffer;

const ENABLE_MARKER: &str = "<ENABLE_NRZSZ_TRACE_LOG>";
const DISABLE_MARKER: &str = "<DISABLE_NRZSZ_TRACE_LOG>";
// Also detect the Go marker for interop with nrzsz-go servers
const ENABLE_MARKER_GO: &str = "<ENABLE_NRZSZ_TRACE_LOG>";

type OptFile = Arc<Mutex<Option<File>>>;
type OptChan = Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>;

/// TraceLogger writes transfer data to a temp log file for debugging.
/// It is enabled/disabled by special markers in the server output:
///   <ENABLE_NRZSZ_TRACE_LOG>  and  <DISABLE_NRZSZ_TRACE_LOG>
/// (plus the Go variants <ENABLE_NRZSZ_TRACE_LOG>/<DISABLE_NRZSZ_TRACE_LOG>)
#[derive(Clone)]
pub struct TraceLogger {
    chan: OptChan,
    file: OptFile,
}

impl Default for TraceLogger {
    fn default() -> Self {
        Self::new()
    }
}

impl TraceLogger {
    pub fn new() -> Self {
        Self {
            chan: Arc::new(Mutex::new(None)),
            file: Arc::new(Mutex::new(None)),
        }
    }

    /// Process a chunk of server output. Returns the (possibly rewritten) buffer.
    pub fn write_trace_log(&self, buf: &[u8], typ: &str) -> Vec<u8> {
        let chan_guard = self.chan.lock().unwrap().clone();
        let file_active = self.file.lock().unwrap().is_some();

        if chan_guard.is_some() && file_active {
            if typ == "svrout" && contains_marker(buf, DISABLE_MARKER) {
                // Disable: close the channel and drop the file reference
                self.chan.lock().unwrap().take();
                if let Some(file) = self.file.lock().unwrap().as_ref() {
                    let _ = file.sync_all();
                }
                self.file.lock().unwrap().take();
                let msg = "Closed trace log".to_string();
                return replace_marker(buf, DISABLE_MARKER, &msg);
            }
            if let Some(tx) = &chan_guard {
                let encoded = encode_buffer(buf).unwrap_or_default();
                let line = format!("[{}]{}\n", typ, encoded);
                let _ = tx.send(line.into_bytes());
            }
            return buf.to_vec();
        }

        if typ == "svrout" && contains_marker(buf, ENABLE_MARKER) {
            return self.enable(buf, ENABLE_MARKER);
        }
        if typ == "svrout" && contains_marker(buf, ENABLE_MARKER_GO) {
            return self.enable(buf, ENABLE_MARKER_GO);
        }
        buf.to_vec()
    }

    fn enable(&self, buf: &[u8], marker: &str) -> Vec<u8> {
        let msg;
        let file = match tempfile::Builder::new()
            .prefix("nrzsz_")
            .suffix(".log")
            .tempfile()
        {
            Ok(f) => {
                match f.keep() {
                    Ok((file, persisted_path)) => {
                        msg = format!("Writing trace log to {}", persisted_path.display());
                        Some(file)
                    }
                    Err(e) => {
                        msg = format!("Persist log file error: {}", e);
                        None
                    }
                }
            }
            Err(e) => {
                msg = format!("Create log file error: {}", e);
                None
            }
        };

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        if let Some(file) = file {
            *self.file.lock().unwrap() = Some(file.try_clone().unwrap());
            *self.chan.lock().unwrap() = Some(tx);
            thread::spawn(move || {
                let mut f = file;
                while let Ok(line) = rx.recv() {
                    if f.write_all(&line).is_err() {
                        break;
                    }
                }
            });
        } else {
            *self.chan.lock().unwrap() = Some(tx);
            thread::spawn(move || {
                while let Ok(line) = rx.recv() {
                    let _ = std::io::sink().write(&line);
                }
            });
        }

        replace_marker(buf, marker, &msg)
    }

    pub fn close(&self) {
        self.chan.lock().unwrap().take();
        self.file.lock().unwrap().take();
    }
}

fn contains_marker(buf: &[u8], marker: &str) -> bool {
    let needle = marker.as_bytes();
    buf.windows(needle.len()).any(|w| w == needle)
}

fn replace_marker(buf: &[u8], marker: &str, replacement: &str) -> Vec<u8> {
    let needle = marker.as_bytes();
    let mut result = Vec::with_capacity(buf.len() + replacement.len());
    let mut i = 0;
    while i < buf.len() {
        if i + needle.len() <= buf.len() && &buf[i..i + needle.len()] == needle {
            result.extend_from_slice(replacement.as_bytes());
            i += needle.len();
        } else {
            result.push(buf[i]);
            i += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_marker_detection() {
        assert!(contains_marker(b"abc<ENABLE_NRZSZ_TRACE_LOG>def", ENABLE_MARKER));
        assert!(!contains_marker(b"abc", ENABLE_MARKER));
        assert!(contains_marker(b"<ENABLE_NRZSZ_TRACE_LOG>", ENABLE_MARKER_GO));
    }

    #[test]
    fn test_replace_marker() {
        let out = replace_marker(b"a<ENABLE_NRZSZ_TRACE_LOG>b", ENABLE_MARKER, "X");
        assert_eq!(out, b"aXb");
    }
}