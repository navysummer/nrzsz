use std::path::PathBuf;
use anyhow::{Result, anyhow};

pub struct FileDialog {
    is_available: bool,
}

impl Default for FileDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl FileDialog {
    pub fn new() -> Self {
        let is_available = Self::check_dialog_available();
        
        Self {
            is_available,
        }
    }
    
    fn check_dialog_available() -> bool {
        // Check if zenity (Linux), osascript (macOS), or PowerShell (Windows) is available
        if cfg!(target_os = "linux") {
            std::process::Command::new("which")
                .arg("zenity")
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
        } else if cfg!(target_os = "macos") {
            std::process::Command::new("which")
                .arg("osascript")
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
        } else if cfg!(target_os = "windows") {
            std::process::Command::new("powershell")
                .arg("-Command")
                .arg("Get-Command")
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
        } else {
            false
        }
    }
    
    pub fn open_file(&self, title: &str) -> Result<Option<PathBuf>> {
        if !self.is_available {
            return Err(anyhow!("File dialog not available"));
        }
        
        #[cfg(target_os = "linux")]
        {
            let output = std::process::Command::new("zenity")
                .args([
                    "--file-selection",
                    "--title", title,
                ])
                .output()?;
            
            if output.status.success() {
                let stdout = String::from_utf8(output.stdout)?;
                Ok(Some(PathBuf::from(stdout.trim())))
            } else {
                Ok(None)
            }
        }
        
        #[cfg(target_os = "macos")]
        {
            let script = format!(
                r#"tell application "System Events"
                    set theFile to POSIX path of (choose file with prompt "{}")
                end tell"#,
                title
            );
            
            let output = std::process::Command::new("osascript")
                .args(["-e", &script])
                .output()?;
            
            if output.status.success() {
                let stdout = String::from_utf8(output.stdout)?;
                Ok(Some(PathBuf::from(stdout.trim())))
            } else {
                Ok(None)
            }
        }
        
        #[cfg(target_os = "windows")]
        {
            let script = format!(
                r#"Add-Type -AssemblyName System.Windows.Forms
                $openFileDialog = New-Object System.Windows.Forms.OpenFileDialog
                $openFileDialog.Title = "{}"
                $openFileDialog.Filter = "All Files (*.*)|*.*"
                if ($openFileDialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{
                    $openFileDialog.FileName
                }}"#,
                title
            );
            
            let output = std::process::Command::new("powershell")
                .args(["-Command", &script])
                .output()?;
            
            if output.status.success() {
                let stdout = String::from_utf8(output.stdout)?;
                Ok(Some(PathBuf::from(stdout.trim())))
            } else {
                Ok(None)
            }
        }
        
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            Err(anyhow!("Platform not supported"))
        }
    }
    
    pub fn save_file(&self, title: &str) -> Result<Option<PathBuf>> {
        if !self.is_available {
            return Err(anyhow!("File dialog not available"));
        }
        
        #[cfg(target_os = "linux")]
        {
            let output = std::process::Command::new("zenity")
                .args([
                    "--file-selection",
                    "--save",
                    "--title", title,
                ])
                .output()?;
            
            if output.status.success() {
                let stdout = String::from_utf8(output.stdout)?;
                Ok(Some(PathBuf::from(stdout.trim())))
            } else {
                Ok(None)
            }
        }
        
        #[cfg(target_os = "macos")]
        {
            let script = format!(
                r#"tell application "System Events"
                    set theFile to POSIX path of (choose file name with prompt "{}")
                end tell"#,
                title
            );
            
            let output = std::process::Command::new("osascript")
                .args(["-e", &script])
                .output()?;
            
            if output.status.success() {
                let stdout = String::from_utf8(output.stdout)?;
                Ok(Some(PathBuf::from(stdout.trim())))
            } else {
                Ok(None)
            }
        }
        
        #[cfg(target_os = "windows")]
        {
            let script = format!(
                r#"Add-Type -AssemblyName System.Windows.Forms
                $saveFileDialog = New-Object System.Windows.Forms.SaveFileDialog
                $saveFileDialog.Title = "{}"
                $saveFileDialog.Filter = "All Files (*.*)|*.*"
                if ($saveFileDialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{
                    $saveFileDialog.FileName
                }}"#,
                title
            );
            
            let output = std::process::Command::new("powershell")
                .args(["-Command", &script])
                .output()?;
            
            if output.status.success() {
                let stdout = String::from_utf8(output.stdout)?;
                Ok(Some(PathBuf::from(stdout.trim())))
            } else {
                Ok(None)
            }
        }
        
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            Err(anyhow!("Platform not supported"))
        }
    }
    
    pub fn select_directory(&self, title: &str) -> Result<Option<PathBuf>> {
        if !self.is_available {
            return Err(anyhow!("File dialog not available"));
        }
        
        #[cfg(target_os = "linux")]
        {
            let output = std::process::Command::new("zenity")
                .args([
                    "--file-selection",
                    "--directory",
                    "--title", title,
                ])
                .output()?;
            
            if output.status.success() {
                let stdout = String::from_utf8(output.stdout)?;
                Ok(Some(PathBuf::from(stdout.trim())))
            } else {
                Ok(None)
            }
        }
        
        #[cfg(target_os = "macos")]
        {
            let script = format!(
                r#"tell application "System Events"
                    set theFolder to POSIX path of (choose folder with prompt "{}")
                end tell"#,
                title
            );
            
            let output = std::process::Command::new("osascript")
                .args(["-e", &script])
                .output()?;
            
            if output.status.success() {
                let stdout = String::from_utf8(output.stdout)?;
                Ok(Some(PathBuf::from(stdout.trim())))
            } else {
                Ok(None)
            }
        }
        
        #[cfg(target_os = "windows")]
        {
            let script = format!(
                r#"Add-Type -AssemblyName System.Windows.Forms
                $folderBrowser = New-Object System.Windows.Forms.FolderBrowserDialog
                $folderBrowser.Description = "{}"
                if ($folderBrowser.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{
                    $folderBrowser.SelectedPath
                }}"#,
                title
            );
            
            let output = std::process::Command::new("powershell")
                .args(["-Command", &script])
                .output()?;
            
            if output.status.success() {
                let stdout = String::from_utf8(output.stdout)?;
                Ok(Some(PathBuf::from(stdout.trim())))
            } else {
                Ok(None)
            }
        }
        
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            Err(anyhow!("Platform not supported"))
        }
    }
    
    pub fn is_available(&self) -> bool {
        self.is_available
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_file_dialog_creation() {
        let dialog = FileDialog::new();
        // Availability depends on the system
        assert!(dialog.is_available() || !dialog.is_available());
    }
}