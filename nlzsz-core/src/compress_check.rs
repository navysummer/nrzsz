use std::path::Path;
use anyhow::Result;
use log::debug;

pub struct CompressionChecker {
    min_compression_ratio: f64,
    max_compression_size: usize,
}

impl CompressionChecker {
    pub fn new() -> Self {
        Self {
            min_compression_ratio: 0.9, // Don't compress if ratio > 90%
            max_compression_size: 100 * 1024 * 1024, // 100MB
        }
    }
    
    pub fn is_compression_profitable(&self, file_path: &Path) -> Result<bool> {
        // Check file size
        let metadata = std::fs::metadata(file_path)?;
        let file_size = metadata.len() as usize;
        
        // Don't compress very large files
        if file_size > self.max_compression_size {
            debug!("File too large for compression: {} bytes", file_size);
            return Ok(false);
        }
        
        // Check if file is already compressed
        if self.is_already_compressed(file_path)? {
            debug!("File is already compressed");
            return Ok(false);
        }
        
        // Check file extension
        if let Some(ext) = file_path.extension() {
            let ext = ext.to_string_lossy().to_lowercase();
            
            // Skip compression for already compressed formats
            let compressed_extensions = [
                "gz", "bz2", "xz", "zst", "lz4", "lzma", "zip", "rar", "7z",
                "jpg", "jpeg", "png", "gif", "bmp", "tiff", "webp",
                "mp3", "mp4", "avi", "mkv", "mov", "wmv",
                "pdf", "docx", "xlsx", "pptx",
            ];
            
            if compressed_extensions.contains(&ext.as_str()) {
                debug!("File extension suggests already compressed: {}", ext);
                return Ok(false);
            }
        }
        
        Ok(true)
    }
    
    pub fn is_already_compressed(&self, file_path: &Path) -> Result<bool> {
        // Read first few bytes to check for compression signatures
        let mut file = std::fs::File::open(file_path)?;
        let mut header = [0u8; 16];
        
        use std::io::Read;
        let bytes_read = file.read(&mut header)?;
        
        if bytes_read < 4 {
            return Ok(false);
        }
        
        // Check for common compression signatures
        // gzip: 1f 8b
        if header[0] == 0x1f && header[1] == 0x8b {
            return Ok(true);
        }
        
        // bzip2: 42 5a 68
        if header[0] == 0x42 && header[1] == 0x5a && header[2] == 0x68 {
            return Ok(true);
        }
        
        // xz: fd 37 7a 58 5a 00
        if header[0] == 0xfd && header[1] == 0x37 && header[2] == 0x7a
            && header[3] == 0x58 && header[4] == 0x5a && header[5] == 0x00
        {
            return Ok(true);
        }
        
        // zstd: 28 b5 2f fd
        if header[0] == 0x28 && header[1] == 0xb5 && header[2] == 0x2f && header[3] == 0xfd {
            return Ok(true);
        }
        
        // lz4: 04 22 4d 18
        if header[0] == 0x04 && header[1] == 0x22 && header[2] == 0x4d && header[3] == 0x18 {
            return Ok(true);
        }
        
        // lzma/xz: fd 37 7a 58 5a 00
        if header[0] == 0xfd && header[1] == 0x37 {
            return Ok(true);
        }
        
        // zip: 50 4b 03 04
        if header[0] == 0x50 && header[1] == 0x4b && header[2] == 0x03 && header[3] == 0x04 {
            return Ok(true);
        }
        
        // rar: 52 61 72 21
        if header[0] == 0x52 && header[1] == 0x61 && header[2] == 0x72 && header[3] == 0x21 {
            return Ok(true);
        }
        
        // 7z: 37 7a bc af 27 1c
        if header[0] == 0x37 && header[1] == 0x7a && header[2] == 0xbc && header[3] == 0xaf {
            return Ok(true);
        }
        
        // PNG: 89 50 4e 47
        if header[0] == 0x89 && header[1] == 0x50 && header[2] == 0x4e && header[3] == 0x47 {
            return Ok(true);
        }
        
        // JPEG: ff d8 ff
        if header[0] == 0xff && header[1] == 0xd8 && header[2] == 0xff {
            return Ok(true);
        }
        
        // GIF: 47 49 46 38
        if header[0] == 0x47 && header[1] == 0x49 && header[2] == 0x46 && header[3] == 0x38 {
            return Ok(true);
        }
        
        // PDF: 25 50 44 46
        if header[0] == 0x25 && header[1] == 0x50 && header[2] == 0x44 && header[3] == 0x46 {
            return Ok(true);
        }
        
        Ok(false)
    }
    
    pub fn estimate_compression_ratio(&self, data: &[u8]) -> f64 {
        if data.is_empty() {
            return 1.0;
        }
        
        // Simple entropy estimation
        let mut freq = [0u32; 256];
        for &byte in data {
            freq[byte as usize] += 1;
        }
        
        let len = data.len() as f64;
        let mut entropy = 0.0;
        
        for &count in &freq {
            if count > 0 {
                let p = count as f64 / len;
                entropy -= p * p.log2();
            }
        }
        
        // Normalize entropy (0-8 bits per byte)
        entropy / 8.0
    }
    
    pub fn set_min_compression_ratio(&mut self, ratio: f64) {
        self.min_compression_ratio = ratio;
    }
    
    pub fn set_max_compression_size(&mut self, size: usize) {
        self.max_compression_size = size;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_compression_checker_creation() {
        let checker = CompressionChecker::new();
        assert_eq!(checker.min_compression_ratio, 0.9);
        assert_eq!(checker.max_compression_size, 100 * 1024 * 1024);
    }
    
    #[test]
    fn test_estimate_compression_ratio() {
        let checker = CompressionChecker::new();
        
        // Empty data
        let ratio = checker.estimate_compression_ratio(&[]);
        assert_eq!(ratio, 1.0);
        
        // All same bytes (low entropy, high compression)
        let data = vec![0u8; 1000];
        let ratio = checker.estimate_compression_ratio(&data);
        assert!(ratio < 0.1);
        
        // Random data (high entropy, low compression)
        let data: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
        let ratio = checker.estimate_compression_ratio(&data);
        assert!(ratio > 0.5);
    }
}