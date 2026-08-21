use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Condvar};
use std::time::{Duration, Instant};
use anyhow::{Result, anyhow};

use crate::utils::strip_vt100;

pub struct NrzszBuffer {
    buffer: Arc<Mutex<Vec<u8>>>,
    condvar: Arc<Condvar>,
    stopped: Arc<AtomicBool>,
    timeout: Arc<Mutex<Duration>>,
    strip_vt100: Arc<AtomicBool>,
}

impl Default for NrzszBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl NrzszBuffer {
    pub fn new() -> Self {
        Self {
            buffer: Arc::new(Mutex::new(Vec::new())),
            condvar: Arc::new(Condvar::new()),
            stopped: Arc::new(AtomicBool::new(false)),
            timeout: Arc::new(Mutex::new(Duration::from_secs(20))),
            strip_vt100: Arc::new(AtomicBool::new(false)),
        }
    }
    
    pub fn set_strip_vt100(&self, enable: bool) {
        self.strip_vt100.store(enable, Ordering::SeqCst);
    }
    
    pub fn get_strip_vt100(&self) -> bool {
        self.strip_vt100.load(Ordering::SeqCst)
    }

    pub fn add_buffer(&self, buf: &[u8]) {
        let mut data = self.buffer.lock().unwrap();
        data.extend_from_slice(buf);
        self.condvar.notify_all();
    }

    pub fn stop_buffer(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.condvar.notify_all();
    }

    pub fn drain_buffer(&self) {
        let mut data = self.buffer.lock().unwrap();
        data.clear();
        self.condvar.notify_all();
    }

    pub fn set_new_timeout(&self, timeout: Duration) {
        *self.timeout.lock().unwrap() = timeout;
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    pub fn read_line(&self, may_has_junk: bool, timeout: Option<Duration>) -> Result<Vec<u8>> {
        let timeout_dur = timeout.unwrap_or(*self.timeout.lock().unwrap());
        let start = Instant::now();

        loop {
            if self.stopped.load(Ordering::SeqCst) {
                return Err(anyhow!("Buffer stopped"));
            }

            let mut guard = self.buffer.lock().unwrap();

            if let Some(nl_pos) = guard.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = guard.drain(..nl_pos).collect();
                guard.drain(..1);
                drop(guard);

                if may_has_junk {
                    line = crate::utils::strip_tmux_status_line(&line);
                    if let Some(hash_pos) = line.iter().rposition(|&b| b == b'#') {
                        line = line[hash_pos..].to_vec();
                    }
                    line = strip_vt100(&line);
                }

                return Ok(line);
            }

            let elapsed = start.elapsed();
            if elapsed >= timeout_dur {
                return Err(anyhow!("Read timeout"));
            }
            let remaining = timeout_dur - elapsed;
            let result = self.condvar.wait_timeout(guard, remaining);
            let (g, timeout_result) = result.map_err(|_| anyhow!("Buffer lock poisoned"))?;
            guard = g;
            if timeout_result.timed_out() {
                return Err(anyhow!("Read timeout"));
            }
        }
    }

    pub fn read_line_on_windows(&self, timeout: Option<Duration>) -> Result<Vec<u8>> {
        let timeout_dur = timeout.unwrap_or(*self.timeout.lock().unwrap());
        let start = Instant::now();

        loop {
            if self.stopped.load(Ordering::SeqCst) {
                return Err(anyhow!("Buffer stopped"));
            }

            let mut guard = self.buffer.lock().unwrap();

            if let Some(pos) = guard.windows(2).position(|w| w == *b"!\n") {
                let mut line: Vec<u8> = guard.drain(..pos).collect();
                guard.drain(..2);

                line = strip_vt100(&line);

                return Ok(line);
            }

            let elapsed = start.elapsed();
            if elapsed >= timeout_dur {
                return Err(anyhow!("Read timeout"));
            }
            let remaining = timeout_dur - elapsed;
            let result = self.condvar.wait_timeout(guard, remaining);
            let (g, timeout_result) = result.map_err(|_| anyhow!("Buffer lock poisoned"))?;
            guard = g;
            if timeout_result.timed_out() {
                return Err(anyhow!("Read timeout"));
            }
        }
    }

    pub fn read_binary(&self, size: usize, timeout: Option<Duration>) -> Result<Vec<u8>> {
        let timeout_dur = timeout.unwrap_or(*self.timeout.lock().unwrap());
        let start = Instant::now();

        loop {
            if self.stopped.load(Ordering::SeqCst) {
                return Err(anyhow!("Buffer stopped"));
            }

            let mut guard = self.buffer.lock().unwrap();

            if guard.len() >= size {
                let result: Vec<u8> = guard.drain(..size).collect();
                return Ok(result);
            }

            let elapsed = start.elapsed();
            if elapsed >= timeout_dur {
                return Err(anyhow!("Read timeout"));
            }
            let remaining = timeout_dur - elapsed;
            let result = self.condvar.wait_timeout(guard, remaining);
            let (g, timeout_result) = result.map_err(|_| anyhow!("Buffer lock poisoned"))?;
            guard = g;
            if timeout_result.timed_out() {
                return Err(anyhow!("Read timeout"));
            }
        }
    }

    /// Wait for at least `min` bytes to be available, then return up to `max` bytes.
    /// This mimics a raw byte stream read (does NOT wait for a newline).
    /// Buffered data is drained even after the buffer is stopped.
    pub fn read_stream(&self, buf: &mut [u8], timeout: Option<Duration>) -> Result<usize> {
        let timeout_dur = timeout.unwrap_or(*self.timeout.lock().unwrap());
        let start = Instant::now();

        loop {
            let mut guard = self.buffer.lock().unwrap();

            // Always drain buffered data first, even if stopped
            if !guard.is_empty() {
                let n = std::cmp::min(buf.len(), guard.len());
                buf[..n].copy_from_slice(&guard.drain(..n).collect::<Vec<u8>>());
                return Ok(n);
            }

            if self.stopped.load(Ordering::SeqCst) {
                return Err(anyhow!("Buffer stopped"));
            }

            let elapsed = start.elapsed();
            if elapsed >= timeout_dur {
                return Err(anyhow!("Read timeout"));
            }
            let remaining = timeout_dur - elapsed;
            let result = self.condvar.wait_timeout(guard, remaining);
            let (g, timeout_result) = result.map_err(|_| anyhow!("Buffer lock poisoned"))?;
            guard = g;
            if timeout_result.timed_out() {
                return Err(anyhow!("Read timeout"));
            }
        }
    }
}

pub fn start_stdin_reader(stdin: Box<dyn Read + Send>, buffer: Arc<NrzszBuffer>) {
    std::thread::spawn(move || {
        let mut reader = stdin;
        let mut buf = [0u8; 32768];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    buffer.add_buffer(&buf[..n]);
                }
                Err(_) => break,
            }
        }
        buffer.stop_buffer();
    });
}

pub struct BufferReader {
    buffer: Arc<NrzszBuffer>,
    timeout: Duration,
}

impl BufferReader {
    pub fn new(buffer: Arc<NrzszBuffer>) -> Self {
        Self {
            buffer,
            timeout: Duration::from_secs(20),
        }
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
        self.buffer.set_new_timeout(timeout);
    }
}

impl Read for BufferReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = match self.buffer.read_stream(buf, Some(self.timeout)) {
            Ok(n) => n,
            Err(e) => {
                if self.buffer.is_stopped() {
                    return Ok(0);
                }
                return Err(std::io::Error::other(e.to_string()));
            }
        };
        // Strip VT100 escape sequences if configured (Windows protocol mode)
        if self.buffer.get_strip_vt100() && n > 0 {
            let stripped = strip_vt100(&buf[..n]);
            let len = stripped.len().min(buf.len());
            buf[..len].copy_from_slice(&stripped[..len]);
            n = len;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_read_line() {
        let buffer = NrzszBuffer::new();
        buffer.add_buffer(b"hello\nworld\n");
        
        let line = buffer.read_line(false, Some(Duration::from_secs(1))).unwrap();
        assert_eq!(line, b"hello");
        
        let line = buffer.read_line(false, Some(Duration::from_secs(1))).unwrap();
        assert_eq!(line, b"world");
    }

    #[test]
    fn test_buffer_read_binary() {
        let buffer = NrzszBuffer::new();
        buffer.add_buffer(b"abcdefghij");
        
        let data = buffer.read_binary(5, Some(Duration::from_secs(1))).unwrap();
        assert_eq!(data, b"abcde");
        
        let data = buffer.read_binary(5, Some(Duration::from_secs(1))).unwrap();
        assert_eq!(data, b"fghij");
    }

    #[test]
    fn test_buffer_timeout() {
        let buffer = NrzszBuffer::new();
        let result = buffer.read_line(false, Some(Duration::from_millis(10)));
        assert!(result.is_err());
    }

    #[test]
    fn test_buffer_stop() {
        let buffer = NrzszBuffer::new();
        buffer.stop_buffer();
        let result = buffer.read_line(false, Some(Duration::from_secs(1)));
        assert!(result.is_err());
    }
}