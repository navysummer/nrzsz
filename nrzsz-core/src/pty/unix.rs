use std::process::{Command, Child};
use std::os::unix::io::FromRawFd;
use anyhow::{Result, anyhow};
use log::debug;

pub struct PtyProcess {
    child: Option<Child>,
    master_fd: Option<i32>,
}

unsafe impl Send for PtyProcess {}

impl Default for PtyProcess {
    fn default() -> Self {
        Self::new()
    }
}

impl PtyProcess {
    pub fn new() -> Self {
        Self {
            child: None,
            master_fd: None,
        }
    }

    pub fn spawn(&mut self, command: &str, args: &[String]) -> Result<()> {
        let (master_fd, slave_fd) = openpty()?;

        // Create stdio from slave fd
        let stdin = unsafe { std::process::Stdio::from_raw_fd(slave_fd) };
        let stdout = unsafe { std::process::Stdio::from_raw_fd(libc::dup(slave_fd)) };
        let stderr = unsafe { std::process::Stdio::from_raw_fd(libc::dup(slave_fd)) };

        // Close slave fd in parent after dup
        unsafe { libc::close(slave_fd); }

        let mut cmd = Command::new(command);
        cmd.args(args);
        cmd.stdin(stdin);
        cmd.stdout(stdout);
        cmd.stderr(stderr);

        self.child = Some(cmd.spawn()?);
        self.master_fd = Some(master_fd);

        debug!("PTY process spawned: {}", command);
        Ok(())
    }

    pub fn read_output(&mut self, buffer: &mut [u8]) -> Result<usize> {
        if let Some(master_fd) = self.master_fd {
            let n = unsafe {
                libc::read(master_fd, buffer.as_mut_ptr() as *mut libc::c_void, buffer.len())
            };
            if n < 0 {
                Err(anyhow!("Failed to read from PTY"))
            } else {
                Ok(n as usize)
            }
        } else {
            Err(anyhow!("PTY not initialized"))
        }
    }

    pub fn write_input(&mut self, data: &[u8]) -> Result<()> {
        if let Some(master_fd) = self.master_fd {
            let n = unsafe {
                libc::write(master_fd, data.as_ptr() as *const libc::c_void, data.len())
            };
            if n < 0 {
                Err(anyhow!("Failed to write to PTY"))
            } else {
                Ok(())
            }
        } else {
            Err(anyhow!("PTY not initialized"))
        }
    }

    pub fn resize(&self, width: u32, height: u32) -> Result<()> {
        if let Some(master_fd) = self.master_fd {
            let winsize = libc::winsize {
                ws_row: height as u16,
                ws_col: width as u16,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };

            let result = unsafe {
                libc::ioctl(master_fd, libc::TIOCSWINSZ, &winsize as *const libc::winsize)
            };

            if result < 0 {
                Err(anyhow!("Failed to resize PTY"))
            } else {
                Ok(())
            }
        } else {
            Err(anyhow!("PTY not initialized"))
        }
    }

    pub fn wait(&mut self) -> Result<i32> {
        if let Some(ref mut child) = self.child {
            let status = child.wait()?;
            Ok(status.code().unwrap_or(1))
        } else {
            Ok(0)
        }
    }

    pub fn kill(&mut self) -> Result<()> {
        if let Some(ref mut child) = self.child {
            child.kill()?;
        }
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    pub fn close(&mut self) {
        if let Some(fd) = self.master_fd.take() {
            unsafe { libc::close(fd); }
        }
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        self.close();
    }
}

fn openpty() -> Result<(i32, i32)> {
    let mut master: i32 = 0;
    let mut slave: i32 = 0;

    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };

    if result < 0 {
        Err(anyhow!("Failed to open PTY"))
    } else {
        Ok((master, slave))
    }
}

pub fn enable_virtual_terminal() -> Result<()> {
    Ok(())
}
