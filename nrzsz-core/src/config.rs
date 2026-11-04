use std::path::PathBuf;
use std::fs;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NrzszConfig {
    #[serde(default = "default_upload_path")]
    pub default_upload_path: String,

    #[serde(default = "default_download_path")]
    pub default_download_path: String,

    #[serde(default = "default_drag_upload_command")]
    pub drag_file_upload_command: String,

    #[serde(default = "default_progress_colors")]
    pub progress_color_pair: (String, String),

    #[serde(default)]
    pub allowed_auto_upload_dirs: Vec<String>,

    #[serde(default)]
    pub zmodem_auto_rename: bool,
}

fn default_upload_path() -> String {
    String::new()
}

fn default_download_path() -> String {
    String::new()
}

fn default_drag_upload_command() -> String {
    "nrz".to_string()
}

fn default_progress_colors() -> (String, String) {
    ("cyan".to_string(), "blue".to_string())
}

impl Default for NrzszConfig {
    fn default() -> Self {
        Self {
            default_upload_path: default_upload_path(),
            default_download_path: default_download_path(),
            drag_file_upload_command: default_drag_upload_command(),
            progress_color_pair: default_progress_colors(),
            allowed_auto_upload_dirs: Vec::new(),
            zmodem_auto_rename: false,
        }
    }
}

impl NrzszConfig {
    pub fn load() -> Result<Self> {
        // Try Go-compatible ~/.trzsz.conf first (matches trzsz-go).
        if let Ok(config) = Self::load_trzsz_conf() {
            return Ok(config);
        }
        // Try to load nrzsz.conf
        if let Ok(config) = Self::load_nrzsz_conf() {
            return Ok(config);
        }
        // Fall back to legacy config for compatibility
        if let Ok(config) = Self::load_legacy_conf() {
            return Ok(config);
        }
        Ok(Self::default())
    }

    /// Read the Go-compatible ~/.trzsz.conf (key=value, `#` comments, lowercase keys).
    fn load_trzsz_conf() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("Cannot find home directory"))?;
        let path = home.join(".trzsz.conf");
        if !path.exists() {
            return Err(anyhow!("Config file not found"));
        }
        let content = fs::read_to_string(&path)?;
        let mut config = Self::default();
        for line in content.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim().to_lowercase();
                let value = value.trim().to_string();
                if key.is_empty() || value.is_empty() {
                    continue;
                }
                match key.as_str() {
                    "allowedautouploaddirs" if config.allowed_auto_upload_dirs.is_empty() => {
                        config.allowed_auto_upload_dirs = value
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                    }
                    "defaultuploadpath" if config.default_upload_path.is_empty() => {
                        config.default_upload_path = value;
                    }
                    "defaultdownloadpath" if config.default_download_path.is_empty() => {
                        config.default_download_path = value;
                    }
                    "dragfileuploadcommand" if config.drag_file_upload_command.is_empty() => {
                        config.drag_file_upload_command = value;
                    }
                    "progresscolorpair" if config.progress_color_pair.0.is_empty() => {
                        let colors: Vec<&str> = value.split_whitespace().collect();
                        if colors.len() >= 2 {
                            config.progress_color_pair = (
                                colors[0].to_string(),
                                colors[1].to_string(),
                            );
                        }
                    }
                    "zmodemautorename" => {
                        match value.to_lowercase().as_str() {
                            "on" | "true" | "yes" | "1" => config.zmodem_auto_rename = true,
                            "off" | "false" | "no" | "0" => config.zmodem_auto_rename = false,
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(config)
    }
    
    fn load_nrzsz_conf() -> Result<Self> {
        let config_path = Self::nrzsz_config_path()?;
        
        if !config_path.exists() {
            return Err(anyhow!("Config file not found"));
        }
        
        let content = fs::read_to_string(&config_path)?;
        let config: Self = serde_json::from_str(&content)?;
        Ok(config)
    }
    
    fn load_legacy_conf() -> Result<Self> {
        let config_path = Self::legacy_config_path()?;
        
        if !config_path.exists() {
            return Err(anyhow!("Config file not found"));
        }
        
        let content = fs::read_to_string(&config_path)?;
        Self::parse_legacy_conf(&content)
    }
    
    fn parse_legacy_conf(content: &str) -> Result<Self> {
        let mut config = Self::default();
        
        for line in content.lines() {
            let line = line.trim();
            
            // Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            
            // Parse key=value pairs
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim();
                
                match key {
                    "DefaultUploadPath" => config.default_upload_path = value.to_string(),
                    "DefaultDownloadPath" => config.default_download_path = value.to_string(),
                    "DragFileUploadCommand" => config.drag_file_upload_command = value.to_string(),
                    "ProgressColorPair" => {
                        // Parse hex colors like "B14FFF 00FFA3"
                        let colors: Vec<&str> = value.split_whitespace().collect();
                        if colors.len() >= 2 {
                            config.progress_color_pair = (
                                colors[0].to_string(),
                                colors[1].to_string(),
                            );
                        }
                    }
                    _ => {
                        // Unknown key, skip
                    }
                }
            }
        }
        
        Ok(config)
    }
    
    pub fn save(&self) -> Result<()> {
        let config_path = Self::nrzsz_config_path()?;
        
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        
        let content = serde_json::to_string_pretty(self)?;
        fs::write(&config_path, content)?;
        Ok(())
    }
    
    fn nrzsz_config_path() -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("Cannot find home directory"))?;
        Ok(home.join(".nrzsz.conf"))
    }
    
    fn legacy_config_path() -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("Cannot find home directory"))?;
        Ok(home.join(".nrzsz_legacy.conf"))
    }
    
    pub fn get_upload_path(&self) -> &str {
        &self.default_upload_path
    }
    
    pub fn get_download_path(&self) -> &str {
        &self.default_download_path
    }
    
    pub fn get_drag_upload_command(&self) -> &str {
        &self.drag_file_upload_command
    }
    
    pub fn get_progress_colors(&self) -> (&str, &str) {
        (&self.progress_color_pair.0, &self.progress_color_pair.1)
    }

    pub fn get_allowed_auto_upload_dirs(&self) -> &[String] {
        &self.allowed_auto_upload_dirs
    }

    pub fn get_zmodem_auto_rename(&self) -> bool {
        self.zmodem_auto_rename
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_config_default() {
        let config = NrzszConfig::default();
        assert!(config.default_upload_path.is_empty());
        assert!(config.default_download_path.is_empty());
        assert_eq!(config.drag_file_upload_command, "nrz");
    }
    
    #[test]
    fn test_parse_legacy_conf() {
        let content = r#"
# This is a comment
DefaultUploadPath=/tmp/uploads
DefaultDownloadPath=/tmp/downloads
DragFileUploadCommand=nrz
ProgressColorPair=B14FFF 00FFA3
"#;
        
        let config = NrzszConfig::parse_legacy_conf(content).unwrap();
        assert_eq!(config.default_upload_path, "/tmp/uploads");
        assert_eq!(config.default_download_path, "/tmp/downloads");
        assert_eq!(config.drag_file_upload_command, "nrz");
        assert_eq!(config.progress_color_pair.0, "B14FFF");
        assert_eq!(config.progress_color_pair.1, "00FFA3");
    }
    
    #[test]
    fn test_config_serialization() {
        let config = NrzszConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: NrzszConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.default_upload_path, deserialized.default_upload_path);
    }
}