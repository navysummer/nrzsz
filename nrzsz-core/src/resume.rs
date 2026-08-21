use std::path::PathBuf;
use std::fs;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeState {
    pub file_path: String,
    pub file_size: u64,
    pub transferred_bytes: u64,
    pub file_md5: String,
    pub timestamp: u64,
}

impl ResumeState {
    pub fn new(file_path: &str, file_size: u64) -> Self {
        Self {
            file_path: file_path.to_string(),
            file_size,
            transferred_bytes: 0,
            file_md5: String::new(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }
    
    pub fn save(&self) -> Result<PathBuf> {
        let state_path = self.state_path()?;
        
        if let Some(parent) = state_path.parent() {
            fs::create_dir_all(parent)?;
        }
        
        let content = serde_json::to_string_pretty(self)?;
        fs::write(&state_path, content)?;
        Ok(state_path)
    }
    
    pub fn load(file_path: &str) -> Result<Option<Self>> {
        let state_path = Self::state_path_for_file(file_path)?;
        
        if !state_path.exists() {
            return Ok(None);
        }
        
        let content = fs::read_to_string(&state_path)?;
        let state: Self = serde_json::from_str(&content)?;
        Ok(Some(state))
    }
    
    pub fn delete(&self) -> Result<()> {
        let state_path = self.state_path()?;
        if state_path.exists() {
            fs::remove_file(&state_path)?;
        }
        Ok(())
    }
    
    fn state_path(&self) -> Result<PathBuf> {
        Self::state_path_for_file(&self.file_path)
    }
    
    fn state_path_for_file(file_path: &str) -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("Cannot find home directory"))?;
        let state_dir = home.join(".nrzsz").join("resume");
        fs::create_dir_all(&state_dir)?;
        
        let file_name = format!("{}.resume", file_path.replace(['/', '\\'], "_"));
        Ok(state_dir.join(file_name))
    }
    
    pub fn update_progress(&mut self, bytes: u64) {
        self.transferred_bytes = bytes;
    }
    
    pub fn is_complete(&self) -> bool {
        self.transferred_bytes >= self.file_size
    }
    
    pub fn can_resume(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        
        // Resume state expires after 24 hours
        now - self.timestamp < 86400
    }
}

pub fn cleanup_old_states() -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Cannot find home directory"))?;
    let state_dir = home.join(".nrzsz").join("resume");
    
    if !state_dir.exists() {
        return Ok(());
    }
    
    for entry in fs::read_dir(&state_dir)? {
        let entry = entry?;
        let path = entry.path();
        
        if path.extension().is_some_and(|ext| ext == "resume") {
            if let Ok(metadata) = fs::metadata(&path) {
                if let Ok(modified) = metadata.modified() {
                    if let Ok(elapsed) = modified.elapsed() {
                        if elapsed.as_secs() > 86400 {
                            let _ = fs::remove_file(&path);
                        }
                    }
                }
            }
        }
    }
    
    Ok(())
}
