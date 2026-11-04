use std::net::TcpStream;
use std::io::{Read, Write};
use std::time::Duration;
use anyhow::{Result, anyhow};

pub fn get_hello_constant(unique_id: &str, port: u16) -> (String, String) {
    let uid = if unique_id.len() > 2 {
        &unique_id[..unique_id.len() - 2]
    } else {
        unique_id
    };
    let client_hello = format!("::TRZSZ::CLIENT::HELLO::{}:{}", uid, port);
    let server_hello = format!("::TRZSZ::SERVER::HELLO::{}:{}", uid, port);
    (client_hello, server_hello)
}

pub fn read_line_from_stream(stream: &mut TcpStream) -> Result<String> {
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
