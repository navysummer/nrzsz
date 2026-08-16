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