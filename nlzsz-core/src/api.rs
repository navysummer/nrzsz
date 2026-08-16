use std::path::PathBuf;
use std::sync::Mutex;
use anyhow::Result;
use log::debug;

// Global storage for tunnel connector and redraw callback
static TUNNEL_CONNECTOR: Mutex<Option<TunnelConnector>> = Mutex::new(None);
static REDRAW_SCREEN_FUNC: Mutex<Option<Box<dyn Fn() + Send + Sync>>> = Mutex::new(None);

pub struct NlzszOptions {
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

impl Default for NlzszOptions {
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

pub struct NlzszFilter {
    options: NlzszOptions,
    is_running: bool,
}

impl NlzszFilter {
    pub fn new(options: NlzszOptions) -> Self {
        Self {
            options,
            is_running: false,
        }
    }
    
    pub fn start(&mut self) -> Result<()> {
        self.is_running = true;
        debug!("NlzszFilter started");
        Ok(())
    }
    
    pub fn stop(&mut self) -> Result<()> {
        self.is_running = false;
        debug!("NlzszFilter stopped");
        Ok(())
    }
    
    pub fn is_running(&self) -> bool {
        self.is_running
    }
    
    pub fn upload_files(&self, files: &[PathBuf]) -> Result<()> {
        for file in files {
            debug!("Uploading: {:?}", file);
            // TODO: Implement actual upload logic
        }
        Ok(())
    }
    
    pub fn download_files(&self, files: &[PathBuf]) -> Result<()> {
        for file in files {
            debug!("Downloading: {:?}", file);
            // TODO: Implement actual download logic
        }
        Ok(())
    }
    
    pub fn set_options(&mut self, options: NlzszOptions) {
        self.options = options;
    }
    
    pub fn get_options(&self) -> &NlzszOptions {
        &self.options
    }
}

pub struct NlzszRelay {
    host: String,
    port: u16,
    is_active: bool,
}

impl NlzszRelay {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
            is_active: false,
        }
    }
    
    pub fn start(&mut self) -> Result<()> {
        self.is_active = true;
        debug!("NlzszRelay started on {}:{}", self.host, self.port);
        Ok(())
    }
    
    pub fn stop(&mut self) -> Result<()> {
        self.is_active = false;
        debug!("NlzszRelay stopped");
        Ok(())
    }
    
    pub fn is_active(&self) -> bool {
        self.is_active
    }
    
    pub fn get_host(&self) -> &str {
        &self.host
    }
    
    pub fn get_port(&self) -> u16 {
        self.port
    }
}

pub fn upload_files(files: &[PathBuf], options: NlzszOptions) -> Result<()> {
    let mut filter = NlzszFilter::new(options);
    filter.start()?;
    filter.upload_files(files)?;
    filter.stop()?;
    Ok(())
}

pub fn download_files(files: &[PathBuf], options: NlzszOptions) -> Result<()> {
    let mut filter = NlzszFilter::new(options);
    filter.start()?;
    filter.download_files(files)?;
    filter.stop()?;
    Ok(())
}

pub fn start_relay(host: &str, port: u16) -> Result<NlzszRelay> {
    let mut relay = NlzszRelay::new(host, port);
    relay.start()?;
    Ok(relay)
}

pub fn one_time_upload(file: PathBuf, options: NlzszOptions) -> Result<()> {
    let files = vec![file];
    upload_files(&files, options)
}

pub fn one_time_download(file: PathBuf, options: NlzszOptions) -> Result<()> {
    let files = vec![file];
    download_files(&files, options)
}

pub type TransferStateCallback = Box<dyn Fn(TransferState) + Send + Sync>;

#[derive(Debug, Clone)]
pub enum TransferState {
    Starting { total_files: usize },
    Transferring { current_file: String, progress: f64 },
    Completed { total_files: usize },
    Error { message: String },
}

pub struct TransferStateHandler {
    callback: Option<TransferStateCallback>,
}

impl TransferStateHandler {
    pub fn new() -> Self {
        Self {
            callback: None,
        }
    }
    
    pub fn set_callback(&mut self, callback: TransferStateCallback) {
        self.callback = Some(callback);
    }
    
    pub fn notify(&self, state: TransferState) {
        if let Some(callback) = &self.callback {
            callback(state);
        }
    }
}

impl Default for TransferStateHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct TunnelConnector {
    host: String,
    port: u16,
}

impl TunnelConnector {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
        }
    }
    
    pub fn connect(&self) -> Result<()> {
        debug!("Connecting to tunnel {}:{}", self.host, self.port);
        // TODO: Implement actual tunnel connection
        Ok(())
    }
    
    pub fn disconnect(&self) -> Result<()> {
        debug!("Disconnecting from tunnel {}:{}", self.host, self.port);
        Ok(())
    }
}

pub fn set_tunnel_connector(connector: Option<TunnelConnector>) {
    if let Ok(mut global) = TUNNEL_CONNECTOR.lock() {
        *global = connector;
        debug!("Tunnel connector updated");
    }
}

pub fn get_tunnel_connector() -> Option<TunnelConnector> {
    TUNNEL_CONNECTOR.lock().ok().and_then(|guard| guard.clone())
}

pub fn set_redraw_screen_func(func: Option<Box<dyn Fn() + Send + Sync>>) {
    if let Ok(mut global) = REDRAW_SCREEN_FUNC.lock() {
        *global = func;
        debug!("Redraw screen function updated");
    }
}

pub fn call_redraw_screen() {
    if let Ok(guard) = REDRAW_SCREEN_FUNC.lock() {
        if let Some(func) = guard.as_ref() {
            func();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_nlzsz_options() {
        let options = NlzszOptions::default();
        assert_eq!(options.compress, Some("auto".to_string()));
        assert!(!options.overwrite);
        assert!(!options.binary);
    }
    
    #[test]
    fn test_nlzsz_filter() {
        let options = NlzszOptions::default();
        let mut filter = NlzszFilter::new(options);
        assert!(!filter.is_running());
        
        filter.start().unwrap();
        assert!(filter.is_running());
        
        filter.stop().unwrap();
        assert!(!filter.is_running());
    }
    
    #[test]
    fn test_nlzsz_relay() {
        let mut relay = NlzszRelay::new("localhost", 8080);
        assert!(!relay.is_active());
        
        relay.start().unwrap();
        assert!(relay.is_active());
        
        relay.stop().unwrap();
        assert!(!relay.is_active());
    }
}