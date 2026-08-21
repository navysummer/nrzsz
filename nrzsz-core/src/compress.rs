use anyhow::Result;
use flate2::read::{ZlibDecoder, ZlibEncoder};
use flate2::Compression;
use std::io::{Read, Cursor};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CompressAlgo {
    None,
    Zlib,
    Zstd,
}

impl CompressAlgo {
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "yes" | "on" | "true" => CompressAlgo::Zstd,
            "no" | "off" | "false" => CompressAlgo::None,
            "auto" => CompressAlgo::Zstd,
            _ => CompressAlgo::Zstd,
        }
    }
    
    pub fn to_str(&self) -> &'static str {
        match self {
            CompressAlgo::None => "none",
            CompressAlgo::Zlib => "zlib",
            CompressAlgo::Zstd => "zstd",
        }
    }
}

pub fn compress_data(data: &[u8], compress_type: CompressAlgo) -> Result<Vec<u8>> {
    match compress_type {
        CompressAlgo::None => Ok(data.to_vec()),
        CompressAlgo::Zlib => {
            let mut encoder = ZlibEncoder::new(Cursor::new(data), Compression::default());
            let mut compressed = Vec::new();
            encoder.read_to_end(&mut compressed)?;
            Ok(compressed)
        }
        CompressAlgo::Zstd => {
            let compressed = zstd::encode_all(data, 3)?;
            Ok(compressed)
        }
    }
}

pub fn compress_if_profitable(data: &[u8], compress_type: CompressAlgo) -> Result<(Vec<u8>, bool)> {
    match compress_type {
        CompressAlgo::None => Ok((data.to_vec(), false)),
        _ => {
            let compressed = compress_data(data, compress_type)?;
            // Only use compression if it actually reduces size (profitability check)
            if compressed.len() < data.len() {
                Ok((compressed, true))
            } else {
                Ok((data.to_vec(), false))
            }
        }
    }
}

pub fn decompress_data(data: &[u8], compress_type: CompressAlgo) -> Result<Vec<u8>> {
    match compress_type {
        CompressAlgo::None => Ok(data.to_vec()),
        CompressAlgo::Zlib => {
            let mut decoder = ZlibDecoder::new(Cursor::new(data));
            let mut decompressed = Vec::new();
            decoder.read_to_end(&mut decompressed)?;
            Ok(decompressed)
        }
        CompressAlgo::Zstd => {
            let decompressed = zstd::decode_all(data)?;
            Ok(decompressed)
        }
    }
}

pub fn detect_compress_type(data: &[u8]) -> CompressAlgo {
    if data.len() < 2 {
        return CompressAlgo::None;
    }
    
    // Check for zstd magic number (0xFD2FB528)
    if data.len() >= 4 && data[0] == 0x28 && data[1] == 0xb5 && data[2] == 0x2f && data[3] == 0xfd {
        return CompressAlgo::Zstd;
    }
    
    // Check for zlib header (first byte 0x78)
    if data[0] == 0x78 && (data[1] == 0x01 || data[1] == 0x5e || data[1] == 0x9c || data[1] == 0xda) {
        return CompressAlgo::Zlib;
    }
    
    CompressAlgo::None
}

/// Detect if compression would be profitable by sampling the data
/// Returns true if compression is profitable (compressed < original)
pub fn detect_compression_profitability(data: &[u8], algo: &CompressAlgo) -> bool {
    if data.is_empty() || *algo == CompressAlgo::None {
        return false;
    }
    
    // Sample up to 3 blocks of 128KB
    let block_size = 128 * 1024;
    let num_blocks = 3.min((data.len() / block_size).max(1));
    
    let mut total_original = 0;
    let mut total_compressed = 0;
    
    for i in 0..num_blocks {
        let offset = i * block_size;
        let end = (offset + block_size).min(data.len());
        let block = &data[offset..end];
        
        if let Ok(compressed) = compress_data(block, *algo) {
            total_original += block.len();
            total_compressed += compressed.len();
        }
    }
    
    total_compressed < total_original
}