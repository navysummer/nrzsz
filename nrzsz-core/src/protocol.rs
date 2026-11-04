use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub const PROTOCOL_VERSION: u32 = 4;

// Protocol version constants for backwards compatibility
pub const PROTOCOL_V2: u32 = 2; // nrzsz 1.1.0-1.1.3
pub const PROTOCOL_V3: u32 = 3; // nrzsz 1.1.4-1.1.9
pub const PROTOCOL_V4: u32 = 4; // nrzsz 1.2.0+

// Type alias for escape characters
pub type EscapeChars = Vec<(Vec<u8>, Vec<u8>)>;

// Custom serialization for escape_chars: Vec<(Vec<u8>, Vec<u8>)> -> JSON array of 2-element arrays of strings
// Uses Latin-1 (ISO-8859-1) encoding to match Go's `charmap.ISO8859_1`
mod escape_chars_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use super::EscapeChars;
    
    fn bytes_to_latin1(bytes: &[u8]) -> String {
        bytes.iter().map(|&b| b as char).collect()
    }
    
    fn latin1_to_bytes(s: &str) -> Vec<u8> {
        s.chars().map(|c| c as u8).collect()
    }
    
    pub fn serialize<S>(chars: &EscapeChars, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let pairs: Vec<(String, String)> = chars
            .iter()
            .map(|(k, v)| {
                let key = bytes_to_latin1(k);
                let val = bytes_to_latin1(v);
                (key, val)
            })
            .collect();
        pairs.serialize(serializer)
    }
    
    pub fn deserialize<'de, D>(deserializer: D) -> Result<EscapeChars, D::Error>
    where
        D: Deserializer<'de>,
    {
        let pairs: Vec<(String, String)> = Vec::deserialize(deserializer)?;
        Ok(pairs
            .into_iter()
            .map(|(k, v)| (latin1_to_bytes(&k), latin1_to_bytes(&v)))
            .collect())
    }
}

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

/// Semantic version comparison for nrzsz version strings
pub fn compare_versions(v1: &str, v2: &str) -> std::cmp::Ordering {
    let parse = |v: &str| -> (u32, u32, u32) {
        let parts: Vec<&str> = v.split('.').collect();
        let major = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        let minor = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let patch = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
        (major, minor, patch)
    };
    
    let (m1, mi1, p1) = parse(v1);
    let (m2, mi2, p2) = parse(v2);
    
    (m1, mi1, p1).cmp(&(m2, mi2, p2))
}

/// Check if a version string is in the nrzsz 1.1.0-1.1.3 range
/// that requires protocol downgrade to V2
pub fn needs_protocol_downgrade(version: &str) -> bool {
    !version.is_empty() &&
    version != "0" &&
    compare_versions(version, "1.1.3") <= std::cmp::Ordering::Equal &&
    compare_versions(version, "1.1.0") >= std::cmp::Ordering::Equal
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
            support_fork: action.fork.unwrap_or(false),
            support_directory: action.support_dir,
            support_tunnel: action.tunnel.unwrap_or(false),
            support_tmuxcc: action.tmuxcc.unwrap_or(false),
            support_compress: true,
        }
    }
    
    pub fn can_use_binary(&self, config: &Config) -> bool {
        self.support_binary && config.binary
    }
    
    pub fn can_use_fork(&self, config: &Config) -> bool {
        self.support_fork && config.fork.unwrap_or(false)
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
    pub newline: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmuxcc: Option<bool>,
    #[serde(rename = "binary")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub support_binary: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
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
    #[serde(default = "default_max_buf_size", rename = "bufsize")]
    pub max_buf_size: usize,
    #[serde(default, with = "escape_chars_serde")]
    pub escape_chars: EscapeChars,
    #[serde(default)]
    pub tmux_pane_width: u32,
    #[serde(default)]
    pub tmux_output_junk: bool,
    #[serde(default = "default_compress")]
    pub compress: CompressType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub client_files: Vec<String>,
    #[serde(default)]
    pub progress_color_pair: (String, String),
}

impl Config {
    /// Returns the progress color pair as an `Option`, or `None` when either
    /// half is empty (no gradient configured).
    pub fn color_pair_option(&self) -> Option<(String, String)> {
        if self.progress_color_pair.0.is_empty() || self.progress_color_pair.1.is_empty() {
            None
        } else {
            Some(self.progress_color_pair.clone())
        }
    }
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
            lang: "rust".to_string(),
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
            fork: None,
            client_files: Vec::new(),
            progress_color_pair: (String::new(), String::new()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub path_id: u32,
    #[serde(skip)]
    pub abs_path: String,
    pub path_name: Vec<String>,
    pub is_dir: bool,
    #[serde(default)]
    pub archive: bool,
    #[serde(default)]
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perm: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryInfo {
    pub path_id: u32,
    pub path_name: Vec<String>,
    pub is_dir: bool,
    #[serde(default)]
    pub archive: bool,
    #[serde(default)]
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perm: Option<u32>,
}

// V3+ resume support
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetFile {
    pub name: String,
    pub size: i64,
}

// V3+ prefix hash for resume support
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefixHash {
    pub step: i64,
    pub hash: String,
    pub over: bool,
}

// V3+ prefix hash acknowledgment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefixHashAck {
    pub step: i64,
    #[serde(rename = "match", default)]
    pub matched: bool,
}