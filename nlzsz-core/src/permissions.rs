use std::path::Path;
use std::fs::{self, Metadata};
use anyhow::Result;
use log::debug;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[derive(Debug, Clone)]
pub struct FilePermissions {
    pub mode: u32,
    pub is_executable: bool,
    pub is_readable: bool,
    pub is_writable: bool,
}

impl FilePermissions {
    pub fn from_metadata(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        {
            let mode = metadata.permissions().mode();
            Self {
                mode,
                is_executable: mode & 0o111 != 0,
                is_readable: mode & 0o444 != 0,
                is_writable: mode & 0o222 != 0,
            }
        }
        
        #[cfg(windows)]
        {
            let readonly = metadata.permissions().readonly();
            Self {
                mode: if readonly { 0o444 } else { 0o666 },
                is_executable: false,
                is_readable: true,
                is_writable: !readonly,
            }
        }
        
        #[cfg(not(any(unix, windows)))]
        {
            Self {
                mode: 0o644,
                is_executable: false,
                is_readable: true,
                is_writable: true,
            }
        }
    }
    
    pub fn apply_to_path(&self, path: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            let permissions = fs::Permissions::from_mode(self.mode);
            fs::set_permissions(path, permissions)?;
            Ok(())
        }
        
        #[cfg(windows)]
        {
            let metadata = fs::metadata(path)?;
            let mut permissions = metadata.permissions();
            permissions.set_readonly(self.mode & 0o222 == 0);
            fs::set_permissions(path, permissions)?;
            Ok(())
        }
        
        #[cfg(not(any(unix, windows)))]
        {
            Ok(())
        }
    }
    
    pub fn set_executable(&mut self, executable: bool) {
        #[cfg(unix)]
        {
            if executable {
                self.mode |= 0o111;
            } else {
                self.mode &= !0o111;
            }
            self.is_executable = executable;
        }
    }
    
    pub fn set_readable(&mut self, readable: bool) {
        #[cfg(unix)]
        {
            if readable {
                self.mode |= 0o444;
            } else {
                self.mode &= !0o444;
            }
            self.is_readable = readable;
        }
    }
    
    pub fn set_writable(&mut self, writable: bool) {
        #[cfg(unix)]
        {
            if writable {
                self.mode |= 0o222;
            } else {
                self.mode &= !0o222;
            }
            self.is_writable = writable;
        }
    }
    
    pub fn from_string(mode_str: &str) -> Result<Self> {
        let mode = u32::from_str_radix(mode_str, 8)?;
        Ok(Self {
            mode,
            is_executable: mode & 0o111 != 0,
            is_readable: mode & 0o444 != 0,
            is_writable: mode & 0o222 != 0,
        })
    }
}

impl std::fmt::Display for FilePermissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:o}", self.mode)
    }
}

pub fn preserve_file_permissions(source: &Path, dest: &Path) -> Result<()> {
    let metadata = fs::metadata(source)?;
    let permissions = FilePermissions::from_metadata(&metadata);
    permissions.apply_to_path(dest)?;
    
    debug!("Preserved permissions for {:?}: {}", dest, permissions.to_string());
    Ok(())
}

pub fn get_file_permissions(path: &Path) -> Result<FilePermissions> {
    let metadata = fs::metadata(path)?;
    Ok(FilePermissions::from_metadata(&metadata))
}

pub fn set_file_permissions(path: &Path, permissions: &FilePermissions) -> Result<()> {
    permissions.apply_to_path(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_file_permissions_creation() {
        let perms = FilePermissions {
            mode: 0o755,
            is_executable: true,
            is_readable: true,
            is_writable: false,
        };
        
        assert_eq!(perms.mode, 0o755);
        assert!(perms.is_executable);
        assert!(perms.is_readable);
        assert!(!perms.is_writable);
    }
    
    #[test]
    fn test_file_permissions_from_string() {
        let perms = FilePermissions::from_string("755").unwrap();
        assert_eq!(perms.mode, 0o755);
        assert!(perms.is_executable);
    }
    
    #[test]
    fn test_file_permissions_to_string() {
        let perms = FilePermissions {
            mode: 0o644,
            is_executable: false,
            is_readable: true,
            is_writable: true,
        };
        assert_eq!(perms.to_string(), "644");
    }
}
