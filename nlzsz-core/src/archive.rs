use std::path::Path;
use std::fs::{self, File};
use std::io::{Write, Read};
use anyhow::{Result, anyhow};
use log::debug;

pub struct ArchiveManager {
    buffer_size: usize,
    preserve_permissions: bool,
}

impl Default for ArchiveManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ArchiveManager {
    pub fn new() -> Self {
        Self {
            buffer_size: 64 * 1024,
            preserve_permissions: true,
        }
    }
    
    pub fn create_archive(&self, paths: &[&Path]) -> Result<Vec<u8>> {
        let mut archive_data = Vec::new();
        
        // Write archive header
        archive_data.extend_from_slice(b"NLZSZ");
        archive_data.push(0); // version
        
        for path in paths {
            if path.is_dir() {
                self.add_directory_to_archive(path, &mut archive_data)?;
            } else if path.is_file() {
                self.add_file_to_archive(path, &mut archive_data)?;
            }
        }
        
        Ok(archive_data)
    }
    
    fn add_directory_to_archive(&self, dir: &Path, archive: &mut Vec<u8>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            
            if path.is_dir() {
                self.add_directory_to_archive(&path, archive)?;
            } else {
                self.add_file_to_archive(&path, archive)?;
            }
        }
        
        Ok(())
    }
    
    fn add_file_to_archive(&self, path: &Path, archive: &mut Vec<u8>) -> Result<()> {
        let metadata = fs::metadata(path)?;
        let filename = path.to_string_lossy();
        
        // Write filename length and filename
        let name_bytes = filename.as_bytes();
        archive.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
        archive.extend_from_slice(name_bytes);
        
        // Write file size
        archive.extend_from_slice(&metadata.len().to_le_bytes());
        
        // Write permissions if enabled
        if self.preserve_permissions {
            let perms = crate::permissions::FilePermissions::from_metadata(&metadata);
            archive.extend_from_slice(&perms.mode.to_le_bytes());
        }
        
        // Write file contents
        let mut file = File::open(path)?;
        let mut buffer = vec![0u8; self.buffer_size];
        
        loop {
            let bytes_read = file.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            archive.extend_from_slice(&buffer[..bytes_read]);
        }
        
        debug!("Added file to archive: {}", path.display());
        Ok(())
    }
    
    pub fn extract_archive(&self, archive_data: &[u8], output_dir: &Path) -> Result<()> {
        // Verify archive header
        if archive_data.len() < 6 || &archive_data[..5] != b"NLZSZ" {
            return Err(anyhow!("Invalid archive header"));
        }
        let mut pos = 6; // Skip header (5 bytes "NLZSZ" + 1 byte version)
        
        while pos < archive_data.len() {
            // Read filename length
            if pos + 4 > archive_data.len() {
                break;
            }
            let name_len = u32::from_le_bytes(archive_data[pos..pos + 4].try_into()?) as usize;
            pos += 4;
            
            // Read filename
            if pos + name_len > archive_data.len() {
                break;
            }
            let filename = String::from_utf8(archive_data[pos..pos + name_len].to_vec())?;
            pos += name_len;
            
            // Read file size
            if pos + 8 > archive_data.len() {
                break;
            }
            let file_size = u64::from_le_bytes(archive_data[pos..pos + 8].try_into()?) as usize;
            pos += 8;
            
            // Read permissions if available
            let mode = if self.preserve_permissions && pos + 4 <= archive_data.len() {
                let mode = u32::from_le_bytes(archive_data[pos..pos + 4].try_into()?);
                pos += 4;
                Some(mode)
            } else {
                None
            };
            
            // Read file contents
            if pos + file_size > archive_data.len() {
                break;
            }
            
            let file_path = output_dir.join(&filename);
            
            // Create parent directories if needed
            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent)?;
            }
            
            let mut file = File::create(&file_path)?;
            file.write_all(&archive_data[pos..pos + file_size])?;
            pos += file_size;
            
            // Apply permissions if available
            if let Some(mode) = mode {
                let perms = crate::permissions::FilePermissions {
                    mode,
                    is_executable: mode & 0o111 != 0,
                    is_readable: mode & 0o444 != 0,
                    is_writable: mode & 0o222 != 0,
                };
                let _ = perms.apply_to_path(&file_path);
            }
            
            debug!("Extracted file: {}", file_path.display());
        }
        
        Ok(())
    }
    
    pub fn set_buffer_size(&mut self, size: usize) {
        self.buffer_size = size;
    }
    
    pub fn set_preserve_permissions(&mut self, preserve: bool) {
        self.preserve_permissions = preserve;
    }
    
    pub fn get_archive_info(&self, archive_data: &[u8]) -> Result<ArchiveInfo> {
        let mut file_count = 0;
        let mut total_size = 0;
        
        // Verify archive header
        if archive_data.len() < 6 || &archive_data[..5] != b"NLZSZ" {
            return Err(anyhow!("Invalid archive header"));
        }
        let mut pos = 6;
        
        while pos < archive_data.len() {
            if pos + 4 > archive_data.len() {
                break;
            }
            let name_len = u32::from_le_bytes(archive_data[pos..pos + 4].try_into()?) as usize;
            pos += 4;
            
            if pos + name_len > archive_data.len() {
                break;
            }
            pos += name_len;
            
            if pos + 8 > archive_data.len() {
                break;
            }
            let file_size = u64::from_le_bytes(archive_data[pos..pos + 8].try_into()?) as usize;
            pos += 8;
            
            if self.preserve_permissions && pos + 4 <= archive_data.len() {
                pos += 4;
            }
            
            if pos + file_size > archive_data.len() {
                break;
            }
            pos += file_size;
            
            file_count += 1;
            total_size += file_size;
        }
        
        Ok(ArchiveInfo {
            file_count,
            total_size,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ArchiveInfo {
    pub file_count: usize,
    pub total_size: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_archive_manager_creation() {
        let manager = ArchiveManager::new();
        assert_eq!(manager.buffer_size, 64 * 1024);
        assert!(manager.preserve_permissions);
    }
    
    #[test]
    fn test_archive_info() {
        let info = ArchiveInfo {
            file_count: 5,
            total_size: 1024,
        };
        assert_eq!(info.file_count, 5);
        assert_eq!(info.total_size, 1024);
    }
}