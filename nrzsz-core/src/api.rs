use crate::protocol::Config;

/// Options for creating a filter or relay.
pub struct NrzszOptions {
    pub compress: Option<String>,
    pub overwrite: bool,
    pub binary: bool,
    pub escape: bool,
    pub directory: bool,
    pub fork: bool,
    pub bufsize: Option<String>,
    pub timeout: Option<u32>,
    pub quiet: bool,
    pub relay: bool,
    pub tracelog: bool,
    pub dragfile: bool,
    pub zmodem: bool,
    pub osc52: bool,
}

impl Default for NrzszOptions {
    fn default() -> Self {
        Self {
            compress: Some("auto".to_string()),
            overwrite: false,
            binary: false,
            escape: false,
            directory: false,
            fork: false,
            bufsize: Some("10M".to_string()),
            timeout: Some(20),
            quiet: false,
            relay: false,
            tracelog: false,
            dragfile: false,
            zmodem: false,
            osc52: false,
        }
    }
}

impl NrzszOptions {
    pub fn to_config(&self) -> Config {
        let compress = match self.compress.as_deref() {
            Some("yes") => crate::protocol::CompressType::Yes,
            Some("no") => crate::protocol::CompressType::No,
            _ => crate::protocol::CompressType::Auto,
        };
        let timeout = self.timeout.unwrap_or(20);
        let max_buf_size = self.bufsize.as_ref()
            .and_then(|b| parse_size(b))
            .unwrap_or(10 * 1024 * 1024) as usize;
        Config {
            quiet: self.quiet,
            binary: self.binary,
            overwrite: self.overwrite,
            directory: self.directory,
            fork: Some(self.fork),
            compress,
            timeout,
            max_buf_size,
            ..Config::default()
        }
    }
}

fn parse_size(s: &str) -> Option<i64> {
    let s = s.trim().to_lowercase();
    let (num, unit) = if let Some(pos) = s.find(|c: char| c.is_alphabetic()) {
        (&s[..pos], &s[pos..])
    } else {
        (s.as_str(), "")
    };
    let n: i64 = num.parse().ok()?;
    Some(match unit {
        "b" | "" => n,
        "k" | "kb" => n * 1024,
        "m" | "mb" => n * 1024 * 1024,
        "g" | "gb" => n * 1024 * 1024 * 1024,
        _ => n,
    })
}

#[derive(Debug, Clone)]
pub enum TransferState {
    Starting { total_files: usize },
    Transferring { current_file: String, progress: f64 },
    Completed { total_files: usize },
    Error { message: String },
}

pub type TransferStateCallback = Box<dyn Fn(TransferState) + Send + Sync>;
pub type RedrawScreenCallback = Box<dyn Fn() + Send + Sync>;

pub struct TransferStateHandler {
    callback: Option<TransferStateCallback>,
}

impl TransferStateHandler {
    pub fn new() -> Self { Self { callback: None } }
    pub fn set_callback(&mut self, cb: TransferStateCallback) { self.callback = Some(cb); }
    pub fn notify(&self, state: TransferState) {
        if let Some(ref cb) = self.callback { cb(state); }
    }
}

impl Default for TransferStateHandler {
    fn default() -> Self { Self::new() }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nrzsz_options() {
        let options = NrzszOptions::default();
        assert_eq!(options.compress, Some("auto".to_string()));
        assert!(!options.overwrite);
    }

    #[test]
    fn test_options_to_config() {
        let options = NrzszOptions {
            quiet: true,
            binary: true,
            overwrite: true,
            compress: Some("yes".to_string()),
            timeout: Some(30),
            ..Default::default()
        };
        let config = options.to_config();
        assert!(config.quiet);
        assert!(config.binary);
        assert!(config.overwrite);
        assert_eq!(config.compress, crate::protocol::CompressType::Yes);
        assert_eq!(config.timeout, 30);
    }

    #[test]
    fn test_parse_size() {
        assert_eq!(parse_size("10M"), Some(10 * 1024 * 1024));
        assert_eq!(parse_size("1K"), Some(1024));
        assert_eq!(parse_size("1G"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size("1024"), Some(1024));
    }
}
