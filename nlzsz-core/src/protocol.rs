use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub const PROTOCOL_VERSION: u32 = 4;

// Protocol version constants for backwards compatibility
pub const PROTOCOL_V2: u32 = 2; // trzsz 1.1.0-1.1.3
pub const PROTOCOL_V3: u32 = 3; // trzsz 1.1.4-1.1.9
pub const PROTOCOL_V4: u32 = 4; // trzsz 1.2.0+

pub fn is_protocol_supported(version: u32) -> bool {
    (PROTOCOL_V2..=PROTOCOL_V4).contains(&version)
}

pub fn generate_unique_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    
    // Generate unique ID
    let id = (now % 10_000_000_000) * 100;
    
    // Add suffix based on platform and environment
    let suffix = if cfg!(windows) {
        "10" // Windows
    } else if let Ok(Some(tmux_info)) = crate::tmux::detect_tmux() {
        if tmux_info.mode == crate::tmux::TmuxMode::NormalMode {
            "20" // tmux normal mode
        } else {
            "00" // tmux control mode or other
        }
    } else {
        "00" // Default (no tmux)
    };
    
    format!("{}{}", id, suffix)
}

pub fn is_repeated_id(id: &str, seen_ids: &mut Vec<String>) -> bool {
    if seen_ids.contains(&id.to_string()) {
        true
    } else {
        seen_ids.push(id.to_string());
        false
    }
}

pub fn negotiate_protocol_version(client_version: u32, server_version: u32) -> u32 {
    // Use the minimum supported version
    let min_version = client_version.min(server_version);
    
    // Ensure we're using a supported version
    if is_protocol_supported(min_version) {
        min_version
    } else {
        // Fallback to protocol V2 for maximum compatibility
        PROTOCOL_V2
    }
}

pub fn get_protocol_features(version: u32) -> ProtocolFeatures {
    ProtocolFeatures {
        support_binary: version >= PROTOCOL_V3,
        support_directory: version >= PROTOCOL_V3,
        support_compress: version >= PROTOCOL_V3,
        support_tmuxcc: version >= PROTOCOL_V3,
        support_tunnel: version >= PROTOCOL_V4,
        support_fork: version >= PROTOCOL_V4,
    }
}

#[derive(Debug, Clone)]
pub struct ProtocolFeatures {
    pub support_binary: bool,
    pub support_directory: bool,
    pub support_compress: bool,
    pub support_tmuxcc: bool,
    pub support_tunnel: bool,
    pub support_fork: bool,
}

pub fn is_windows_unique_id(id: &str) -> bool {
    id.ends_with("10")
}

pub fn is_tmux_normal_mode_id(id: &str) -> bool {
    id.ends_with("20")
}

#[derive(Debug, Clone)]
pub struct ClientCapabilities {
    pub support_binary: bool,
    pub support_fork: bool,
    pub support_directory: bool,
    pub support_tunnel: bool,
    pub support_tmuxcc: bool,
    pub support_compress: bool,
}

impl Default for ClientCapabilities {
    fn default() -> Self {
        Self {
            support_binary: true,
            support_fork: true,
            support_directory: true,
            support_tunnel: true,
            support_tmuxcc: true,
            support_compress: true,
        }
    }
}

impl ClientCapabilities {
    pub fn from_action(action: &Action) -> Self {
        Self {
            support_binary: action.support_binary.unwrap_or(true),
            support_fork: action.fork.unwrap_or(true),
            support_directory: action.support_dir,
            support_tunnel: action.tunnel.unwrap_or(true),
            support_tmuxcc: action.tmuxcc.unwrap_or(false),
            support_compress: action.comp.is_some(),
        }
    }
    
    pub fn can_use_binary(&self, config: &Config) -> bool {
        self.support_binary && config.binary
    }
    
    pub fn can_use_fork(&self, config: &Config) -> bool {
        self.support_fork && config.fork
    }
    
    pub fn can_use_directory(&self, config: &Config) -> bool {
        self.support_directory && config.directory
    }
    
    pub fn can_use_tunnel(&self, _config: &Config) -> bool {
        self.support_tunnel
    }
    
    pub fn can_use_tmuxcc(&self) -> bool {
        self.support_tmuxcc
    }
    
    pub fn can_use_compress(&self) -> bool {
        self.support_compress
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub lang: String,
    pub confirm: bool,
    pub version: String,
    pub support_dir: bool,
    pub protocol: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmuxcc: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub support_binary: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comp: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub lang: String,
    #[serde(default)]
    pub quiet: bool,
    #[serde(default)]
    pub binary: bool,
    #[serde(default)]
    pub directory: bool,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default = "default_timeout")]
    pub timeout: u32,
    #[serde(default = "default_newline")]
    pub newline: String,
    #[serde(default)]
    pub protocol: u32,
    #[serde(default = "default_max_buf_size", alias = "bufsize")]
    pub max_buf_size: usize,
    #[serde(default)]
    pub escape_chars: Vec<(Vec<u8>, Vec<u8>)>,
    #[serde(default)]
    pub tmux_pane_width: u32,
    #[serde(default)]
    pub tmux_output_junk: bool,
    #[serde(default = "default_compress")]
    pub compress: CompressType,
    #[serde(default)]
    pub fork: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum CompressType {
    #[default]
    Auto,
    Yes,
    No,
}

impl CompressType {
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "yes" | "true" | "1" => CompressType::Yes,
            "no" | "false" | "2" => CompressType::No,
            _ => CompressType::Auto,
        }
    }
    
    pub fn from_int(i: i32) -> Self {
        match i {
            1 => CompressType::Yes,
            2 => CompressType::No,
            _ => CompressType::Auto,
        }
    }
    
    pub fn to_int(&self) -> i32 {
        match self {
            CompressType::Auto => 0,
            CompressType::Yes => 1,
            CompressType::No => 2,
        }
    }
    
    pub fn to_str(&self) -> &str {
        match self {
            CompressType::Auto => "auto",
            CompressType::Yes => "yes",
            CompressType::No => "no",
        }
    }
}

impl serde::Serialize for CompressType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_i32(self.to_int())
    }
}

impl<'de> serde::Deserialize<'de> for CompressType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;
        
        struct CompressTypeVisitor;
        
        impl<'de> de::Visitor<'de> for CompressTypeVisitor {
            type Value = CompressType;
            
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an integer (0, 1, 2) or a string (auto, yes, no)")
            }
            
            fn visit_i32<E>(self, value: i32) -> Result<CompressType, E>
            where
                E: de::Error,
            {
                Ok(CompressType::from_int(value))
            }
            
            fn visit_i64<E>(self, value: i64) -> Result<CompressType, E>
            where
                E: de::Error,
            {
                Ok(CompressType::from_int(value as i32))
            }
            
            fn visit_u64<E>(self, value: u64) -> Result<CompressType, E>
            where
                E: de::Error,
            {
                Ok(CompressType::from_int(value as i32))
            }
            
            fn visit_str<E>(self, value: &str) -> Result<CompressType, E>
            where
                E: de::Error,
            {
                Ok(CompressType::parse(value))
            }
            
            fn visit_string<E>(self, value: String) -> Result<CompressType, E>
            where
                E: de::Error,
            {
                Ok(CompressType::parse(&value))
            }
        }
        
        deserializer.deserialize_any(CompressTypeVisitor)
    }
}

fn default_timeout() -> u32 {
    20
}

fn default_newline() -> String {
    "\n".to_string()
}

fn default_max_buf_size() -> usize {
    10 * 1024 * 1024 // 10MB
}

fn default_compress() -> CompressType {
    CompressType::Auto
}

impl Default for Config {
    fn default() -> Self {
        Self {
            lang: "rs".to_string(),
            quiet: false,
            binary: false,
            directory: false,
            overwrite: false,
            timeout: default_timeout(),
            newline: default_newline(),
            protocol: PROTOCOL_VERSION,
            max_buf_size: default_max_buf_size(),
            escape_chars: Vec::new(),
            tmux_pane_width: 0,
            tmux_output_junk: false,
            compress: default_compress(),
            fork: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub path_id: u32,
    pub abs_path: String,
    pub path_name: Vec<String>,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryInfo {
    pub path_id: u32,
    pub path_name: Vec<String>,
    pub is_dir: bool,
}