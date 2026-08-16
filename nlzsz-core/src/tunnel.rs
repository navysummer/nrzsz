use std::net::{TcpListener, TcpStream};
use std::io::{Read, Write};
use anyhow::{Result, anyhow};
use log::debug;

pub struct TunnelConnection {
    listener: Option<TcpListener>,
    stream: Option<TcpStream>,
    port: u16,
    is_server: bool,
}

impl TunnelConnection {
    pub fn new(port: u16) -> Self {
        Self {
            listener: None,
            stream: None,
            port,
            is_server: false,
        }
    }
    
    pub fn listen(&mut self) -> Result<()> {
        let addr = format!("127.0.0.1:{}", self.port);
        self.listener = Some(TcpListener::bind(&addr)?);
        self.is_server = true;
        debug!("Tunnel listening on {}", addr);
        Ok(())
    }
    
    pub fn accept(&mut self) -> Result<()> {
        if let Some(ref listener) = self.listener {
            let (stream, addr) = listener.accept()?;
            self.stream = Some(stream);
            debug!("Tunnel connection accepted from {}", addr);
            Ok(())
        } else {
            Err(anyhow!("Tunnel not listening"))
        }
    }
    
    pub fn connect(&mut self, addr: &str) -> Result<()> {
        let stream = TcpStream::connect(addr)?;
        self.stream = Some(stream);
        self.is_server = false;
        debug!("Tunnel connected to {}", addr);
        Ok(())
    }
    
    pub fn send_data(&mut self, data: &[u8]) -> Result<()> {
        if let Some(ref mut stream) = self.stream {
            stream.write_all(data)?;
            stream.flush()?;
            Ok(())
        } else {
            Err(anyhow!("Tunnel not connected"))
        }
    }
    
    pub fn receive_data(&mut self, buffer: &mut [u8]) -> Result<usize> {
        if let Some(ref mut stream) = self.stream {
            let bytes_read = stream.read(buffer)?;
            Ok(bytes_read)
        } else {
            Err(anyhow!("Tunnel not connected"))
        }
    }
    
    pub fn send_all(&mut self, data: &[u8]) -> Result<()> {
        self.send_data(data)
    }
    
    pub fn receive_all(&mut self, size: usize) -> Result<Vec<u8>> {
        let mut buffer = vec![0u8; size];
        let bytes_read = self.receive_data(&mut buffer)?;
        buffer.truncate(bytes_read);
        Ok(buffer)
    }
    
    pub fn close(&mut self) -> Result<()> {
        if let Some(ref mut stream) = self.stream {
            stream.shutdown(std::net::Shutdown::Both)?;
        }
        self.stream = None;
        self.listener = None;
        Ok(())
    }
    
    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }
    
    pub fn is_server(&self) -> bool {
        self.is_server
    }
    
    pub fn get_port(&self) -> u16 {
        self.port
    }
}

pub struct TunnelManager {
    tunnels: Vec<TunnelConnection>,
}

impl Default for TunnelManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TunnelManager {
    pub fn new() -> Self {
        Self {
            tunnels: Vec::new(),
        }
    }
    
    pub fn create_tunnel(&mut self, port: u16) -> Result<()> {
        let mut tunnel = TunnelConnection::new(port);
        tunnel.listen()?;
        self.tunnels.push(tunnel);
        Ok(())
    }
    
    pub fn connect_tunnel(&mut self, port: u16, addr: &str) -> Result<()> {
        let mut tunnel = TunnelConnection::new(port);
        tunnel.connect(addr)?;
        self.tunnels.push(tunnel);
        Ok(())
    }
    
    pub fn accept_connection(&mut self, tunnel_index: usize) -> Result<()> {
        if let Some(tunnel) = self.tunnels.get_mut(tunnel_index) {
            tunnel.accept()
        } else {
            Err(anyhow!("Invalid tunnel index"))
        }
    }
    
    pub fn send_data(&mut self, tunnel_index: usize, data: &[u8]) -> Result<()> {
        if let Some(tunnel) = self.tunnels.get_mut(tunnel_index) {
            tunnel.send_data(data)
        } else {
            Err(anyhow!("Invalid tunnel index"))
        }
    }
    
    pub fn receive_data(&mut self, tunnel_index: usize, buffer: &mut [u8]) -> Result<usize> {
        if let Some(tunnel) = self.tunnels.get_mut(tunnel_index) {
            tunnel.receive_data(buffer)
        } else {
            Err(anyhow!("Invalid tunnel index"))
        }
    }
    
    pub fn close_tunnel(&mut self, tunnel_index: usize) -> Result<()> {
        if let Some(tunnel) = self.tunnels.get_mut(tunnel_index) {
            tunnel.close()
        } else {
            Err(anyhow!("Invalid tunnel index"))
        }
    }
    
    pub fn close_all(&mut self) -> Result<()> {
        for tunnel in &mut self.tunnels {
            tunnel.close()?;
        }
        self.tunnels.clear();
        Ok(())
    }
    
    pub fn get_tunnel_count(&self) -> usize {
        self.tunnels.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_tunnel_connection_creation() {
        let tunnel = TunnelConnection::new(8080);
        assert_eq!(tunnel.get_port(), 8080);
        assert!(!tunnel.is_connected());
        assert!(!tunnel.is_server());
    }
    
    #[test]
    fn test_tunnel_manager_creation() {
        let manager = TunnelManager::new();
        assert_eq!(manager.get_tunnel_count(), 0);
    }
}
