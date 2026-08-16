use anyhow::{Result, anyhow};
use log::debug;

#[cfg(unix)]
use std::os::fd::AsRawFd;

pub fn fork_process() -> Result<bool> {
    #[cfg(unix)]
    {
        // On Unix, we can use fork() to create a child process
        unsafe {
            let pid = libc::fork();
            if pid < 0 {
                return Err(anyhow!("Fork failed"));
            } else if pid > 0 {
                // Parent process - return true to indicate we're the parent
                debug!("Parent process continuing, child PID: {}", pid);
                return Ok(true);
            }
            // Child process - return false to indicate we're the child
            debug!("Child process started");
            Ok(false)
        }
    }
    
    #[cfg(windows)]
    {
        // On Windows, we don't have fork(), so we'll use a different approach
        // We'll spawn a new process and detach it
        debug!("Starting background transfer on Windows");
        
        let current_exe = std::env::current_exe()?;
        let args: Vec<String> = std::env::args().skip(1).collect();
        
        let mut cmd = std::process::Command::new(current_exe);
        cmd.args(&args);
        cmd.creation_flags(0x00000008); // DETACHED_PROCESS
        
        let child = cmd.spawn()?;
        debug!("Background process started with PID: {}", child.id());
        
        // Return false to indicate we're the child (background process)
        Ok(false)
    }
    
    #[cfg(not(any(unix, windows)))]
    {
        return Err(anyhow!("Background transfer not supported on this platform"));
    }
}

pub fn daemonize() -> Result<()> {
    #[cfg(unix)]
    {
        // Create a new session
        unsafe {
            if libc::setsid() < 0 {
                return Err(anyhow!("Failed to create new session"));
            }
        }
        
        // Redirect standard file descriptors to /dev/null
        let dev_null = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")?;
        
        unsafe {
            libc::dup2(dev_null.as_raw_fd(), 0); // stdin
            libc::dup2(dev_null.as_raw_fd(), 1); // stdout
            libc::dup2(dev_null.as_raw_fd(), 2); // stderr
        }
        
        debug!("Process daemonized");
    }
    
    #[cfg(windows)]
    {
        // On Windows, we don't have daemonize() in the same way
        // The process will continue in the background
        debug!("Background transfer started (Windows)");
    }
    
    Ok(())
}

pub fn is_background_mode() -> bool {
    std::env::var("NLZSZ_BACKGROUND").is_ok()
}

pub fn set_background_mode() {
    std::env::set_var("NLZSZ_BACKGROUND", "1");
}
