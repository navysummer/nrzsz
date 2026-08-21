use std::net::{TcpListener, TcpStream};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use anyhow::{Result, anyhow};
use log::{debug, warn};

pub fn get_hello_constant(unique_id: &str, port: u16) -> (String, String) {
    let uid = if unique_id.len() > 2 {
        &unique_id[..unique_id.len() - 2]
    } else {
        unique_id
    };
    let client_hello = format!("::NRZSZ::CLIENT::HELLO::{}:{}", uid, port);
    let server_hello = format!("::NRZSZ::SERVER::HELLO::{}:{}", uid, port);
    (client_hello, server_hello)
}

fn read_line_from_stream(stream: &mut TcpStream) -> Result<String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte)?;
        if n == 0 {
            return Err(anyhow!("Tunnel connection closed"));
        }
        if byte[0] == b'\n' {
            break;
        }
        buf.push(byte[0]);
        if buf.len() > 200 {
            return Err(anyhow!("Tunnel hello line too long"));
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

pub struct TunnelConnection {
    stream: Option<TcpStream>,
    port: u16,
    is_server: bool,
    connected: Arc<AtomicBool>,
}

use std::sync::Arc;

/// Perform the server-side tunnel handshake on an already-accepted stream.
/// Reads the client hello, verifies it, and responds with the server hello.
pub fn server_tunnel_handshake(stream: &mut TcpStream, unique_id: &str, port: u16) -> Result<()> {
    let (client_hello, server_hello) = get_hello_constant(unique_id, port);
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let received = read_line_from_stream(stream)?;
    let received = received.trim().to_string();
    if received != client_hello {
        return Err(anyhow!(
            "Tunnel: unexpected client hello (expected match for {:?}, got {:?})",
            client_hello,
            received
        ));
    }
    stream.write_all(server_hello.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

impl TunnelConnection {
    pub fn new(port: u16) -> Self {
        Self {
            stream: None,
            port,
            is_server: false,
            connected: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn listen_and_accept(&mut self, unique_id: &str) -> Result<()> {
        let addr = format!("127.0.0.1:{}", self.port);
        let listener = TcpListener::bind(&addr)?;
        self.is_server = true;

        let (client_hello, server_hello) = get_hello_constant(unique_id, self.port);
        debug!("Tunnel server listening on {}, expecting: {}", addr, client_hello);

        listener.set_nonblocking(false)?;
        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                    match read_line_from_stream(&mut stream) {
                        Ok(received) => {
                            let received = received.trim();
                            if received == client_hello {
                                debug!("Tunnel client handshake verified");
                                stream.write_all(server_hello.as_bytes())?;
                                stream.write_all(b"\n")?;
                                stream.flush()?;
                                self.stream = Some(stream);
                                self.connected.store(true, Ordering::SeqCst);
                                return Ok(());
                            } else {
                                warn!("Tunnel: unexpected hello: {:?}", received);
                            }
                        }
                        Err(e) => {
                            warn!("Tunnel: failed to read hello: {}", e);
                        }
                    }
                }
                Err(e) => {
                    warn!("Tunnel accept error: {}", e);
                }
            }
        }
        Err(anyhow!("Tunnel listener closed"))
    }

    pub fn connect_and_handshake(&mut self, addr: &str, unique_id: &str) -> Result<()> {
        self.is_server = false;
        let (client_hello, server_hello) = get_hello_constant(unique_id, self.port);
        debug!("Tunnel client connecting to {}, sending: {}", addr, client_hello);

        let mut stream = TcpStream::connect(addr)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;

        stream.write_all(client_hello.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;

        let received = read_line_from_stream(&mut stream)?;
        let received = received.trim();

        if received == server_hello {
            debug!("Tunnel server handshake verified");
            self.stream = Some(stream);
            self.connected.store(true, Ordering::SeqCst);
            Ok(())
        } else {
            Err(anyhow!("Tunnel: unexpected server hello: {:?}", received))
        }
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

    pub fn close(&mut self) -> Result<()> {
        if let Some(ref mut stream) = self.stream {
            stream.shutdown(std::net::Shutdown::Both)?;
        }
        self.stream = None;
        self.connected.store(false, Ordering::SeqCst);
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    pub fn is_server(&self) -> bool {
        self.is_server
    }

    pub fn get_port(&self) -> u16 {
        self.port
    }

    pub fn get_stream_ref(&self) -> Option<&TcpStream> {
        self.stream.as_ref()
    }

    pub fn get_stream_mut(&mut self) -> Option<&mut TcpStream> {
        self.stream.as_mut()
    }

    pub fn into_stream(self) -> Option<TcpStream> {
        self.stream
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
        let tunnel = TunnelConnection::new(port);
        self.tunnels.push(tunnel);
        Ok(())
    }

    pub fn get_tunnel_mut(&mut self, index: usize) -> Option<&mut TunnelConnection> {
        self.tunnels.get_mut(index)
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

    #[test]
    fn test_get_hello_constant() {
        let (client_hello, server_hello) = get_hello_constant("12345678", 9090);
        assert_eq!(client_hello, "::NRZSZ::CLIENT::HELLO::123456:9090");
        assert_eq!(server_hello, "::NRZSZ::SERVER::HELLO::123456:9090");
    }

    #[test]
    fn test_get_hello_constant_short_uid() {
        let (client_hello, server_hello) = get_hello_constant("AB", 1234);
        assert_eq!(client_hello, "::NRZSZ::CLIENT::HELLO::AB:1234");
        assert_eq!(server_hello, "::NRZSZ::SERVER::HELLO::AB:1234");
    }

    #[test]
    fn test_tunnel_handshake() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();

        let (client_hello, server_hello) = get_hello_constant("test1234", port);

        let server_handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let received = read_line_from_stream(&mut stream).unwrap();
            let received = received.trim();
            assert_eq!(received, client_hello);
            stream.write_all(server_hello.as_bytes()).unwrap();
            stream.write_all(b"\n").unwrap();
            stream.flush().unwrap();
        });

        let mut tunnel = TunnelConnection::new(port);
        tunnel.connect_and_handshake(&format!("127.0.0.1:{}", port), "test1234")?;
        assert!(tunnel.is_connected());

        server_handle.join().unwrap();
        Ok(())
    }
}
