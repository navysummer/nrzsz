use std::path::PathBuf;
use std::fs;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NlzszConfig {
    #[serde(default = "default_upload_path")]
    pub default_upload_path: String,
    
    #[serde(default = "default_download_path")]
    pub default_download_path: String,
    
    #[serde(default = "default_drag_upload_command")]
    pub drag_file_upload_command: String,
    
    #[serde(default = "default_progress_colors")]
    pub progress_color_pair: (String, String),
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

impl Default for NlzszConfig {
    fn default() -> Self {
        Self {
            default_upload_path: default_upload_path(),
            default_download_path: default_download_path(),
            drag_file_upload_command: default_drag_upload_command(),
            progress_color_pair: default_progress_colors(),
        }
    }
}

impl NlzszConfig {
    pub fn load() -> Result<Self> {
        // Try to load nlzsz.conf first
        if let Ok(config) = Self::load_nlzsz_conf() {
            return Ok(config);
        }
        
        // Fall back to legacy config for compatibility
        if let Ok(config) = Self::load_legacy_conf() {
            return Ok(config);
        }
        
        Ok(Self::default())
    }
    
    fn load_nlzsz_conf() -> Result<Self> {
        let config_path = Self::nlzsz_config_path()?;
        
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
        let config_path = Self::nlzsz_config_path()?;
        
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        
        let content = serde_json::to_string_pretty(self)?;
        fs::write(&config_path, content)?;
        Ok(())
    }
    
    fn nlzsz_config_path() -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("Cannot find home directory"))?;
        Ok(home.join(".nlzsz.conf"))
    }
    
    fn legacy_config_path() -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("Cannot find home directory"))?;
        Ok(home.join(".nlzsz_legacy.conf"))
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
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_config_default() {
        let config = NlzszConfig::default();
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
        
        let config = NlzszConfig::parse_legacy_conf(content).unwrap();
        assert_eq!(config.default_upload_path, "/tmp/uploads");
        assert_eq!(config.default_download_path, "/tmp/downloads");
        assert_eq!(config.drag_file_upload_command, "nrz");
        assert_eq!(config.progress_color_pair.0, "B14FFF");
        assert_eq!(config.progress_color_pair.1, "00FFA3");
    }
    
    #[test]
    fn test_config_serialization() {
        let config = NlzszConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: NlzszConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.default_upload_path, deserialized.default_upload_path);
    }
}