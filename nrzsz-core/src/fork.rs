use anyhow::{Result, anyhow};
use log::debug;
use std::io::Write;

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
    std::env::var("NRZSZ_BACKGROUND").is_ok()
}

pub fn set_background_mode() {
    std::env::set_var("NRZSZ_BACKGROUND", "1");
}

pub fn switch_to_background() {
    // Signal server by closing stdin
    // Note: caller should handle terminal reset
    std::env::set_var("NRZSZ_BACKGROUND", "1");
    debug!("Switching to background transfer mode");
}

/// Fork to background matching Go's forkToBackground.
/// Re-execs the current process with NRZSZ-FORK-BACKGROUND=TRUE.
/// Returns Ok(true) if this is the parent (should continue as the foreground),
/// Ok(false) if this is the background process (should continue the transfer).
#[cfg(unix)]
pub fn fork_to_background() -> Result<bool> {
    if std::env::var("NRZSZ-FORK-BACKGROUND") == Ok("TRUE".to_string()) {
        // This is the background process
        return Ok(false);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.args(&args);
    cmd.env("NRZSZ-FORK-BACKGROUND", "TRUE");
    cmd.stdout(std::process::Stdio::inherit());
    cmd.stdin(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()?;

    let mut stdin = child.stdin.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();

    // Forward our stdin to the child and child's stderr to our stderr
    let input = std::io::stdin();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut input = input.lock();
        let mut buf = [0u8; 1024];
        loop {
            match input.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    stdin.write_all(&buf[..n]).ok();
                    stdin.flush().ok();
                }
                Err(_) => break,
            }
        }
    });

    std::thread::spawn(move || {
        use std::io::{Read, Write};
        let mut out = std::io::stderr();
        let mut buf = [0u8; 1024];
        loop {
            match stderr.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    out.write_all(&buf[..n]).ok();
                    out.flush().ok();
                }
                Err(_) => break,
            }
        }
    });

    let _ = child.wait();
    Ok(true)
}

#[cfg(windows)]
pub fn fork_to_background() -> Result<bool> {
    use std::io::Write;
    if std::env::var("NRZSZ-FORK-BACKGROUND") == Ok("TRUE".to_string()) {
        return Ok(false);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.args(&args);
    cmd.env("NRZSZ-FORK-BACKGROUND", "TRUE");
    cmd.creation_flags(0x00000008); // DETACHED_PROCESS
    let _child = cmd.spawn()?;
    Ok(true)
}
