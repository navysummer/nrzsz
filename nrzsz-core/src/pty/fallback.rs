use std::process::{Command, Child};
use std::io::{Read, Write};
use anyhow::{Result, anyhow};
use log::debug;

pub struct PtyProcess {
    child: Option<Child>,
}

impl Default for PtyProcess {
    fn default() -> Self {
        Self::new()
    }
}

impl PtyProcess {
    pub fn new() -> Self {
        Self { child: None }
    }

    pub fn spawn(&mut self, command: &str, args: &[String]) -> Result<()> {
        let mut cmd = Command::new(command);
        cmd.args(args);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        self.child = Some(cmd.spawn()?);
        debug!("Process spawned (non-PTY): {}", command);
        Ok(())
    }

    pub fn read_output(&mut self, buffer: &mut [u8]) -> Result<usize> {
        if let Some(ref mut child) = self.child {
            if let Some(ref mut stdout) = child.stdout {
                Ok(stdout.read(buffer)?)
            } else {
                Err(anyhow!("No stdout"))
            }
        } else {
            Err(anyhow!("No child process"))
        }
    }

    pub fn write_input(&mut self, data: &[u8]) -> Result<()> {
        if let Some(ref mut child) = self.child {
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(data)?;
                stdin.flush()?;
                Ok(())
            } else {
                Err(anyhow!("No stdin"))
            }
        } else {
            Err(anyhow!("No child process"))
        }
    }

    pub fn resize(&self, _width: u32, _height: u32) -> Result<()> {
        Ok(())
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

    pub fn close(&mut self) {}
}

pub fn enable_virtual_terminal() -> Result<()> {
    Ok(())
}
