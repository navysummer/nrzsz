//! Embedding API — mirrors Go's `trzsz` embedding surface:
//!
//! - `NrzszOptions` bundles the user-facing settings (as Go's embedding
//!   `setters` do) and builds either an [`NrzszFilter`] (via
//!   [`NrzszOptions::build_filter`]) or a bare [`Config`] (via
//!   [`NrzszOptions::to_config`]).
//! - `TransferStateHandler` wires a `bool` transfer-state callback onto the
//!   filter (Go's `SetTransferStateCallback(true/false)`).
//!
//! Relay usage (Go's `-r`) is deliberately not handled here: a relay wraps
//! user-supplied streams around an `NrzszRelay`, so callers construct
//! `NrzszRelay::new(...)` directly with their own pipes.

use crate::filter::NrzszFilter;
use crate::protocol::{CompressType, Config};

/// Options for creating a filter.
///
/// Fields mirror the CLI surface documented on `nsz`/`nrz`/`nrzsz`.
/// `relay` and `tracelog` are consumed by the caller to decide between
/// `NrzszFilter` (transfer / filter mode) and `NrzszRelay` (relay mode);
/// they have no effect on the filter itself.
#[derive(Debug, Clone)]
pub struct NrzszOptions {
    /// Compression mode: "auto" (default), "yes" or "no".
    pub compress: Option<String>,
    pub overwrite: bool,
    pub binary: bool,
    pub escape: bool,
    pub directory: bool,
    /// Fork to background for the transfer (nsz -f).
    pub fork: bool,
    /// Max buffer chunk size, e.g. "10M" (1K<=N<=1G).
    pub bufsize: Option<String>,
    /// Timeout in seconds for each buffer chunk (<= 0 = never).
    pub timeout: Option<u32>,
    pub quiet: bool,
    /// Choose relay mode (for the caller to select `NrzszRelay`, not the filter).
    pub relay: bool,
    /// Trace the server-side protocol frames to a log (relay mode only).
    pub tracelog: bool,
    /// Enable drag-file upload detection in the filter.
    pub dragfile: bool,
    /// Enable ZMODEM passthrough (`-z`).
    pub zmodem: bool,
    /// Enable OSC52 clipboard detection.
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
    /// Build an `NrzszFilter` from these options: the `Config` comes from
    /// [`NrzszOptions::to_config`], and the common setters are wired:
    /// `dragfile` → `set_enable_dragfile`, `zmodem` → `set_enable_zmodem`,
    /// `osc52` → `set_enable_osc52`.
    ///
    /// The caller still has to set the tunnel connector / download or upload
    /// paths / uploaded files that describe the concrete transfer; embed
    /// those with the filter's setters afterwards.
    pub fn build_filter(self) -> NrzszFilter {
        let config = self.to_config();
        let mut filter = NrzszFilter::new(config);
        if self.dragfile {
            filter.set_enable_dragfile(true);
        }
        if self.zmodem {
            filter.set_enable_zmodem(true);
        }
        if self.osc52 {
            filter.set_enable_osc52(true);
        }
        filter
    }

    pub fn to_config(&self) -> Config {
        let compress = match self.compress.as_deref() {
            Some("yes") => CompressType::Yes,
            Some("no") => CompressType::No,
            _ => CompressType::Auto,
        };
        let timeout = self.timeout.unwrap_or(20);
        let max_buf_size = self
            .bufsize
            .as_ref()
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
        "k" | "kb" | "kib" => n * 1024,
        "m" | "mb" | "mib" => n * 1024 * 1024,
        "g" | "gb" | "gib" => n * 1024 * 1024 * 1024,
        _ => n,
    })
}

/// Go's transfer-state callback: `true` when a transfer starts, `false` when
/// it finishes (or is stopped).
pub type TransferStateCallback = Box<dyn Fn(bool) + Send + Sync>;

/// Convenience wrapper that hands the bool callback to the filter.
///
/// Mirrors Go's `SetTransferStateCallback`.
#[derive(Default)]
pub struct TransferStateHandler {
    callback: Option<TransferStateCallback>,
}

impl TransferStateHandler {
    pub fn new() -> Self {
        Self { callback: None }
    }

    pub fn set_callback(&mut self, cb: TransferStateCallback) {
        self.callback = Some(cb);
    }

    /// Register the handler onto the given filter (Go's embedding setter).
    pub fn apply_to(self, filter: &mut NrzszFilter) {
        if let Some(cb) = self.callback {
            filter.set_transfer_state_callback(cb);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nrzsz_options() {
        let options = NrzszOptions::default();
        assert_eq!(options.compress, Some("auto".to_string()));
        assert!(!options.overwrite);
        assert!(!options.dragfile);
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
        assert_eq!(config.compress, CompressType::Yes);
        assert_eq!(config.timeout, 30);
    }

    #[test]
    fn test_parse_size() {
        assert_eq!(parse_size("10M"), Some(10 * 1024 * 1024));
        assert_eq!(parse_size("1K"), Some(1024));
        assert_eq!(parse_size("1G"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size("1024"), Some(1024));
    }

    #[test]
    fn test_build_filter_wires_setters() {
        let options = NrzszOptions {
            dragfile: true,
            zmodem: true,
            osc52: true,
            ..Default::default()
        };
        let filter = options.build_filter();
        assert!(filter.osc52_enabled());
        assert!(filter.zmodem_enabled());
        assert!(filter.dragfile_enabled());
    }

    #[test]
    fn test_transfer_state_handler_applies_to_filter() {
        let mut filter = NrzszFilter::new(crate::protocol::Config {
            quiet: true,
            ..crate::protocol::Config::default()
        });

        let (tx, rx) = std::sync::mpsc::channel();
        let mut handler = TransferStateHandler::new();
        handler.set_callback(Box::new(move |t: bool| {
            let _ = tx.send(t);
        }));
        handler.apply_to(&mut filter);

        // A trigger line makes the filter fire the transfer-state callback
        // with `true` at the start of the transfer (connection refused is
        // fine — the callback fires regardless).
        filter.start().unwrap();
        let _ = filter.process_output(b"::TRZSZ:TRANSFER:S:0.1.0:1:57376\n");
        let got = rx.recv_timeout(std::time::Duration::from_secs(10));
        assert_eq!(got, Ok(true), "transfer-state callback did not fire");
    }
}
