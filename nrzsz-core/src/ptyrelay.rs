use anyhow::{Result, anyhow};
use log::debug;

use crate::pty::PtyProcess;

pub struct PtyRelay {
    pty: PtyProcess,
    command: String,
    args: Vec<String>,
    is_active: bool,
}

impl PtyRelay {
    pub fn new(command: &str, args: Vec<String>) -> Self {
        Self {
            pty: PtyProcess::new(),
            command: command.to_string(),
            args,
            is_active: false,
        }
    }
    
    pub fn start(&mut self) -> Result<()> {
        self.pty.spawn(&self.command, &self.args)?;
        self.is_active = true;
        debug!("PTY relay started: {} {:?}", self.command, self.args);
        Ok(())
    }
    
    pub fn stop(&mut self) -> Result<()> {
        self.pty.kill()?;
        self.is_active = false;
        debug!("PTY relay stopped");
        Ok(())
    }
    
    pub fn is_active(&self) -> bool {
        self.is_active
    }
    
    pub fn read_output(&mut self, buffer: &mut [u8]) -> Result<usize> {
        self.pty.read_output(buffer)
    }
    
    pub fn write_input(&mut self, data: &[u8]) -> Result<()> {
        self.pty.write_input(data)
    }
    
    pub fn resize(&self, width: u32, height: u32) -> Result<()> {
        self.pty.resize(width, height)
    }
    
    pub fn wait(&mut self) -> Result<i32> {
        self.pty.wait()
    }
}

pub struct PtyRelayManager {
    relays: Vec<PtyRelay>,
}

impl Default for PtyRelayManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PtyRelayManager {
    pub fn new() -> Self {
        Self {
            relays: Vec::new(),
        }
    }
    
    pub fn add_relay(&mut self, command: &str, args: Vec<String>) -> Result<()> {
        let mut relay = PtyRelay::new(command, args);
        relay.start()?;
        self.relays.push(relay);
        Ok(())
    }
    
    pub fn remove_relay(&mut self, index: usize) -> Result<()> {
        if index < self.relays.len() {
            self.relays[index].stop()?;
            self.relays.remove(index);
            Ok(())
        } else {
            Err(anyhow!("Invalid relay index"))
        }
    }
    
    pub fn get_relay(&self, index: usize) -> Option<&PtyRelay> {
        self.relays.get(index)
    }
    
    pub fn get_relay_mut(&mut self, index: usize) -> Option<&mut PtyRelay> {
        self.relays.get_mut(index)
    }
    
    pub fn stop_all(&mut self) -> Result<()> {
        for relay in &mut self.relays {
            relay.stop()?;
        }
        self.relays.clear();
        Ok(())
    }
    
    pub fn get_count(&self) -> usize {
        self.relays.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_pty_relay_creation() {
        let relay = PtyRelay::new("echo", vec!["hello".to_string()]);
        assert!(!relay.is_active());
    }
    
    #[test]
    fn test_pty_relay_manager_creation() {
        let manager = PtyRelayManager::new();
        assert_eq!(manager.get_count(), 0);
    }
}
