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

/// Encode bytes as standard base64 string
pub fn encode_base64(data: &[u8]) -> String {
    STANDARD.encode(data)
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

pub fn recv_line(reader: &mut impl Read, buf: &mut Vec<u8>, newline: &str) -> Result<String> {
    recv_line_with_newline(reader, buf, newline)
}

/// Read a line with custom newline delimiter (e.g., "!\n" for Windows)
pub fn recv_line_with_newline(reader: &mut impl Read, buf: &mut Vec<u8>, newline: &str) -> Result<String> {
    let newline_bytes = newline.as_bytes();
    let nl_len = newline_bytes.len();
    
    loop {
        let mut byte = [0u8; 1];
        match reader.read_exact(&mut byte) {
            Ok(()) => {
                buf.push(byte[0]);
                if buf.len() >= nl_len && buf[buf.len() - nl_len..] == *newline_bytes {
                    let line_bytes = buf[..buf.len() - nl_len].to_vec();
                    let line = String::from_utf8_lossy(&line_bytes).to_string();
                    buf.clear();
                    debug!("Recv: {}", line.trim());
                    return Ok(line);
                }
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

/// Strip tmux DCS sequences: \x1bP=...\x1b\\ (status line updates)
pub fn strip_tmux_status_line(buf: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(buf.len());
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == 0x1b && i + 2 < buf.len() && buf[i+1] == b'P' && buf[i+2] == b'=' {
            // Find the ST (string terminator): \x1b\\
            if let Some(end) = buf[i..].windows(2).position(|w| w == [0x1b, b'\\']) {
                i += end + 2;
                continue;
            }
        }
        result.push(buf[i]);
        i += 1;
    }
    result
}

/// Read a line, stripping leading junk before the `#` character.
/// In tmux normal mode, output may contain tmux status lines before the protocol message.
pub fn recv_line_may_has_junk(reader: &mut impl Read, buf: &mut Vec<u8>, newline: &str, expect_type: &str) -> Result<String> {
    let line = recv_line_with_newline(reader, buf, newline)?;
    let line_bytes = line.as_bytes();
    
    // Find the last occurrence of #<expect_type>: in the line
    let pattern = format!("#{}:", expect_type);
    let pattern_bytes = pattern.as_bytes();
    
    if let Some(idx) = line_bytes.windows(pattern_bytes.len())
        .enumerate()
        .rfind(|(_, w)| *w == pattern_bytes)
        .map(|(i, _)| i)
    {
        let cleaned = &line_bytes[idx..];
        let cleaned = strip_tmux_status_line(cleaned);
        Ok(String::from_utf8_lossy(&cleaned).into_owned())
    } else {
        // No match, just return the line stripped of tmux sequences
        let cleaned = strip_tmux_status_line(line_bytes);
        Ok(String::from_utf8_lossy(&cleaned).into_owned())
    }
}

pub fn send_integer(writer: &mut impl Write, typ: &str, value: i64, newline: &str) -> Result<()> {
    send_line(writer, typ, &value.to_string(), newline)
}

pub fn recv_integer(reader: &mut impl Read, typ: &str, buf: &mut Vec<u8>, newline: &str) -> Result<i64> {
    let line = recv_line(reader, buf, newline)?;
    let idx = line.find(':').ok_or_else(|| anyhow!("Invalid format: {}", line))?;
    let prefix = &line[1..idx];
    if prefix != typ {
        return Err(anyhow!("Expected {}, got {}", typ, prefix));
    }
    let value = line[idx+1..].trim().parse::<i64>()?;
    Ok(value)
}

pub fn recv_integer_with_newline(reader: &mut impl Read, typ: &str, buf: &mut Vec<u8>, newline: &str) -> Result<i64> {
    let line = recv_line_with_newline(reader, buf, newline)?;
    let idx = line.find(':').ok_or_else(|| anyhow!("Invalid format: {}", line))?;
    let prefix = &line[1..idx];
    if prefix != typ {
        return Err(anyhow!("Expected {}, got {}", typ, prefix));
    }
    let value = line[idx+1..].trim().parse::<i64>()?;
    Ok(value)
}

pub fn check_integer(reader: &mut impl Read, expected: i64, buf: &mut Vec<u8>, newline: &str) -> Result<()> {
    let result = recv_integer(reader, "SUCC", buf, newline)?;
    if result != expected {
        return Err(anyhow!("Integer check failed: expected {}, got {}", expected, result));
    }
    Ok(())
}

pub fn check_integer_with_newline(reader: &mut impl Read, expected: i64, buf: &mut Vec<u8>, newline: &str) -> Result<()> {
    let result = recv_integer_with_newline(reader, "SUCC", buf, newline)?;
    if result != expected {
        return Err(anyhow!("Integer check failed: expected {}, got {}", expected, result));
    }
    Ok(())
}

/// Send a V2 pipeline ack: #SUCC:<length>/<step>\n
pub fn send_pipeline_ack(writer: &mut impl Write, length: i64, step: i64, newline: &str) -> Result<()> {
    send_line(writer, "SUCC", &format!("{}/{}", length, step), newline)
}

/// Receive a V2 pipeline ack and parse the length and step
pub fn recv_pipeline_ack(reader: &mut impl Read, buf: &mut Vec<u8>, newline: &str) -> Result<(i64, i64)> {
    let line = recv_line(reader, buf, newline)?;
    let idx = line.find(':').ok_or_else(|| anyhow!("Invalid format: {}", line))?;
    let prefix = &line[1..idx];
    if prefix != "SUCC" {
        return Err(anyhow!("Expected SUCC, got {}", prefix));
    }
    let parts: Vec<&str> = line[idx+1..].trim().split('/').collect();
    if parts.len() != 2 {
        return Err(anyhow!("Invalid pipeline ack format: {}", line));
    }
    let length = parts[0].parse::<i64>()?;
    let step = parts[1].parse::<i64>()?;
    Ok((length, step))
}

/// Check a V2 pipeline ack: receives #SUCC:<length>/<step>\n and verifies length
pub fn check_pipeline_ack(reader: &mut impl Read, expected_length: i64, buf: &mut Vec<u8>, newline: &str) -> Result<i64> {
    let (length, step) = recv_pipeline_ack(reader, buf, newline)?;
    if length != expected_length {
        return Err(anyhow!("Pipeline ack length check failed: expected {}, got {}", expected_length, length));
    }
    Ok(step)
}

pub fn recv_pipeline_ack_with_newline(reader: &mut impl Read, buf: &mut Vec<u8>, newline: &str) -> Result<(i64, i64)> {
    let line = recv_line_with_newline(reader, buf, newline)?;
    let idx = line.find(':').ok_or_else(|| anyhow!("Invalid format: {}", line))?;
    let prefix = &line[1..idx];
    if prefix != "SUCC" {
        return Err(anyhow!("Expected SUCC, got {}", prefix));
    }
    let parts: Vec<&str> = line[idx+1..].trim().split('/').collect();
    if parts.len() != 2 {
        return Err(anyhow!("Invalid pipeline ack format: {}", line));
    }
    let length = parts[0].parse::<i64>()?;
    let step = parts[1].parse::<i64>()?;
    Ok((length, step))
}

pub fn check_pipeline_ack_with_newline(reader: &mut impl Read, expected_length: i64, buf: &mut Vec<u8>, newline: &str) -> Result<i64> {
    let (length, step) = recv_pipeline_ack_with_newline(reader, buf, newline)?;
    if length != expected_length {
        return Err(anyhow!("Pipeline ack length check failed: expected {}, got {}", expected_length, length));
    }
    Ok(step)
}

pub fn send_string(writer: &mut impl Write, typ: &str, value: &str, newline: &str) -> Result<()> {
    let encoded = encode_buffer(value.as_bytes())?;
    send_line(writer, typ, &encoded, newline)
}

pub fn recv_string(reader: &mut impl Read, typ: &str, buf: &mut Vec<u8>, newline: &str) -> Result<String> {
    let line = recv_line(reader, buf, newline)?;
    let idx = line.find(':').ok_or_else(|| anyhow!("Invalid format: {}", line))?;
    let prefix = &line[1..idx];
    if prefix != typ {
        return Err(anyhow!("Expected {}, got {}", typ, prefix));
    }
    let encoded = line[idx+1..].trim().to_string();
    let decoded = decode_buffer(&encoded)?;
    Ok(String::from_utf8(decoded)?)
}

/// Receive a zlib+base64 encoded string, with junk tolerance (tmux status lines, etc.)
pub fn recv_string_may_has_junk(reader: &mut impl Read, typ: &str, buf: &mut Vec<u8>, newline: &str) -> Result<String> {
    let line = recv_line_may_has_junk(reader, buf, newline, typ)?;
    let idx = line.find(':').ok_or_else(|| anyhow!("Invalid format: {}", line))?;
    let prefix = &line[1..idx];
    if prefix != typ {
        return Err(anyhow!("Expected {}, got {}", typ, prefix));
    }
    let encoded = line[idx+1..].trim().to_string();
    let decoded = decode_buffer(&encoded)?;
    Ok(String::from_utf8(decoded)?)
}

pub fn check_string(reader: &mut impl Read, expected: &str, buf: &mut Vec<u8>, newline: &str) -> Result<()> {
    let result = recv_string(reader, "SUCC", buf, newline)?;
    if result != expected {
        return Err(anyhow!("String check failed: expected {}, got {}", expected, result));
    }
    Ok(())
}

pub fn send_binary(writer: &mut impl Write, typ: &str, data: &[u8], newline: &str) -> Result<()> {
    let encoded = encode_buffer(data)?;
    send_line(writer, typ, &encoded, newline)
}

pub fn recv_binary(reader: &mut impl Read, typ: &str, buf: &mut Vec<u8>, newline: &str) -> Result<Vec<u8>> {
    let line = recv_line(reader, buf, newline)?;
    let idx = line.find(':').ok_or_else(|| anyhow!("Invalid format: {}", line))?;
    let prefix = &line[1..idx];
    if prefix != typ {
        return Err(anyhow!("Expected {}, got {}", typ, prefix));
    }
    let encoded = line[idx+1..].trim().to_string();
    decode_buffer(&encoded)
}

pub fn check_binary(reader: &mut impl Read, expected: &[u8], buf: &mut Vec<u8>, newline: &str) -> Result<()> {
    let result = recv_binary(reader, "SUCC", buf, newline)?;
    if result != expected {
        return Err(anyhow!("Binary check failed"));
    }
    Ok(())
}

pub fn get_escape_chars(escape_all: bool) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut chars: Vec<(Vec<u8>, Vec<u8>)> = vec![
        (vec![0xee], vec![0xee, 0xee]),          // Escape 0xEE -> 0xEE 0xEE
        (vec![0x7e], vec![0xee, 0x31]),          // Escape ~ -> 0xEE 0x31
    ];
    if escape_all {
        let extra = [0x02u8, 0x0d, 0x10, 0x11, 0x13, 0x18, 0x1b, 0x1d, 0x8d, 0x90, 0x91, 0x93, 0x9d];
        for (i, &c) in extra.iter().enumerate() {
            chars.push((vec![c], vec![0xee, (0x41 + i) as u8]));
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

pub fn resolve_home_dir(path: &str) -> Result<String> {
    if path.starts_with("~/") || path == "~" {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map_err(|_| anyhow!("Cannot determine home directory"))?;
        if path == "~" {
            Ok(home)
        } else {
            Ok(format!("{}/{}", home, &path[2..]))
        }
    } else {
        Ok(path.to_string())
    }
}

pub fn strip_vt100(data: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0x1b {
            // ESC sequence
            if i + 1 < data.len() {
                match data[i + 1] {
                    b'[' => {
                        // CSI sequence: ESC [ ... final byte
                        i += 2;
                        while i < data.len() {
                            let c = data[i];
                            i += 1;
                            if (0x40..=0x7e).contains(&c) {
                                break;
                            }
                        }
                    }
                    b']' => {
                        // OSC sequence: ESC ] ... ST (ESC \)
                        i += 2;
                        while i < data.len() {
                            if data[i] == 0x1b && i + 1 < data.len() && data[i + 1] == b'\\' {
                                i += 2;
                                break;
                            }
                            i += 1;
                        }
                    }
                    _ => {
                        // Two-byte escape: skip both
                        i += 2;
                    }
                }
            } else {
                i += 1;
            }
        } else {
            result.push(data[i]);
            i += 1;
        }
    }
    result
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