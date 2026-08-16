use std::env;
use std::process::Command;
use anyhow::{Result, anyhow};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TmuxMode {
    NoTmux = 0,
    NormalMode = 1,
    ControlMode = 2,
}

pub struct TmuxInfo {
    pub mode: TmuxMode,
    pub pane_width: u32,
    pub tmux_tty: Option<String>,
}

pub fn detect_tmux() -> Result<Option<TmuxInfo>> {
    if env::var("TMUX").is_err() {
        return Ok(None);
    }
    
    let output = Command::new("tmux")
        .args(["display-message", "-p", "#{client_tty}:#{client_control_mode}:#{pane_width}"])
        .output()?;
    
    if !output.status.success() {
        return Err(anyhow!("Failed to get tmux info"));
    }
    
    let stdout = String::from_utf8(output.stdout)?;
    let parts: Vec<&str> = stdout.trim().split(':').collect();
    
    if parts.len() != 3 {
        return Err(anyhow!("Unexpected tmux output: {}", stdout));
    }
    
    let tmux_tty = parts[0].to_string();
    let control_mode = parts[1];
    let pane_width = parts[2].parse::<u32>().unwrap_or(0);
    
    let mode = if control_mode == "1" || !tmux_tty.starts_with('/') || !std::path::Path::new(&tmux_tty).exists() {
        TmuxMode::ControlMode
    } else {
        TmuxMode::NormalMode
    };
    
    Ok(Some(TmuxInfo {
        mode,
        pane_width,
        tmux_tty: Some(tmux_tty),
    }))
}

pub fn refresh_tmux_client() -> Result<()> {
    Command::new("tmux")
        .arg("refresh-client")
        .output()?;
    Ok(())
}

pub fn get_tmux_status_interval() -> Result<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-p", "#{status-interval}"])
        .output()?;
    
    let stdout = String::from_utf8(output.stdout)?;
    let interval = stdout.trim().to_string();
    
    Ok(if interval.is_empty() {
        "15".to_string()
    } else {
        interval
    })
}

pub fn set_tmux_status_interval(interval: &str) -> Result<()> {
    let interval = if interval.is_empty() {
        "15"
    } else {
        interval
    };
    
    Command::new("tmux")
        .args(["setw", "status-interval", interval])
        .output()?;
    
    Ok(())
}

pub fn strip_tmux_status_line(buf: &str) -> String {
    let mut result = buf.to_string();
    
    loop {
        let begin_idx = match result.find("\x1bP=") {
            Some(idx) => idx,
            None => return result,
        };
        
        let mid_idx = match result[begin_idx + 3..].find("\x1bP=") {
            Some(idx) => begin_idx + 3 + idx,
            None => return result[..begin_idx].to_string(),
        };
        
        let end_idx = match result[mid_idx + 3..].find("\x1b\\") {
            Some(idx) => mid_idx + 3 + idx,
            None => return result[..begin_idx].to_string(),
        };
        
        result = format!("{}{}", &result[..begin_idx], &result[end_idx + 2..]);
    }
}

pub fn send_tmux_cc_message(message: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args(["display-message", "-d", "0", message])
        .output()?;
    
    if !output.status.success() {
        return Err(anyhow!("Failed to send tmux CC message"));
    }
    
    Ok(())
}

pub fn get_tmux_pane_id() -> Result<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-p", "#{pane_id}"])
        .output()?;
    
    let stdout = String::from_utf8(output.stdout)?;
    Ok(stdout.trim().to_string())
}

pub fn set_tmux_pane_name(name: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args(["rename-window", name])
        .output()?;
    
    if !output.status.success() {
        // Try pane name instead
        Command::new("tmux")
            .args(["select-pane", "-t", name])
            .output()?;
    }
    
    Ok(())
}

pub fn set_tmux_status_line(line: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args(["set-option", "status-left", line])
        .output()?;
    
    if !output.status.success() {
        return Err(anyhow!("Failed to set tmux status line"));
    }
    
    Ok(())
}

pub fn get_tmux_option(option: &str) -> Result<String> {
    let output = Command::new("tmux")
        .args(["show-option", "-pv", option])
        .output()?;
    
    let stdout = String::from_utf8(output.stdout)?;
    Ok(stdout.trim().to_string())
}

pub fn set_tmux_option(option: &str, value: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args(["set-option", "-q", option, value])
        .output()?;
    
    if !output.status.success() {
        return Err(anyhow!("Failed to set tmux option"));
    }
    
    Ok(())
}