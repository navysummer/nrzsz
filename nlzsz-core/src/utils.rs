use std::io::{self, Read, Write, Cursor};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use flate2::read::{ZlibDecoder, ZlibEncoder};
use flate2::Compression;
use anyhow::{Result, anyhow};
use log::debug;

pub fn encode_buffer(buf: &[u8]) -> Result<String> {
    let mut encoder = ZlibEncoder::new(Cursor::new(buf), Compression::default());
    let mut compressed = Vec::new();
    encoder.read_to_end(&mut compressed)?;
    Ok(STANDARD.encode(&compressed))
}

pub fn decode_buffer(buf: &str) -> Result<Vec<u8>> {
    let decoded = STANDARD.decode(buf)?;
    let mut decoder = ZlibDecoder::new(Cursor::new(&decoded));
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed)?;
    Ok(decompressed)
}

pub fn send_line(writer: &mut impl Write, typ: &str, buf: &str, newline: &str) -> Result<()> {
    let line = format!("#{}:{}{}", typ, buf, newline);
    debug!("Send: {}", line.trim());
    writer.write_all(line.as_bytes())?;
    writer.flush()?;
    Ok(())
}

pub fn recv_line(reader: &mut impl Read, buf: &mut Vec<u8>) -> Result<String> {
    loop {
        let mut byte = [0u8; 1];
        match reader.read_exact(&mut byte) {
            Ok(()) => {
                if byte[0] == b'\n' {
                    let line = String::from_utf8_lossy(buf).to_string();
                    buf.clear();
                    debug!("Recv: {}", line.trim());
                    return Ok(line);
                }
                buf.push(byte[0]);
            }
            Err(e) => {
                if e.kind() == io::ErrorKind::UnexpectedEof {
                    return Ok(String::from_utf8_lossy(buf).to_string());
                }
                return Err(e.into());
            }
        }
    }
}

pub fn send_integer(writer: &mut impl Write, typ: &str, value: i64, newline: &str) -> Result<()> {
    send_line(writer, typ, &value.to_string(), newline)
}

pub fn recv_integer(reader: &mut impl Read, typ: &str, buf: &mut Vec<u8>) -> Result<i64> {
    let line = recv_line(reader, buf)?;
    let idx = line.find(':').ok_or_else(|| anyhow!("Invalid format: {}", line))?;
    let prefix = &line[1..idx];
    if prefix != typ {
        return Err(anyhow!("Expected {}, got {}", typ, prefix));
    }
    let value = line[idx+1..].trim().parse::<i64>()?;
    Ok(value)
}

pub fn check_integer(reader: &mut impl Read, expected: i64, buf: &mut Vec<u8>) -> Result<()> {
    let result = recv_integer(reader, "SUCC", buf)?;
    if result != expected {
        return Err(anyhow!("Integer check failed: expected {}, got {}", expected, result));
    }
    Ok(())
}

pub fn send_string(writer: &mut impl Write, typ: &str, value: &str, newline: &str) -> Result<()> {
    let encoded = encode_buffer(value.as_bytes())?;
    send_line(writer, typ, &encoded, newline)
}

pub fn recv_string(reader: &mut impl Read, typ: &str, buf: &mut Vec<u8>) -> Result<String> {
    let line = recv_line(reader, buf)?;
    let idx = line.find(':').ok_or_else(|| anyhow!("Invalid format: {}", line))?;
    let prefix = &line[1..idx];
    if prefix != typ {
        return Err(anyhow!("Expected {}, got {}", typ, prefix));
    }
    let encoded = line[idx+1..].trim().to_string();
    let decoded = decode_buffer(&encoded)?;
    Ok(String::from_utf8(decoded)?)
}

pub fn check_string(reader: &mut impl Read, expected: &str, buf: &mut Vec<u8>) -> Result<()> {
    let result = recv_string(reader, "SUCC", buf)?;
    if result != expected {
        return Err(anyhow!("String check failed: expected {}, got {}", expected, result));
    }
    Ok(())
}

pub fn send_binary(writer: &mut impl Write, typ: &str, data: &[u8], newline: &str) -> Result<()> {
    let encoded = encode_buffer(data)?;
    send_line(writer, typ, &encoded, newline)
}

pub fn recv_binary(reader: &mut impl Read, typ: &str, buf: &mut Vec<u8>) -> Result<Vec<u8>> {
    let line = recv_line(reader, buf)?;
    let idx = line.find(':').ok_or_else(|| anyhow!("Invalid format: {}", line))?;
    let prefix = &line[1..idx];
    if prefix != typ {
        return Err(anyhow!("Expected {}, got {}", typ, prefix));
    }
    let encoded = line[idx+1..].trim().to_string();
    decode_buffer(&encoded)
}

pub fn check_binary(reader: &mut impl Read, expected: &[u8], buf: &mut Vec<u8>) -> Result<()> {
    let result = recv_binary(reader, "SUCC", buf)?;
    if result != expected {
        return Err(anyhow!("Binary check failed"));
    }
    Ok(())
}

pub fn get_escape_chars(escape_all: bool) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut chars: Vec<(Vec<u8>, Vec<u8>)> = vec![
        (vec![0x00, 0xee], vec![0x00, 0xee, 0xee]),
        (vec![0x00, 0x7e], vec![0x00, 0xee, 0x31]),
    ];
    if escape_all {
        let extra = [0x02u8, 0x0d, 0x10, 0x11, 0x13, 0x18, 0x1b, 0x1d, 0x8d, 0x90, 0x91, 0x93, 0x9d];
        for (i, &c) in extra.iter().enumerate() {
            chars.push((vec![0x00, c], vec![0x00, (0x41 + i) as u8]));
        }
    }
    chars
}

pub fn escape_data(data: &[u8], escape_chars: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    if escape_chars.is_empty() {
        return data.to_vec();
    }
    
    let mut result = Vec::new();
    let mut i = 0;
    
    while i < data.len() {
        let mut matched = false;
        for (from, to) in escape_chars {
            if data[i..].starts_with(from) {
                result.extend_from_slice(to);
                i += from.len();
                matched = true;
                break;
            }
        }
        if !matched {
            result.push(data[i]);
            i += 1;
        }
    }
    
    result
}

pub fn unescape_data(data: &[u8], escape_chars: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    if escape_chars.is_empty() {
        return data.to_vec();
    }
    
    let mut result = Vec::new();
    let mut i = 0;
    
    while i < data.len() {
        let mut matched = false;
        for (from, to) in escape_chars {
            if data[i..].starts_with(to) {
                result.extend_from_slice(from);
                i += to.len();
                matched = true;
                break;
            }
        }
        if !matched {
            result.push(data[i]);
            i += 1;
        }
    }
    
    result
}

pub fn calculate_md5(data: &[u8]) -> Vec<u8> {
    md5::compute(data).to_vec()
}

pub fn get_columns() -> u32 {
    if let Ok(output) = std::process::Command::new("stty").arg("size").output() {
        if let Ok(s) = String::from_utf8(output.stdout) {
            let parts: Vec<&str> = s.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Ok(cols) = parts[1].parse::<u32>() {
                    return cols;
                }
            }
        }
    }
    0
}

#[cfg(windows)]
pub mod windows_flush {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use winapi::um::fileapi::{GetStdHandle, STD_OUTPUT_HANDLE};
    use winapi::um::handleapi::INVALID_HANDLE_VALUE;
    use winapi::um::winbase::ENABLE_VIRTUAL_TERMINAL_PROCESSING;
    use winapi::um::consoleapi::GetConsoleMode;
    use winapi::um::consoleapi::SetConsoleMode;
    
    pub fn disable_line_wrap() -> Result<(), io::Error> {
        unsafe {
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::new(io::ErrorKind::Other, "Invalid handle"));
            }
            
            let mut mode = 0;
            if GetConsoleMode(handle, &mut mode) == 0 {
                return Err(io::Error::new(io::ErrorKind::Other, "GetConsoleMode failed"));
            }
            
            // Disable ENABLE_PROCESSED_OUTPUT to prevent line wrapping
            mode &= !ENABLE_VIRTUAL_TERMINAL_PROCESSING;
            
            if SetConsoleMode(handle, mode) == 0 {
                return Err(io::Error::new(io::ErrorKind::Other, "SetConsoleMode failed"));
            }
            
            Ok(())
        }
    }
    
    pub fn enable_line_wrap() -> Result<(), io::Error> {
        unsafe {
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::new(io::ErrorKind::Other, "Invalid handle"));
            }
            
            let mut mode = 0;
            if GetConsoleMode(handle, &mut mode) == 0 {
                return Err(io::Error::new(io::ErrorKind::Other, "GetConsoleMode failed"));
            }
            
            // Enable ENABLE_VIRTUAL_TERMINAL_PROCESSING
            mode |= ENABLE_VIRTUAL_TERMINAL_PROCESSING;
            
            if SetConsoleMode(handle, mode) == 0 {
                return Err(io::Error::new(io::ErrorKind::Other, "SetConsoleMode failed"));
            }
            
            Ok(())
        }
    }
}