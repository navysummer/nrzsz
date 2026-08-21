#[cfg(unix)]
mod unix;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(unix)]
pub use unix::*;
#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pty_process_creation() {
        let pty = PtyProcess::new();
        assert!(!pty.is_running());
    }
}
