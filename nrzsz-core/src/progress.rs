use indicatif::{ProgressBar, ProgressStyle};
use console::Term;
use anyhow::Result;

pub struct TransferProgress {
    pb: ProgressBar,
    quiet: bool,
}

impl TransferProgress {
    pub fn new(total: u64, quiet: bool) -> Self {
        let pb = ProgressBar::new(total);
        
        if !quiet {
            pb.set_style(ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}) ({eta})")
                .unwrap()
                .progress_chars("=>-"));
        }
        
        Self {
            pb,
            quiet,
        }
    }

    pub fn new_with_file(total: u64, quiet: bool, file_name: &str) -> Self {
        let pb = ProgressBar::new(total);
        
        if !quiet {
            pb.set_style(ProgressStyle::default_bar()
                .template(&format!("{{spinner:.green}} {} [{{elapsed_precise}}] [{{bar:40.cyan/blue}}] {{bytes}}/{{total_bytes}} ({{bytes_per_sec}}) ({{eta}})", file_name))
                .unwrap()
                .progress_chars("=>-"));
        }
        
        Self {
            pb,
            quiet,
        }
    }
    
    pub fn update(&self, current: u64) {
        self.pb.set_position(current);
    }
    
    pub fn finish(&self, message: &str) {
        if self.quiet {
            self.pb.finish_and_clear();
        } else {
            self.pb.finish_with_message(message.to_string());
        }
    }
    
    pub fn finish_with_error(&self, message: &str) {
        if self.quiet {
            self.pb.finish_and_clear();
        } else {
            self.pb.abandon_with_message(message.to_string());
        }
    }
}

pub fn print_transfer_result(file_name: &str, success: bool, size: u64) -> Result<()> {
    let term = Term::stdout();
    
    if success {
        let msg = format!("{}: {} bytes transferred successfully", file_name, size);
        term.write_line(&msg)?;
    } else {
        let msg = format!("{}: transfer failed", file_name);
        term.write_line(&msg)?;
    }
    
    Ok(())
}