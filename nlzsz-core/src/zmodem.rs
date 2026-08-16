use std::path::Path;
use anyhow::{Result, anyhow};
use log::debug;

use crate::pty::PtyProcess;

pub struct ZmodemProxy {
    is_active: bool,
    pty: Option<PtyProcess>,
}

impl Default for ZmodemProxy {
    fn default() -> Self {
        Self::new()
    }
}

impl ZmodemProxy {
    pub fn new() -> Self {
        Self {
            is_active: false,
            pty: None,
        }
    }
    
    pub fn start_proxy(&mut self) -> Result<()> {
        self.is_active = true;
        debug!("Zmodem proxy started");
        Ok(())
    }
    
    pub fn stop_proxy(&mut self) -> Result<()> {
        if let Some(ref mut pty) = self.pty {
            pty.kill()?;
        }
        self.pty = None;
        self.is_active = false;
        debug!("Zmodem proxy stopped");
        Ok(())
    }
    
    pub fn is_active(&self) -> bool {
        self.is_active
    }
    
    pub fn proxy_zmodem_send(&mut self, file: &Path) -> Result<()> {
        if !self.is_active {
            return Err(anyhow!("Zmodem proxy not active"));
        }
        
        debug!("Proxying Zmodem send for: {:?}", file);
        
        // Start sz command in PTY
        let mut pty = PtyProcess::new();
        pty.spawn("sz", &[file.to_string_lossy().to_string()])?;
        
        // Read output and forward it
        let mut buffer = [0u8; 4096];
        loop {
            match pty.read_output(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    // Forward output
                    print!("{}", String::from_utf8_lossy(&buffer[..n]));
                }
                Err(_) => break,
            }
        }
        
        pty.wait()?;
        Ok(())
    }
    
    pub fn proxy_zmodem_receive(&mut self, output_dir: &Path) -> Result<()> {
        if !self.is_active {
            return Err(anyhow!("Zmodem proxy not active"));
        }
        
        debug!("Proxying Zmodem receive to: {:?}", output_dir);
        
        // Start rz command in PTY
        let mut pty = PtyProcess::new();
        pty.spawn("rz", &[])?;
        
        // Change to output directory
        let cd_cmd = format!("cd {}\n", output_dir.to_string_lossy());
        pty.write_input(cd_cmd.as_bytes())?;
        
        // Read output and forward it
        let mut buffer = [0u8; 4096];
        loop {
            match pty.read_output(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    // Forward output
                    print!("{}", String::from_utf8_lossy(&buffer[..n]));
                }
                Err(_) => break,
            }
        }
        
        pty.wait()?;
        Ok(())
    }
    
    pub fn detect_zmodem_sequence(&self, data: &[u8]) -> bool {
        // Zmodem start sequence: **\x18**\x00
        data.starts_with(b"**\x18**\x00")
    }
    
    pub fn send_zmodem_start(&self) -> Vec<u8> {
        b"**\x18**\x00".to_vec()
    }
    
    pub fn send_zmodem_cancel(&self) -> Vec<u8> {
        b"\x18\x18\x18\x18\x18\x18\x18\x18".to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_zmodem_proxy_creation() {
        let proxy = ZmodemProxy::new();
        assert!(!proxy.is_active());
    }
    
    #[test]
    fn test_zmodem_detection() {
        let proxy = ZmodemProxy::new();
        let data = b"**\x18**\00some data";
        assert!(proxy.detect_zmodem_sequence(data));
        
        let data2 = b"not zmodem";
        assert!(!proxy.detect_zmodem_sequence(data2));
    }
}