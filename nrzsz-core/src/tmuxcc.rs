use std::process::Command;
use anyhow::{Result, anyhow};
use log::debug;

pub fn encode_hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect()
}

pub fn send_keys(pane_id: &str, keys_hex: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args(["-CC", "send-keys", "-t", pane_id, "-l", keys_hex])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.is_empty() && !stderr.contains("unknown") {
            return Err(anyhow!("tmux send-keys failed: {}", stderr.trim()));
        }
    }
    Ok(())
}

pub fn send_pane_data(pane_id: &str, data: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "-l", data])
        .output()?;
    if !output.status.success() {
        let output2 = Command::new("tmux")
            .args(["-CC", "send-keys", "-t", pane_id, "-l", &encode_hex(data.as_bytes())])
            .output()?;
        if !output2.status.success() {
            return Err(anyhow!("tmux send-keys failed"));
        }
    }
    Ok(())
}

pub fn parse_output(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if !line.starts_with("%output") {
        return None;
    }
    let rest = &line["%output".len()..];
    let rest = rest.trim();
    let mut parts = rest.splitn(2, char::is_whitespace);
    let pane_id = parts.next()?.trim().to_string();
    let data = parts.next()?.trim().to_string();
    if pane_id.is_empty() { None } else { Some((pane_id, data)) }
}

pub fn parse_extended_output(line: &str) -> Option<(String, Vec<u8>)> {
    let line = line.trim();
    if !line.starts_with("%extended-output") {
        return None;
    }
    let rest = &line["%extended-output".len()..];
    let rest = rest.trim();
    let mut parts = rest.splitn(3, |c: char| c.is_whitespace() || c == ':');
    let pane_id = parts.next()?.trim().to_string();
    let _num_bytes = parts.next()?.trim().parse::<usize>().ok()?;
    let data = parts.next()?.trim();
    let bytes = parse_output_data(data);
    Some((pane_id, bytes))
}

fn parse_output_data(data: &str) -> Vec<u8> {
    use base64::Engine as _;
    if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(data.trim()) {
        return decoded;
    }
    data.as_bytes().to_vec()
}

pub fn match_tmux_ack(line: &str, seq: u32) -> Option<bool> {
    let line = line.trim();
    if line.starts_with(&format!("%end {}", seq)) || line.starts_with(&format!("%end{}", seq)) {
        Some(true)
    } else if line.starts_with(&format!("%error {}", seq)) || line.starts_with(&format!("%error{}", seq)) {
        Some(false)
    } else {
        None
    }
}

pub fn get_control_pane_id() -> Result<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-p", "#{pane_id}"])
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let pane_id = stdout.trim().to_string();
    if pane_id.is_empty() {
        Err(anyhow!("Failed to get tmux pane ID"))
    } else {
        Ok(pane_id)
    }
}

pub fn wait_for_ack(lines: &mut impl Iterator<Item = String>, seq: u32) -> Result<()> {
    for line in lines {
        if let Some(is_end) = match_tmux_ack(&line, seq) {
            if is_end { return Ok(()); }
            return Err(anyhow!("tmux command error: {}", line.trim()));
        }
        debug!("tmux ack wait: (skipping) {}", line.trim());
    }
    Err(anyhow!("tmux ack timeout"))
}

/// Encode tmux output with octal encoding for non-printable characters.
/// Matches Go's encodeTmuxOutput.
pub fn encode_tmux_output(prefix: &str, output: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(prefix.len() + output.len() * 4 + 2);
    buf.extend_from_slice(prefix.as_bytes());
    for &b in output {
        if b < b' ' || b == b'\\' || b > b'~' {
            buf.extend_from_slice(format!("\\{:03o}", b).as_bytes());
        } else {
            buf.push(b);
        }
    }
    buf.extend_from_slice(b"\r\n");
    buf
}

/// Detect tmux `-CC` drag input (`send -lt %N /` or the iTerm2 bracketed-paste
/// sequence), matching Go's `hasTmuxSendPrefix` (drag.go).
/// Only meaningful on macOS (iTerm2 drags); Go gates this to macOS too.
pub fn has_tmux_send_prefix(buf: &[u8]) -> bool {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = buf;
        return false;
    }

    #[cfg(target_os = "macos")]
    {
        let mut i = 0;
        // send -lt %N /
        if match_at(buf, &mut i, "send -lt %")
            && skip_num(buf, &mut i)
            && match_at(buf, &mut i, " /")
            && !match_at(buf, &mut i, "\r")
        {
            return true;
        }

        i = 0;
        // send -t %N 0x1b 0x5b;
        if !match_at(buf, &mut i, "send -t %")
            || !skip_num(buf, &mut i)
            || !match_at(buf, &mut i, " 0x1b 0x5b; ")
        {
            return false;
        }
        // send -lt %N 200;
        if !match_at(buf, &mut i, "send -lt %")
            || !skip_num(buf, &mut i)
            || !match_at(buf, &mut i, " 200; ")
        {
            return false;
        }
        // send -t %N 0x7e;
        if !match_at(buf, &mut i, "send -t %")
            || !skip_num(buf, &mut i)
            || !match_at(buf, &mut i, " 0x7e; ")
        {
            return false;
        }
        // send -lt %N /
        if !match_at(buf, &mut i, "send -lt %")
            || !skip_num(buf, &mut i)
            || !match_at(buf, &mut i, " /")
            || match_at(buf, &mut i, "\r")
        {
            return false;
        }
        true
    }
}

#[cfg(target_os = "macos")]
fn match_at(buf: &[u8], i: &mut usize, s: &str) -> bool {
    let b = s.as_bytes();
    if buf.len() - *i < b.len() {
        return false;
    }
    for j in 0..b.len() {
        if buf[*i + j] != b[j] {
            return false;
        }
    }
    *i += b.len();
    true
}

#[cfg(target_os = "macos")]
fn skip_num(buf: &[u8], i: &mut usize) -> bool {
    let start = *i;
    while *i < buf.len() && buf[*i].is_ascii_digit() {
        *i += 1;
    }
    *i > start
}

/// Redirects the process's stdout (fd 1) and stderr (fd 2) through a pipe
/// drained by threads that re-emit each chunk via `encode_tmux_output`, so
/// that during a tmux `-CC` transfer all terminal output — including the
/// indicatif progress bar, which draws to stderr — is wrapped in tmux
/// control-mode `%output` messages (matching Go's per-write
/// `encodeTmuxOutput` wrapping in progress.go and ctrlc.go).
///
/// The redirect stays active for the lifetime of the value and is restored on
/// drop. Only meaningful on Unix; a no-op elsewhere.
pub struct TmuxOutputRedirect {
    #[cfg(unix)]
    saved_fds: [i32; 2],
    #[cfg(unix)]
    encoders: Vec<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl TmuxOutputRedirect {
    /// Start redirecting fd 1 and fd 2 through the tmux-encoder threads.
    pub fn start(prefix: &str) -> std::io::Result<Self> {
        use std::io::{Read, Write};
        use std::os::fd::FromRawFd;

        let mut saved_fds = [0i32; 2];
        let mut encoders: Vec<std::thread::JoinHandle<()>> = Vec::with_capacity(2);

        for fd in [1i32, 2i32] {
            let mut pipe_fds = [0i32; 2];
            if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
                for saved in saved_fds {
                    if saved > 0 {
                        unsafe { libc::close(saved) };
                    }
                }
                for h in encoders {
                    let _ = h.join();
                }
                return Err(std::io::Error::last_os_error());
            }
            let (read_fd, write_fd) = (pipe_fds[0], pipe_fds[1]);

            let saved_fd = unsafe { libc::dup(fd) };
            if saved_fd < 0 {
                unsafe {
                    libc::close(read_fd);
                    libc::close(write_fd);
                }
                for saved in saved_fds {
                    if saved > 0 {
                        unsafe { libc::close(saved) };
                    }
                }
                for h in encoders {
                    let _ = h.join();
                }
                return Err(std::io::Error::last_os_error());
            }

            // The encoder thread writes to its own dup of the real fd so that
            // its encoded output bypasses the redirect.
            let out_fd = unsafe { libc::dup(saved_fd) };
            if out_fd < 0 {
                unsafe {
                    libc::close(read_fd);
                    libc::close(write_fd);
                    libc::close(saved_fd);
                }
                for saved in saved_fds {
                    if saved > 0 {
                        unsafe { libc::close(saved) };
                    }
                }
                for h in encoders {
                    let _ = h.join();
                }
                return Err(std::io::Error::last_os_error());
            }

            let prefix = prefix.to_string();
            let encoder = std::thread::spawn(move || {
                let mut reader = unsafe { std::fs::File::from_raw_fd(read_fd) };
                let mut writer = unsafe { std::fs::File::from_raw_fd(out_fd) };
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let encoded = encode_tmux_output(&prefix, &buf[..n]);
                            let _ = writer.write_all(&encoded);
                            let _ = writer.flush();
                        }
                    }
                }
            });

            // Point fd at the pipe write end.
            if unsafe { libc::dup2(write_fd, fd) } < 0 {
                let err = std::io::Error::last_os_error();
                unsafe {
                    libc::close(write_fd);
                    libc::close(saved_fd);
                }
                for saved in saved_fds {
                    if saved > 0 {
                        unsafe { libc::close(saved) };
                    }
                }
                for h in encoders {
                    let _ = h.join();
                }
                let _ = encoder.join();
                return Err(err);
            }
            unsafe { libc::close(write_fd) };
            saved_fds[(fd - 1) as usize] = saved_fd;
            encoders.push(encoder);
        }

        Ok(Self {
            saved_fds,
            encoders,
        })
    }
}

#[cfg(unix)]
impl Drop for TmuxOutputRedirect {
    fn drop(&mut self) {
        unsafe {
            // Restore fd 1 and fd 2; this also closes the last reference to
            // the pipe write ends so the encoder threads see EOF and exit.
            for (i, saved) in self.saved_fds.iter().enumerate() {
                let fd = (i + 1) as i32;
                libc::dup2(*saved, fd);
                libc::close(*saved);
            }
        }
        for encoder in self.encoders.drain(..) {
            let _ = encoder.join();
        }
    }
}

#[cfg(not(unix))]
impl TmuxOutputRedirect {
    pub fn start(_prefix: &str) -> std::io::Result<Self> {
        Ok(Self {})
    }
}

/// Check if a byte can be sent as a literal character in tmux control mode.
/// Matches Go's canSendAsLiteralCharacter.
pub fn can_send_as_literal(b: u8) -> bool {
    (0x21..0x7f).contains(&b) && !matches!(b, b'"' | b'#' | b'\'' | b';' | b'\\' | b'{' | b'}')
}

/// Convert data to hex strings for tmux send-keys.
/// Matches Go's convertToHexStrings.
pub fn convert_to_hex_strings(data: &[u8]) -> String {
    data.iter().enumerate().map(|(i, &b)| {
        if i > 0 { " " } else { "" }.to_string() +
            if b == 0 { "C-Space".to_string() } else { format!("0x{:x}", b) }.as_str()
    }).collect::<Vec<_>>().join("")
}

/// Send data to tmux via control mode send-keys.
/// Matches Go's tmuxccSendKeys.
pub fn tmuxcc_send_keys(writer: &mut dyn std::io::Write, pane_id: &[u8], data: &[u8], as_literal: bool) -> Result<()> {
    let data_str = if as_literal {
        String::from_utf8_lossy(data).to_string()
    } else {
        convert_to_hex_strings(data)
    };
    let cmd = if as_literal {
        format!("send -lt {}{}\r", String::from_utf8_lossy(pane_id), data_str)
    } else {
        format!("send -t {}{}\r", String::from_utf8_lossy(pane_id), data_str)
    };
    writer.write_all(cmd.as_bytes())?;
    writer.flush()?;
    Ok(())
}

/// Write data to tmux, splitting into literal and hex chunks.
/// Matches Go's tmuxccWriteAll.
pub fn tmuxcc_write_all(writer: &mut dyn std::io::Write, pane_id: &[u8], data: &[u8]) -> Result<()> {
    const MAX_LITERAL: usize = 1000;
    const MAX_HEX: usize = MAX_LITERAL / 8;

    if data.is_empty() {
        return Ok(());
    }

    let mut start = 0;
    let mut as_literal = can_send_as_literal(data[0]);

    for i in 1..data.len() {
        let current_literal = can_send_as_literal(data[i]);
        if as_literal != current_literal {
            tmuxcc_send_keys(writer, pane_id, &data[start..i], as_literal)?;
            start = i;
            as_literal = current_literal;
            continue;
        }
        let current_len = i - start;
        if (as_literal && current_len >= MAX_LITERAL) || (!as_literal && current_len >= MAX_HEX) {
            tmuxcc_send_keys(writer, pane_id, &data[start..i], as_literal)?;
            start = i;
        }
    }
    if start < data.len() {
        tmuxcc_send_keys(writer, pane_id, &data[start..], as_literal)?;
    }
    Ok(())
}

/// Decode tmux CC output by processing octal escapes.
/// Matches Go's tmuxccAddBuffer.
pub fn tmuxcc_decode_output(data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let c = data[i];
        if c < b' ' {
            i += 1;
            continue;
        }
        if c == b'\\' && i + 3 < data.len() {
            let a = data[i + 1];
            let b = data[i + 2];
            let c = data[i + 3];
            if (b'0'..=b'7').contains(&a) && (b'0'..=b'7').contains(&b) && (b'0'..=b'7').contains(&c) {
                buf.push((a - b'0') * 64 + (b - b'0') * 8 + (c - b'0'));
                i += 4;
                continue;
            }
        }
        buf.push(c);
        i += 1;
    }
    buf
}

/// Parse tmux `send` commands from client input.
/// Returns (decoded bytes, pane_id).
/// Matches Go's decodeTmuxInput.
pub fn decode_tmux_input(input: &[u8]) -> (Vec<u8>, String) {
    let mut buf = Vec::new();
    let mut pane_id = String::new();

    let parts: Vec<&[u8]> = input.split(|&b| b == b';' || b == b'\r').collect();
    for part in &parts {
        let cmd = part.to_vec();
        let cmd_str = String::from_utf8_lossy(&cmd);
        let cmd_str = cmd_str.trim();

        if cmd_str.len() < 13 || !cmd_str.starts_with("send ") {
            continue;
        }

        let rest = cmd_str.strip_prefix("send ").unwrap();
        if let Some(after) = rest.strip_prefix("-lt %") {
            if let Some(pos) = after.find(' ') {
                if pane_id.is_empty() {
                    pane_id = format!("%{}", &after[..pos]);
                }
                let data = &after[pos+1..];
                buf.extend_from_slice(data.as_bytes());
            }
        } else if let Some(after) = rest.strip_prefix("-t %") {
            if let Some(pos) = after.find(' ') {
                if pane_id.is_empty() {
                    pane_id = format!("%{}", &after[..pos]);
                }
                let hex_part = &rest[5+pos..];
                for hex in hex_part.split_whitespace() {
                    if hex.starts_with("0x") || hex.starts_with("0X") {
                        if let Ok(val) = u8::from_str_radix(&hex[2..], 16) {
                            buf.push(val);
                        }
                    }
                }
            }
        }
    }
    (buf, pane_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_hex() {
        assert_eq!(encode_hex(b"hello"), "68656c6c6f");
        assert_eq!(encode_hex(&[0x00, 0x1b, 0xff]), "001bff");
    }

    #[test]
    fn test_parse_output() {
        let line = "%output %1 aGVsbG8=";
        let (pane_id, data) = parse_output(line).unwrap();
        assert_eq!(pane_id, "%1");
        let decoded = parse_output_data(&data);
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn test_match_ack() {
        assert_eq!(match_tmux_ack("%end 3", 3), Some(true));
        assert_eq!(match_tmux_ack("%error 3: command failed", 3), Some(false));
        assert_eq!(match_tmux_ack("%output %1 abc", 3), None);
    }

    #[test]
    fn test_encode_tmux_output() {
        let result = encode_tmux_output("%output %1 ", b"hello");
        assert_eq!(result, b"%output %1 hello\r\n");
    }

    #[test]
    fn test_encode_tmux_output_octal() {
        let result = encode_tmux_output("%output %1 ", b"\x1b");
        assert_eq!(result, b"%output %1 \\033\r\n");
    }

    #[test]
    fn test_can_send_as_literal() {
        assert!(can_send_as_literal(b'a'));
        assert!(can_send_as_literal(b'0'));
        assert!(!can_send_as_literal(b'\n'));
        assert!(!can_send_as_literal(b'"'));
        assert!(!can_send_as_literal(b'\\'));
    }

    #[test]
    fn test_tmuxcc_decode_output() {
        let data = b"hello\\033world";
        let decoded = tmuxcc_decode_output(data);
        assert_eq!(decoded, b"hello\x1bworld");
    }

    #[test]
    fn test_decode_tmux_input() {
        let input = b"send -lt %1 hello";
        let (data, pane_id) = decode_tmux_input(input);
        assert_eq!(pane_id, "%1");
        assert_eq!(data, b"hello");
    }
}
