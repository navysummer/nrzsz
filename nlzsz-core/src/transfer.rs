use std::io::{Read, Write};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use anyhow::{Result, anyhow};

use crate::protocol::{Config, FileInfo, DirectoryInfo, PROTOCOL_VERSION};
use crate::utils::*;
use crate::progress::TransferProgress;

static STOPPED: AtomicBool = AtomicBool::new(false);

pub fn set_stopped(stopped: bool) {
    STOPPED.store(stopped, Ordering::SeqCst);
}

pub fn is_stopped() -> bool {
    STOPPED.load(Ordering::SeqCst)
}

pub struct TransferSession {
    config: Config,
    stdin: Box<dyn Read>,
    stdout: Box<dyn Write>,
    created_files: Vec<PathBuf>,
    stopped: Arc<AtomicBool>,
    #[cfg(unix)]
    original_tmux_interval: Option<String>,
    negotiated_protocol: u32,
    remote_capabilities: Option<crate::protocol::ClientCapabilities>,
}

impl TransferSession {
    pub fn new(config: Config, stdin: Box<dyn Read>, stdout: Box<dyn Write>) -> Self {
        Self {
            config,
            stdin,
            stdout,
            created_files: Vec::new(),
            stopped: Arc::new(AtomicBool::new(false)),
            #[cfg(unix)]
            original_tmux_interval: None,
            negotiated_protocol: PROTOCOL_VERSION,
            remote_capabilities: None,
        }
    }
    
    pub fn set_stopped(&self, stopped: bool) {
        self.stopped.store(stopped, Ordering::SeqCst);
        set_stopped(stopped);
    }
    
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst) || is_stopped()
    }
    
    #[cfg(unix)]
    pub fn save_tmux_status_interval(&mut self) {
        if let Ok(interval) = crate::tmux::get_tmux_status_interval() {
            self.original_tmux_interval = Some(interval);
            // Set status interval to 0 during transfer to prevent tmux from refreshing
            let _ = crate::tmux::set_tmux_status_interval("0");
        }
    }
    
    #[cfg(unix)]
    pub fn restore_tmux_status_interval(&mut self) {
        if let Some(interval) = &self.original_tmux_interval {
            let _ = crate::tmux::set_tmux_status_interval(interval);
            self.original_tmux_interval = None;
        }
    }
    
    pub fn get_negotiated_protocol(&self) -> u32 {
        self.negotiated_protocol
    }
    
    pub fn get_remote_capabilities(&self) -> Option<&crate::protocol::ClientCapabilities> {
        self.remote_capabilities.as_ref()
    }
    
    pub fn can_use_binary(&self) -> bool {
        self.config.binary && self.remote_capabilities.as_ref()
            .is_none_or(|caps| caps.support_binary)
    }
    
    pub fn can_use_compress(&self) -> bool {
        self.remote_capabilities.as_ref()
            .is_none_or(|caps| caps.support_compress)
    }
    
    pub fn send_action(&mut self, confirm: bool, version: &str, remote_is_windows: bool) -> Result<()> {
        let mut action = crate::protocol::Action {
            lang: "rs".to_string(),
            confirm,
            version: version.to_string(),
            support_dir: true,
            protocol: PROTOCOL_VERSION,
            newline: None,
            binary: None,
            tunnel: None,
            fork: None,
            tmuxcc: None,
            support_binary: Some(true),
            comp: Some(1), // Support compression
        };
        
        if remote_is_windows {
            action.newline = Some("!\n".to_string());
            action.binary = Some(false);
        }
        
        let action_json = serde_json::to_string(&action)?;
        send_string(&mut self.stdout, "ACT", &action_json, &self.config.newline)?;
        Ok(())
    }
    
    pub fn recv_action(&mut self) -> Result<crate::protocol::Action> {
        let mut buf = Vec::new();
        let action_str = recv_string(&mut self.stdin, "ACT", &mut buf)?;
        let action: crate::protocol::Action = serde_json::from_str(&action_str)?;
        
        if let Some(newline) = &action.newline {
            self.config.newline = newline.clone();
        }
        
        // Negotiate protocol version
        self.negotiated_protocol = crate::protocol::negotiate_protocol_version(
            PROTOCOL_VERSION,
            action.protocol,
        );
        
        // Extract client capabilities
        self.remote_capabilities = Some(crate::protocol::ClientCapabilities::from_action(&action));
        
        Ok(action)
    }
    
    pub fn send_config(&mut self, action: &crate::protocol::Action) -> Result<()> {
        let mut config = self.config.clone();
        
        // Adjust config based on remote capabilities
        let remote_caps = crate::protocol::ClientCapabilities::from_action(action);
        
        // If remote doesn't support binary, force text mode
        if !remote_caps.support_binary {
            config.binary = false;
        }
        
        // If remote doesn't support compression, disable it
        if !remote_caps.support_compress {
            config.compress = crate::protocol::CompressType::No;
        }
        
        // If remote doesn't support directory transfer, disable it
        if !remote_caps.support_directory {
            config.directory = false;
        }
        
        let config_json = serde_json::to_string(&config)?;
        send_string(&mut self.stdout, "CFG", &config_json, &self.config.newline)?;
        Ok(())
    }
    
    pub fn recv_config(&mut self) -> Result<Config> {
        let mut buf = Vec::new();
        let config_str = recv_string(&mut self.stdin, "CFG", &mut buf)?;
        let config: Config = serde_json::from_str(&config_str)?;
        self.config = config.clone();
        Ok(config)
    }
    
    pub fn client_exit(&mut self, msg: &str) -> Result<()> {
        send_string(&mut self.stdout, "EXIT", msg, &self.config.newline)?;
        Ok(())
    }
    
    pub fn recv_exit(&mut self) -> Result<String> {
        let mut buf = Vec::new();
        recv_string(&mut self.stdin, "EXIT", &mut buf)
    }
    
    pub fn server_exit(&mut self, msg: &str) -> Result<()> {
        // Reset terminal state
        self.stdout.write_all(b"\x1b8\x1b[0J")?;
        self.stdout.write_all(msg.as_bytes())?;
        self.stdout.write_all(b"\r\n")?;
        self.stdout.write_all(b"\x1b[?25h")?;
        self.stdout.flush()?;
        Ok(())
    }
    
    pub fn client_error(&mut self, ex: &anyhow::Error) -> Result<()> {
        let err_msg = ex.to_string();
        send_string(&mut self.stdout, "FAIL", &err_msg, &self.config.newline)?;
        eprintln!("{}", err_msg);
        Ok(())
    }
    
    pub fn server_error(&mut self, ex: &anyhow::Error) -> Result<()> {
        let err_msg = ex.to_string();
        send_string(&mut self.stdout, "FAIL", &err_msg, &self.config.newline)?;
        self.server_exit(&err_msg)
    }
    
    pub fn send_file_num(&mut self, num: usize) -> Result<()> {
        send_integer(&mut self.stdout, "NUM", num as i64, &self.config.newline)?;
        check_integer(&mut self.stdin, num as i64, &mut Vec::new())?;
        Ok(())
    }
    
    pub fn recv_file_num(&mut self) -> Result<usize> {
        let mut buf = Vec::new();
        let num = recv_integer(&mut self.stdin, "NUM", &mut buf)? as usize;
        send_integer(&mut self.stdout, "SUCC", num as i64, &self.config.newline)?;
        Ok(num)
    }
    
    pub fn send_file_name(&mut self, file: &FileInfo) -> Result<String> {
        let name = file.path_name.last().unwrap().clone();
        
        // Validate filename length (255 characters max)
        if name.len() > 255 {
            return Err(anyhow!("Filename too long: {} (max 255 characters)", name));
        }
        
        if self.config.directory {
            let file_json = serde_json::to_string(file)?;
            send_string(&mut self.stdout, "NAME", &file_json, &self.config.newline)?;
        } else {
            send_string(&mut self.stdout, "NAME", &name, &self.config.newline)?;
        }
        
        let mut buf = Vec::new();
        let remote_name = recv_string(&mut self.stdin, "SUCC", &mut buf)?;
        Ok(remote_name)
    }
    
    pub fn recv_file_name(&mut self, dest_path: &Path) -> Result<(Option<File>, String, String)> {
        if self.config.directory {
            let mut buf = Vec::new();
            let file_json = recv_string(&mut self.stdin, "NAME", &mut buf)?;
            let file_info: DirectoryInfo = serde_json::from_str(&file_json)?;
            
            // Validate filename length (255 characters max)
            if file_info.path_name.last().is_some_and(|name| name.len() > 255) {
                return Err(anyhow!("Filename too long (max 255 characters)"));
            }
            
            let (file, local_name, file_name) = self.create_dir_or_file(dest_path, &file_info)?;
            
            send_string(&mut self.stdout, "SUCC", &local_name, &self.config.newline)?;
            Ok((file, local_name, file_name))
        } else {
            let mut buf = Vec::new();
            let file_name = recv_string(&mut self.stdin, "NAME", &mut buf)?;
            
            // Validate filename length (255 characters max)
            if file_name.len() > 255 {
                return Err(anyhow!("Filename too long: {} (max 255 characters)", file_name));
            }
            
            let (file, local_name) = self.create_file(dest_path, &file_name)?;
            
            send_string(&mut self.stdout, "SUCC", &local_name, &self.config.newline)?;
            Ok((Some(file), local_name, file_name))
        }
    }
    
    pub fn send_file_size(&mut self, file_path: &Path) -> Result<u64> {
        let metadata = fs::metadata(file_path)?;
        let size = metadata.len();
        send_integer(&mut self.stdout, "SIZE", size as i64, &self.config.newline)?;
        check_integer(&mut self.stdin, size as i64, &mut Vec::new())?;
        Ok(size)
    }
    
    pub fn recv_file_size(&mut self) -> Result<u64> {
        let mut buf = Vec::new();
        let size = recv_integer(&mut self.stdin, "SIZE", &mut buf)? as u64;
        send_integer(&mut self.stdout, "SUCC", size as i64, &self.config.newline)?;
        Ok(size)
    }
    
    pub fn send_file_data(&mut self, file_path: &Path, size: u64, progress: &TransferProgress) -> Result<Vec<u8>> {
        let mut file = File::open(file_path)?;
        let mut buf = vec![0u8; 1024];
        let mut md5_hash = md5::Context::new();
        let mut sent = 0u64;
        
        // Check if remote supports compression before using it
        let compress_type = if self.can_use_compress() {
            match &self.config.compress {
                crate::protocol::CompressType::Auto => crate::compress::CompressAlgo::Zstd,
                crate::protocol::CompressType::Yes => crate::compress::CompressAlgo::Zstd,
                crate::protocol::CompressType::No => crate::compress::CompressAlgo::None,
            }
        } else {
            crate::compress::CompressAlgo::None
        };
        
        while sent < size {
            let bytes_read = file.read(&mut buf)?;
            if bytes_read == 0 {
                break;
            }
            
            let data = &buf[..bytes_read];
            
            let compressed_data = if compress_type != crate::compress::CompressAlgo::None {
                crate::compress::compress_data(data, compress_type)?
            } else {
                data.to_vec()
            };
            
            if self.config.binary {
                let escaped = escape_data(&compressed_data, &self.config.escape_chars);
                send_line(&mut self.stdout, "DATA", &escaped.len().to_string(), &self.config.newline)?;
                self.stdout.write_all(&escaped)?;
            } else {
                send_binary(&mut self.stdout, "DATA", &compressed_data, &self.config.newline)?;
            }
            
            md5_hash.consume(data);
            
            check_integer(&mut self.stdin, compressed_data.len() as i64, &mut Vec::new())?;
            
            sent += bytes_read as u64;
            progress.update(sent);
        }
        
        Ok(md5_hash.compute().to_vec())
    }
    
    pub fn recv_file_data(&mut self, file: &mut File, size: u64, progress: &TransferProgress) -> Result<Vec<u8>> {
        let mut md5_hash = md5::Context::new();
        let mut received = 0u64;
        
        // Check if remote supports compression before using it
        let mut compress_type = if self.can_use_compress() {
            match &self.config.compress {
                crate::protocol::CompressType::Auto => crate::compress::CompressAlgo::Zstd,
                crate::protocol::CompressType::Yes => crate::compress::CompressAlgo::Zstd,
                crate::protocol::CompressType::No => crate::compress::CompressAlgo::None,
            }
        } else {
            crate::compress::CompressAlgo::None
        };
        let mut first_chunk = true;
        
        while received < size {
            let compressed_data = if self.config.binary {
                let mut buf = Vec::new();
                let line = recv_line(&mut self.stdin, &mut buf)?;
                let idx = line.find(':').ok_or_else(|| anyhow!("Invalid format"))?;
                let prefix = &line[1..idx];
                if prefix != "DATA" {
                    return Err(anyhow!("Expected DATA, got {}", prefix));
                }
                let len = line[idx+1..].trim().parse::<usize>()?;
                let mut data_buf = vec![0u8; len];
                self.stdin.read_exact(&mut data_buf)?;
                unescape_data(&data_buf, &self.config.escape_chars)
            } else {
                let mut buf = Vec::new();
                recv_binary(&mut self.stdin, "DATA", &mut buf)?
            };
            
            if first_chunk && compress_type == crate::compress::CompressAlgo::None {
                compress_type = crate::compress::detect_compress_type(&compressed_data);
                first_chunk = false;
            }
            
            let data = if compress_type != crate::compress::CompressAlgo::None {
                crate::compress::decompress_data(&compressed_data, compress_type)?
            } else {
                compressed_data.clone()
            };
            
            file.write_all(&data)?;
            md5_hash.consume(&data);
            
            send_integer(&mut self.stdout, "SUCC", compressed_data.len() as i64, &self.config.newline)?;
            
            // Track uncompressed data size
            received += data.len() as u64;
            progress.update(received);
        }
        
        Ok(md5_hash.compute().to_vec())
    }
    
    pub fn send_file_md5(&mut self, digest: &[u8]) -> Result<()> {
        send_binary(&mut self.stdout, "MD5", digest, &self.config.newline)?;
        let mut buf = Vec::new();
        check_binary(&mut self.stdin, digest, &mut buf)?;
        Ok(())
    }
    
    pub fn recv_file_md5(&mut self, digest: &[u8]) -> Result<()> {
        let mut buf = Vec::new();
        let expected = recv_binary(&mut self.stdin, "MD5", &mut buf)?;
        if digest != expected.as_slice() {
            return Err(anyhow!("MD5 check failed"));
        }
        send_binary(&mut self.stdout, "SUCC", digest, &self.config.newline)?;
        Ok(())
    }
    
    pub fn send_files(&mut self, file_list: &[FileInfo], quiet: bool) -> Result<Vec<String>> {
        // Save tmux status interval before transfer
        self.save_tmux_status_interval();
        
        self.send_file_num(file_list.len())?;
        
        let mut remote_list = Vec::new();
        
        for file in file_list {
            let remote_name = self.send_file_name(file)?;
            
            if !remote_list.contains(&remote_name) {
                remote_list.push(remote_name.clone());
            }
            
            if file.is_dir {
                continue;
            }
            
            let size = self.send_file_size(Path::new(&file.abs_path))?;
            let progress = TransferProgress::new(size, quiet);
            let md5 = self.send_file_data(Path::new(&file.abs_path), size, &progress)?;
            self.send_file_md5(&md5)?;
            progress.finish("Done");
        }
        
        // Restore tmux status interval after transfer
        self.restore_tmux_status_interval();
        
        Ok(remote_list)
    }
    
    pub fn recv_files(&mut self, dest_path: &Path, quiet: bool) -> Result<Vec<String>> {
        // Save tmux status interval before transfer
        self.save_tmux_status_interval();
        
        let num = self.recv_file_num()?;
        let mut local_list = Vec::new();
        
        for _ in 0..num {
            let (file, local_name, _file_name) = self.recv_file_name(dest_path)?;
            
            if !local_list.contains(&local_name) {
                local_list.push(local_name);
            }
            
            let mut file = match file {
                Some(f) => f,
                None => continue,
            };
            
            let size = self.recv_file_size()?;
            let progress = TransferProgress::new(size, quiet);
            let md5 = self.recv_file_data(&mut file, size, &progress)?;
            self.recv_file_md5(&md5)?;
            progress.finish("Done");
        }
        
        // Restore tmux status interval after transfer
        self.restore_tmux_status_interval();
        
        Ok(local_list)
    }
    
    fn create_file(&mut self, dest_path: &Path, file_name: &str) -> Result<(File, String)> {
        let local_name = if self.config.overwrite {
            file_name.to_string()
        } else {
            get_new_name(dest_path, file_name)?
        };
        
        let full_path = dest_path.join(&local_name);
        let file = File::create(&full_path)?;
        self.created_files.push(full_path);
        
        Ok((file, local_name))
    }
    
    fn create_dir_or_file(&mut self, dest_path: &Path, file_info: &DirectoryInfo) -> Result<(Option<File>, String, String)> {
        let file_name = file_info.path_name.last().unwrap().clone();
        
        let local_name = if self.config.overwrite {
            file_info.path_name[0].clone()
        } else {
            get_new_name(dest_path, &file_info.path_name[0])?
        };
        
        if file_info.path_name.len() > 1 {
            let parent_path = dest_path.join(&local_name).join(file_info.path_name[1..file_info.path_name.len()-1].iter().collect::<PathBuf>());
            fs::create_dir_all(&parent_path)?;
            self.created_files.push(parent_path.clone());
            
            let full_path = parent_path.join(&file_name);
            
            if file_info.is_dir {
                fs::create_dir_all(&full_path)?;
                self.created_files.push(full_path);
                return Ok((None, local_name, file_name));
            }
            
            let file = File::create(&full_path)?;
            self.created_files.push(full_path);
            Ok((Some(file), local_name, file_name))
        } else {
            let full_path = dest_path.join(&local_name);
            
            if file_info.is_dir {
                fs::create_dir_all(&full_path)?;
                self.created_files.push(full_path);
                return Ok((None, local_name, file_name));
            }
            
            let file = File::create(&full_path)?;
            self.created_files.push(full_path);
            Ok((Some(file), local_name, file_name))
        }
    }
}

fn get_new_name(path: &Path, name: &str) -> Result<String> {
    let full_path = path.join(name);
    if !full_path.exists() {
        return Ok(name.to_string());
    }
    
    for i in 0..1000 {
        let new_name = format!("{}.{}", name, i);
        if !path.join(&new_name).exists() {
            return Ok(new_name);
        }
    }
    
    Err(anyhow!("Failed to assign new file name"))
}

pub fn check_paths_readable(paths: &[String], directory: bool) -> Result<Vec<FileInfo>> {
    let mut file_list = Vec::new();
    
    for (i, path) in paths.iter().enumerate() {
        let abs_path = PathBuf::from(path).canonicalize()?;
        
        if !abs_path.exists() {
            return Err(anyhow!("No such file: {}", abs_path.display()));
        }
        
        let metadata = fs::metadata(&abs_path)?;
        
        if !directory && metadata.is_dir() {
            return Err(anyhow!("Is a directory: {}", abs_path.display()));
        }
        
        let mut visited_dirs = std::collections::HashSet::new();
        check_path_readable(i as u32, &abs_path, &mut file_list, &mut vec![abs_path.file_name().unwrap().to_string_lossy().to_string()], &mut visited_dirs)?;
    }
    
    Ok(file_list)
}

fn check_path_readable(
    path_id: u32,
    path: &Path,
    file_list: &mut Vec<FileInfo>,
    rel_path: &mut Vec<String>,
    visited_dirs: &mut std::collections::HashSet<PathBuf>,
) -> Result<()> {
    let metadata = fs::metadata(path)?;
    
    if !metadata.is_dir() {
        if !metadata.is_file() {
            return Err(anyhow!("Not a regular file: {}", path.display()));
        }
        
        // Check read permission (platform-specific)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o444 == 0 {
                return Err(anyhow!("No permission to read: {}", path.display()));
            }
        }
        
        #[cfg(windows)]
        {
            // On Windows, we can't easily check read permissions in the same way
            // We'll rely on the file open operation to fail if no permission
        }
        
        file_list.push(FileInfo {
            path_id,
            abs_path: path.to_string_lossy().to_string(),
            path_name: rel_path.clone(),
            is_dir: false,
        });
        
        return Ok(());
    }
    
    let real_path = fs::canonicalize(path)?;
    if visited_dirs.contains(&real_path) {
        return Err(anyhow!("Duplicate link: {}", path.display()));
    }
    visited_dirs.insert(real_path);
    
    file_list.push(FileInfo {
        path_id,
        abs_path: path.to_string_lossy().to_string(),
        path_name: rel_path.clone(),
        is_dir: true,
    });
    
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file_path = entry.path();
        rel_path.push(entry.file_name().to_string_lossy().to_string());
        check_path_readable(path_id, &file_path, file_list, rel_path, visited_dirs)?;
        rel_path.pop();
    }
    
    Ok(())
}