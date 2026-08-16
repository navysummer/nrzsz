use std::path::{Path, PathBuf};
use anyhow::Result;
use log::debug;

pub struct DragDropSupport {
    is_enabled: bool,
    detected_files: Vec<PathBuf>,
}

impl Default for DragDropSupport {
    fn default() -> Self {
        Self::new()
    }
}

impl DragDropSupport {
    pub fn new() -> Self {
        Self {
            is_enabled: false,
            detected_files: Vec::new(),
        }
    }
    
    pub fn enable(&mut self) {
        self.is_enabled = true;
        debug!("Drag-drop detection enabled");
    }
    
    pub fn disable(&mut self) {
        self.is_enabled = false;
        debug!("Drag-drop detection disabled");
    }
    
    pub fn is_enabled(&self) -> bool {
        self.is_enabled
    }
    
    pub fn detect_dragged_files(&mut self, input: &str) -> Vec<PathBuf> {
        if !self.is_enabled {
            return Vec::new();
        }
        
        let mut files = Vec::new();
        
        // Split input by newlines and check each line
        for line in input.lines() {
            let line = line.trim();
            
            // Skip empty lines
            if line.is_empty() {
                continue;
            }
            
            // Check if line is a file path
            let path = PathBuf::from(line);
            if path.exists() {
                files.push(path);
                debug!("Detected file: {}", line);
            }
        }
        
        self.detected_files = files.clone();
        files
    }
    
    pub fn detect_file_paths_from_terminal(&mut self, input: &str) -> Vec<PathBuf> {
        if !self.is_enabled {
            return Vec::new();
        }
        
        let mut files = Vec::new();
        
        // Look for file paths in terminal input
        // This handles cases where files are dragged into terminal
        for word in input.split_whitespace() {
            let word = word.trim();
            
            // Skip empty words
            if word.is_empty() {
                continue;
            }
            
            // Remove quotes if present
            let word = if (word.starts_with('"') && word.ends_with('"'))
                || (word.starts_with('\'') && word.ends_with('\''))
            {
                &word[1..word.len() - 1]
            } else {
                word
            };
            
            // Check if word is a file path
            let path = PathBuf::from(word);
            if path.exists() {
                files.push(path);
                debug!("Detected file path: {}", word);
            }
        }
        
        self.detected_files = files.clone();
        files
    }
    
    pub fn get_detected_files(&self) -> &[PathBuf] {
        &self.detected_files
    }
    
    pub fn clear_detected_files(&mut self) {
        self.detected_files.clear();
    }
    
    pub fn process_dropped_files(&self, files: &[PathBuf]) -> Result<Vec<PathBuf>> {
        let mut processed = Vec::new();
        
        for file in files {
            if file.exists() && file.is_file() {
                processed.push(file.clone());
            }
        }
        
        Ok(processed)
    }
    
    pub fn setup_watch_dir(&self) -> Result<PathBuf> {
        let watch_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".nlzsz")
            .join("uploads");
        
        std::fs::create_dir_all(&watch_dir)?;
        debug!("Watch directory created: {:?}", watch_dir);
        Ok(watch_dir)
    }
    
    pub fn watch_for_files(&self, watch_dir: &Path) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        
        if watch_dir.exists() {
            for entry in std::fs::read_dir(watch_dir)? {
                let entry = entry?;
                let path = entry.path();
                
                if path.is_file() {
                    files.push(path);
                }
            }
        }
        
        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_drag_drop_creation() {
        let support = DragDropSupport::new();
        assert!(!support.is_enabled());
    }
    
    #[test]
    fn test_enable_disable() {
        let mut support = DragDropSupport::new();
        support.enable();
        assert!(support.is_enabled());
        
        support.disable();
        assert!(!support.is_enabled());
    }
    
    #[test]
    fn test_detect_file_paths() {
        let mut support = DragDropSupport::new();
        support.enable();
        
        // Test with non-existent files
        let files = support.detect_dragged_files("/nonexistent/file.txt");
        assert!(files.is_empty());
    }
}