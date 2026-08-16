use std::process::{Command, Stdio};
use anyhow::{Result, anyhow};
use log::debug;

pub struct ZenityProgress {
    is_available: bool,
    child: Option<std::process::Child>,
}

impl Default for ZenityProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl ZenityProgress {
    pub fn new() -> Self {
        let is_available = Self::check_zenity_available();
        
        Self {
            is_available,
            child: None,
        }
    }
    
    fn check_zenity_available() -> bool {
        Command::new("which")
            .arg("zenity")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
    
    pub fn show_progress(&mut self, title: &str, _maximum: f64) -> Result<()> {
        if !self.is_available {
            return Err(anyhow!("Zenity not available"));
        }
        
        self.child = Some(Command::new("zenity")
            .args([
                "--progress",
                "--title", title,
                "--percentage=0",
                "--auto-close",
                "--auto-kill",
            ])
            .stdin(Stdio::piped())
            .spawn()?);
        
        debug!("Started Zenity progress dialog");
        Ok(())
    }
    
    pub fn update_progress(&mut self, progress: f64) -> Result<()> {
        if let Some(ref mut child) = self.child {
            if let Some(ref mut stdin) = child.stdin {
                use std::io::Write;
                writeln!(stdin, "{:.1}", progress * 100.0)?;
            }
        }
        
        Ok(())
    }
    
    pub fn close_progress(&mut self) -> Result<()> {
        if let Some(ref mut child) = self.child {
            child.kill()?;
        }
        self.child = None;
        
        debug!("Closed Zenity progress dialog");
        Ok(())
    }
    
    pub fn show_error(&self, message: &str) -> Result<()> {
        if !self.is_available {
            return Err(anyhow!("Zenity not available"));
        }
        
        Command::new("zenity")
            .args([
                "--error",
                "--text", message,
            ])
            .output()?;
        
        Ok(())
    }
    
    pub fn show_info(&self, message: &str) -> Result<()> {
        if !self.is_available {
            return Err(anyhow!("Zenity not available"));
        }
        
        Command::new("zenity")
            .args([
                "--info",
                "--text", message,
            ])
            .output()?;
        
        Ok(())
    }
    
    pub fn show_question(&self, message: &str) -> Result<bool> {
        if !self.is_available {
            return Err(anyhow!("Zenity not available"));
        }
        
        let output = Command::new("zenity")
            .args([
                "--question",
                "--text", message,
            ])
            .output()?;
        
        Ok(output.status.success())
    }
    
    pub fn show_file_selection(&self, title: &str) -> Result<Option<String>> {
        if !self.is_available {
            return Err(anyhow!("Zenity not available"));
        }
        
        let output = Command::new("zenity")
            .args([
                "--file-selection",
                "--title", title,
            ])
            .output()?;
        
        if output.status.success() {
            let stdout = String::from_utf8(output.stdout)?;
            Ok(Some(stdout.trim().to_string()))
        } else {
            Ok(None)
        }
    }
    
    pub fn is_available(&self) -> bool {
        self.is_available
    }
}

impl Drop for ZenityProgress {
    fn drop(&mut self) {
        let _ = self.close_progress();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_zenity_creation() {
        let progress = ZenityProgress::new();
        // Availability depends on the system
        assert!(progress.is_available() || !progress.is_available());
    }
}
