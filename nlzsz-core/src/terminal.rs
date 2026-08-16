use std::process::{Command, Stdio, Child};
use std::io::{self, Read};
use anyhow::{Result, anyhow};
use log::debug;

pub struct TerminalWrapper {
    child: Option<Child>,
    is_server: bool,
}

impl Default for TerminalWrapper {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalWrapper {
    pub fn new() -> Self {
        Self {
            child: None,
            is_server: false,
        }
    }
    
    pub fn is_warp_terminal() -> bool {
        // Check for Warp terminal environment variables
        std::env::var("WARP_TERMINAL_VERSION").is_ok() ||
        std::env::var("TERM_PROGRAM").is_ok_and(|v| v.contains("Warp"))
    }
    
    pub fn get_terminal_info() -> TerminalInfo {
        let is_warp = Self::is_warp_terminal();
        let is_tmux = std::env::var("TMUX").is_ok();
        let is_screen = std::env::var("STY").is_ok();
        
        TerminalInfo {
            is_warp,
            is_tmux,
            is_screen,
            is_dumb: std::env::var("TERM").map_or(true, |v| v == "dumb"),
        }
    }
    
    pub fn wrap_ssh_command(&mut self, args: &[String]) -> Result<()> {
        let mut cmd = Command::new("ssh");
        cmd.args(args);
        
        // Set environment variables to enable nlzsz on the server
        cmd.env("NLZSZ", "1");
        cmd.env("NLZSZ_CWD", std::env::current_dir()?);
        
        self.child = Some(cmd
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?);
        
        Ok(())
    }
    
    pub fn run_server_side(&mut self) -> Result<()> {
        self.is_server = true;
        
        // Check if nlzsz is enabled
        if std::env::var("NLZSZ").is_err() {
            return Err(anyhow!("nlzsz not enabled"));
        }
        
        // Detect if we're in a transfer session
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        
        let mut buffer = [0u8; 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    // Process input
                    self.process_input(&buffer[..n])?;
                }
                Err(e) => {
                    if e.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(e.into());
                }
            }
        }
        
        Ok(())
    }
    
    fn process_input(&self, data: &[u8]) -> Result<()> {
        // Check for nlzsz escape sequence
        if data.starts_with(b"\x1b[0;9999d") {
            debug!("Received nlzsz start sequence");
            // Start transfer mode
        }
        
        Ok(())
    }
    
    pub fn is_server_mode(&self) -> bool {
        self.is_server
    }
    
    pub fn wait(&mut self) -> Result<i32> {
        if let Some(ref mut child) = self.child {
            let status = child.wait()?;
            Ok(status.code().unwrap_or(1))
        } else {
            Ok(0)
        }
    }
    
    pub fn kill(&mut self) -> Result<()> {
        if let Some(ref mut child) = self.child {
            child.kill()?;
        }
        Ok(())
    }
}

pub fn setup_server_env() {
    // Set environment variables for server-side detection
    std::env::set_var("NLZSZ", "1");
    std::env::set_var("NLZSZ_CWD", std::env::current_dir().unwrap().to_string_lossy().to_string());
}

pub fn detect_client_support() -> bool {
    // Check if the client supports nlzsz
    std::env::var("NLZSZ").is_ok()
}

pub fn get_client_cwd() -> Option<String> {
    std::env::var("NLZSZ_CWD").ok()
}

#[derive(Debug, Clone)]
pub struct TerminalInfo {
    pub is_warp: bool,
    pub is_tmux: bool,
    pub is_screen: bool,
    pub is_dumb: bool,
}

impl TerminalInfo {
    pub fn should_use_escape_codes(&self) -> bool {
        // Warp terminal has issues with certain escape sequences
        if self.is_warp {
            return false;
        }
        // Dumb terminals don't support escape codes
        if self.is_dumb {
            return false;
        }
        true
    }
    
    pub fn should_refresh_client(&self) -> bool {
        // tmux and screen need special handling
        self.is_tmux || self.is_screen
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_terminal_wrapper_creation() {
        let wrapper = TerminalWrapper::new();
        assert!(!wrapper.is_server_mode());
    }
    
    #[test]
    fn test_detect_client_support() {
        // Test with no env vars
        std::env::remove_var("NLZSZ");
        assert!(!detect_client_support());
        
        // Test with NLZSZ env var
        std::env::set_var("NLZSZ", "1");
        assert!(detect_client_support());
        
        // Clean up
        std::env::remove_var("NLZSZ");
    }
}
