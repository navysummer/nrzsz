use anyhow::{Result, anyhow};
use log::debug;

pub struct ITerm2TmuxCC {
    is_active: bool,
    pane_id: Option<String>,
}

impl Default for ITerm2TmuxCC {
    fn default() -> Self {
        Self::new()
    }
}

impl ITerm2TmuxCC {
    pub fn new() -> Self {
        Self {
            is_active: false,
            pane_id: None,
        }
    }
    
    pub fn activate(&mut self) -> Result<()> {
        self.is_active = true;
        
        // Detect pane ID
        self.detect_pane_id()?;
        
        debug!("iTerm2 tmux-CC activated for pane: {:?}", self.pane_id);
        Ok(())
    }
    
    pub fn deactivate(&mut self) -> Result<()> {
        self.is_active = false;
        self.pane_id = None;
        
        debug!("iTerm2 tmux-CC deactivated");
        Ok(())
    }
    
    fn detect_pane_id(&mut self) -> Result<()> {
        // Try to get pane ID from tmux
        let output = std::process::Command::new("tmux")
            .args(["display-message", "-p", "#{pane_id}"])
            .output()?;
        
        if output.status.success() {
            let stdout = String::from_utf8(output.stdout)?;
            self.pane_id = Some(stdout.trim().to_string());
        }
        
        Ok(())
    }
    
    pub fn send_command(&self, command: &str) -> Result<()> {
        if !self.is_active {
            return Err(anyhow!("iTerm2 tmux-CC not active"));
        }
        
        // Send command to tmux
        let output = std::process::Command::new("tmux")
            .args(["send-keys", command, "Enter"])
            .output()?;
        
        if !output.status.success() {
            return Err(anyhow!("Failed to send command to tmux"));
        }
        
        debug!("Sent command to tmux: {}", command);
        Ok(())
    }
    
    pub fn capture_pane_content(&self) -> Result<String> {
        if !self.is_active {
            return Err(anyhow!("iTerm2 tmux-CC not active"));
        }
        
        let output = std::process::Command::new("tmux")
            .args(["capture-pane", "-p"])
            .output()?;
        
        if !output.status.success() {
            return Err(anyhow!("Failed to capture pane content"));
        }
        
        let stdout = String::from_utf8(output.stdout)?;
        Ok(stdout)
    }
    
    pub fn set_pane_title(&self, title: &str) -> Result<()> {
        if !self.is_active {
            return Err(anyhow!("iTerm2 tmux-CC not active"));
        }
        
        let output = std::process::Command::new("tmux")
            .args(["rename-window", title])
            .output()?;
        
        if !output.status.success() {
            return Err(anyhow!("Failed to set pane title"));
        }
        
        debug!("Set pane title: {}", title);
        Ok(())
    }
    
    pub fn send_control_sequence(&self, sequence: &str) -> Result<()> {
        if !self.is_active {
            return Err(anyhow!("iTerm2 tmux-CC not active"));
        }
        
        // Send control sequence to tmux
        print!("\x1bP={}\x1b\\", sequence);
        
        debug!("Sent control sequence: {}", sequence);
        Ok(())
    }
    
    pub fn is_active(&self) -> bool {
        self.is_active
    }
    
    pub fn get_pane_id(&self) -> Option<&str> {
        self.pane_id.as_deref()
    }
    
    pub fn update_progress(&self, progress: f64, filename: &str) -> Result<()> {
        if !self.is_active {
            return Err(anyhow!("iTerm2 tmux-CC not active"));
        }
        
        // Update tmux status bar with progress
        let status_line = format!("{}: {:.1}%", filename, progress * 100.0);
        
        let output = std::process::Command::new("tmux")
            .args(["set-option", "status-left", &status_line])
            .output()?;
        
        if !output.status.success() {
            return Err(anyhow!("Failed to update progress"));
        }
        
        debug!("Updated progress: {}", status_line);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_iterm2_tmux_cc_creation() {
        let cc = ITerm2TmuxCC::new();
        assert!(!cc.is_active());
        assert!(cc.get_pane_id().is_none());
    }
}
