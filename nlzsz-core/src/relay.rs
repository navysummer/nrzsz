use std::net::TcpStream;
use std::io::{Read, Write, BufReader, BufWriter};
use anyhow::Result;
use log::debug;

pub struct RelayConnection {
    reader: BufReader<Box<dyn Read>>,
    writer: BufWriter<Box<dyn Write>>,
    host: String,
    port: u16,
    buffer_size: usize,
}

impl RelayConnection {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            reader: BufReader::new(Box::new(std::io::empty())),
            writer: BufWriter::new(Box::new(std::io::sink())),
            host: host.to_string(),
            port,
            buffer_size: 64 * 1024,
        }
    }
    
    pub fn connect(&mut self) -> Result<()> {
        let addr = format!("{}:{}", self.host, self.port);
        debug!("Connecting to relay: {}", addr);
        
        let stream = TcpStream::connect(&addr)?;
        stream.set_nodelay(true)?;
        
        self.reader = BufReader::new(Box::new(stream.try_clone()?));
        self.writer = BufWriter::new(Box::new(stream));
        
        Ok(())
    }
    
    pub fn send_data(&mut self, data: &[u8]) -> Result<()> {
        // Send length prefix
        let len = data.len() as u32;
        self.writer.write_all(&len.to_le_bytes())?;
        
        // Send data
        self.writer.write_all(data)?;
        self.writer.flush()?;
        
        Ok(())
    }
    
    pub fn receive_data(&mut self) -> Result<Vec<u8>> {
        // Read length prefix
        let mut len_buf = [0u8; 4];
        self.reader.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;
        
        // Read data
        let mut data = vec![0u8; len];
        self.reader.read_exact(&mut data)?;
        
        Ok(data)
    }
    
    pub fn close(&mut self) -> Result<()> {
        let _ = self.writer.flush();
        Ok(())
    }
    
    pub fn get_host(&self) -> &str {
        &self.host
    }
    
    pub fn get_port(&self) -> u16 {
        self.port
    }
    
    pub fn set_buffer_size(&mut self, size: usize) {
        self.buffer_size = size;
    }
}

pub struct RelayManager {
    connections: Vec<RelayConnection>,
    current_index: usize,
}

impl Default for RelayManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RelayManager {
    pub fn new() -> Self {
        Self {
            connections: Vec::new(),
            current_index: 0,
        }
    }
    
    pub fn add_connection(&mut self, host: &str, port: u16) -> Result<()> {
        let mut connection = RelayConnection::new(host, port);
        connection.connect()?;
        self.connections.push(connection);
        Ok(())
    }
    
    pub fn add_ssh_connection(&mut self, ssh_args: &[String]) -> Result<()> {
        // For SSH connections, we need to use the SSH command
        // This is a simplified version - real implementation would need to handle SSH protocol
        debug!("Adding SSH relay connection: {:?}", ssh_args);
        Ok(())
    }
    
    pub fn send_action(&mut self, action: &crate::protocol::Action) -> Result<()> {
        let action_json = serde_json::to_string(action)?;
        self.send_through_relays(action_json.as_bytes())
    }
    
    pub fn receive_action(&mut self) -> Result<crate::protocol::Action> {
        let data = self.receive_from_relays()?;
        let action: crate::protocol::Action = serde_json::from_slice(&data)?;
        Ok(action)
    }
    
    pub fn send_file_data(&mut self, data: &[u8]) -> Result<()> {
        self.send_through_relays(data)
    }
    
    pub fn receive_file_data(&mut self) -> Result<Vec<u8>> {
        self.receive_from_relays()
    }
    
    pub fn send_through_relays(&mut self, data: &[u8]) -> Result<()> {
        if let Some(connection) = self.connections.first_mut() {
            connection.send_data(data)?;
        }
        Ok(())
    }
    
    pub fn receive_from_relays(&mut self) -> Result<Vec<u8>> {
        if let Some(connection) = self.connections.last_mut() {
            let data = connection.receive_data()?;
            return Ok(data);
        }
        Ok(Vec::new())
    }
    
    pub fn close_all(&mut self) -> Result<()> {
        for connection in &mut self.connections {
            connection.close()?;
        }
        self.connections.clear();
        Ok(())
    }
    
    pub fn get_connection_count(&self) -> usize {
        self.connections.len()
    }
    
    pub fn get_current_connection(&self) -> Option<&RelayConnection> {
        self.connections.get(self.current_index)
    }
    
    pub fn get_current_connection_mut(&mut self) -> Option<&mut RelayConnection> {
        self.connections.get_mut(self.current_index)
    }
    
    pub fn next_connection(&mut self) -> bool {
        if self.connections.is_empty() {
            return false;
        }
        if self.current_index < self.connections.len() - 1 {
            self.current_index += 1;
            true
        } else {
            false
        }
    }
    
    pub fn prev_connection(&mut self) -> bool {
        if self.current_index > 0 {
            self.current_index -= 1;
            true
        } else {
            false
        }
    }
    
    pub fn setup_relay_chain(&mut self, hosts: &[(String, u16)]) -> Result<()> {
        for (host, port) in hosts {
            self.add_connection(host, *port)?;
        }
        Ok(())
    }
    
    pub fn is_relay_mode(&self) -> bool {
        self.connections.len() > 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_relay_manager_creation() {
        let manager = RelayManager::new();
        assert_eq!(manager.get_connection_count(), 0);
        assert!(!manager.is_relay_mode());
    }
    
    #[test]
    fn test_relay_connection_creation() {
        let connection = RelayConnection::new("localhost", 8080);
        assert_eq!(connection.get_host(), "localhost");
        assert_eq!(connection.get_port(), 8080);
    }
    
    #[test]
    fn test_relay_navigation() {
        let mut manager = RelayManager::new();
        // Can't test navigation without connections
        // next_connection returns false when no connections
        assert!(!manager.next_connection());
        // prev_connection returns false when current_index is 0
        assert!(!manager.prev_connection());
    }
}
