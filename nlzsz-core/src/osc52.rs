use std::io::Write;
use anyhow::{Result, anyhow};
use log::debug;

pub struct Osc52Clipboard {
    is_enabled: bool,
    buffer: Vec<u8>,
}

impl Default for Osc52Clipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Osc52Clipboard {
    pub fn new() -> Self {
        Self {
            is_enabled: false,
            buffer: Vec::new(),
        }
    }
    
    pub fn enable(&mut self) {
        self.is_enabled = true;
        debug!("OSC52 clipboard enabled");
    }
    
    pub fn disable(&mut self) {
        self.is_enabled = false;
        debug!("OSC52 clipboard disabled");
    }
    
    pub fn is_enabled(&self) -> bool {
        self.is_enabled
    }
    
    pub fn copy_to_clipboard(&self, data: &[u8]) -> Result<()> {
        if !self.is_enabled {
            return Err(anyhow!("OSC52 clipboard not enabled"));
        }
        
        // OSC52 escape sequence: ESC ] 52 ; <clipboard> ; <base64data> ESC \
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data);
        
        let sequence = format!("\x1b]52;c;{}\x07", encoded);
        
        print!("{}", sequence);
        
        debug!("Copied {} bytes to clipboard", data.len());
        Ok(())
    }
    
    pub fn paste_from_clipboard(&mut self) -> Result<Vec<u8>> {
        if !self.is_enabled {
            return Err(anyhow!("OSC52 clipboard not enabled"));
        }
        
        // Request clipboard contents
        print!("\x1b]52;c;?\x07");
        
        // Note: In practice, the response would come from the terminal
        // This is a simplified implementation
        
        Ok(self.buffer.clone())
    }
    
    pub fn process_clipboard_response(&mut self, data: &[u8]) -> Result<()> {
        // Parse OSC52 response
        if data.starts_with(b"\x1b]52;c;") && data.ends_with(b"\x07") {
            let encoded = &data[7..data.len() - 1];
            let decoded = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                encoded,
            )?;
            
            self.buffer = decoded;
            debug!("Received {} bytes from clipboard", self.buffer.len());
        }
        
        Ok(())
    }
    
    pub fn write_to_system_clipboard(&self, data: &[u8]) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            let mut process = std::process::Command::new("pbcopy")
                .stdin(std::process::Stdio::piped())
                .spawn()?;
            
            if let Some(ref mut stdin) = process.stdin {
                stdin.write_all(data)?;
            }
            
            process.wait()?;
            Ok(())
        }
        
        #[cfg(target_os = "linux")]
        {
            // Try xclip first, then xsel
            if let Ok(mut process) = std::process::Command::new("xclip")
                .args(["-selection", "clipboard"])
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                if let Some(ref mut stdin) = process.stdin {
                    stdin.write_all(data)?;
                }
                process.wait()?;
                return Ok(());
            }
            
            if let Ok(mut process) = std::process::Command::new("xsel")
                .args(["--clipboard", "--input"])
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                if let Some(ref mut stdin) = process.stdin {
                    stdin.write_all(data)?;
                }
                process.wait()?;
                return Ok(());
            }
            
            return Err(anyhow!("No clipboard tool found"));
        }
        
        #[cfg(target_os = "windows")]
        {
            let mut process = std::process::Command::new("powershell")
                .args(["-Command", "Set-Clipboard"])
                .stdin(std::process::Stdio::piped())
                .spawn()?;
            
            if let Some(ref mut stdin) = process.stdin {
                stdin.write_all(data)?;
            }
            
            process.wait()?;
            return Ok(());
        }
        
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Err(anyhow!("Platform not supported"))
        }
    }
    
    pub fn read_from_system_clipboard(&self) -> Result<Vec<u8>> {
        #[cfg(target_os = "macos")]
        {
            let output = std::process::Command::new("pbpaste")
                .output()?;
            
            Ok(output.stdout)
        }
        
        #[cfg(target_os = "linux")]
        {
            if let Ok(output) = std::process::Command::new("xclip")
                .args(["-selection", "clipboard", "-o"])
                .output()
            {
                return Ok(output.stdout);
            }
            
            if let Ok(output) = std::process::Command::new("xsel")
                .args(["--clipboard", "--output"])
                .output()
            {
                return Ok(output.stdout);
            }
            
            return Err(anyhow!("No clipboard tool found"));
        }
        
        #[cfg(target_os = "windows")]
        {
            let output = std::process::Command::new("powershell")
                .args(["-Command", "Get-Clipboard"])
                .output()?;
            
            return Ok(output.stdout);
        }
        
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Err(anyhow!("Platform not supported"))
        }
    }
    
    pub fn copy_file_to_clipboard(&self, file_path: &std::path::Path) -> Result<()> {
        let data = std::fs::read(file_path)?;
        self.write_to_system_clipboard(&data)
    }
    
    pub fn save_clipboard_to_file(&self, file_path: &std::path::Path) -> Result<()> {
        let data = self.read_from_system_clipboard()?;
        std::fs::write(file_path, data)?;
        debug!("Saved clipboard contents to {:?}", file_path);
        Ok(())
    }
    
    pub fn clear_clipboard(&self) -> Result<()> {
        self.write_to_system_clipboard(b"")
    }
    
    pub fn get_buffer(&self) -> &[u8] {
        &self.buffer
    }
    
    pub fn set_buffer(&mut self, data: Vec<u8>) {
        self.buffer = data;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_osc52_creation() {
        let clipboard = Osc52Clipboard::new();
        assert!(!clipboard.is_enabled());
    }
    
    #[test]
    fn test_enable_disable() {
        let mut clipboard = Osc52Clipboard::new();
        clipboard.enable();
        assert!(clipboard.is_enabled());
        
        clipboard.disable();
        assert!(!clipboard.is_enabled());
    }
}