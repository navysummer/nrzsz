use std::io::{Read, Write};
use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use anyhow::Result;
use tokio::sync::mpsc;

use crate::protocol::Config;
use crate::utils::*;
use crate::progress::TransferProgress;
use crate::compress::{CompressAlgo, compress_data, decompress_data};

pub struct PipelineTransfer {
    config: Config,
    stdin: Box<dyn Read + Send>,
    stdout: Box<dyn Write + Send>,
    stopped: Arc<AtomicBool>,
    remote_supports_compress: bool,
}

impl PipelineTransfer {
    pub fn new(
        config: Config,
        stdin: Box<dyn Read + Send>,
        stdout: Box<dyn Write + Send>,
        stopped: Arc<AtomicBool>,
    ) -> Self {
        Self {
            config,
            stdin,
            stdout,
            stopped,
            remote_supports_compress: true, // Default to true
        }
    }
    
    pub fn set_remote_supports_compress(&mut self, supports: bool) {
        self.remote_supports_compress = supports;
    }

    pub fn send_file_data_pipeline(
        &mut self,
        file_path: &Path,
        size: u64,
        progress: &TransferProgress,
    ) -> Result<Vec<u8>> {
        // Check if remote supports compression before using it
        let compress_type = if self.remote_supports_compress {
            match &self.config.compress {
                crate::protocol::CompressType::Auto => CompressAlgo::Zstd,
                crate::protocol::CompressType::Yes => CompressAlgo::Zstd,
                crate::protocol::CompressType::No => CompressAlgo::None,
            }
        } else {
            CompressAlgo::None
        };
        let chunk_size = self.config.max_buf_size.min(1024 * 1024);

        let (data_tx, mut data_rx) = mpsc::channel::<Vec<u8>>(10);
        let (compressed_tx, mut compressed_rx) = mpsc::channel::<Vec<u8>>(10);

        let stopped = self.stopped.clone();

        let file_path_clone = file_path.to_path_buf();
        tokio::spawn(async move {
            let mut file = File::open(&file_path_clone).unwrap();
            let mut buf = vec![0u8; chunk_size];
            let mut sent = 0u64;

            while sent < size && !stopped.load(Ordering::SeqCst) {
                match file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(bytes_read) => {
                        let data = buf[..bytes_read].to_vec();
                        if data_tx.send(data).await.is_err() {
                            break;
                        }
                        sent += bytes_read as u64;
                    }
                    Err(_) => break,
                }
            }
        });

        let compress_type_clone = compress_type;
        tokio::spawn(async move {
            while let Some(data) = data_rx.recv().await {
                let compressed = if compress_type_clone != CompressAlgo::None {
                    compress_data(&data, compress_type_clone).unwrap_or(data)
                } else {
                    data
                };
                if compressed_tx.send(compressed).await.is_err() {
                    break;
                }
            }
        });

        let mut md5_hash = md5::Context::new();
        let mut total_sent = 0u64;

        while let Some(compressed_data) = compressed_rx.blocking_recv() {
            if self.stopped.load(Ordering::SeqCst) {
                break;
            }

            md5_hash.consume(&compressed_data);

            if self.config.binary {
                let escaped = escape_data(&compressed_data, &self.config.escape_chars);
                send_line(&mut self.stdout, "DATA", &escaped.len().to_string(), &self.config.newline)?;
                self.stdout.write_all(&escaped)?;
            } else {
                send_binary(&mut self.stdout, "DATA", &compressed_data, &self.config.newline)?;
            }

            check_integer(&mut self.stdin, compressed_data.len() as i64, &mut Vec::new())?;

            total_sent += compressed_data.len() as u64;
            progress.update(total_sent);
        }

        Ok(md5_hash.compute().to_vec())
    }

    pub fn recv_file_data_pipeline(
        &mut self,
        file: &mut File,
        _size: u64,
        progress: &TransferProgress,
    ) -> Result<Vec<u8>> {
        // Check if remote supports compression before using it
        let compress_type = if self.remote_supports_compress {
            match &self.config.compress {
                crate::protocol::CompressType::Auto => CompressAlgo::Zstd,
                crate::protocol::CompressType::Yes => CompressAlgo::Zstd,
                crate::protocol::CompressType::No => CompressAlgo::None,
            }
        } else {
            CompressAlgo::None
        };
        let mut current_compress_type = compress_type;
        let mut first_chunk = true;

        let mut md5_hash = md5::Context::new();
        let mut total_received = 0u64;

        loop {
            if self.stopped.load(Ordering::SeqCst) {
                break;
            }

            let compressed_data = if self.config.binary {
                let mut buf = Vec::new();
                let line = match recv_line(&mut self.stdin, &mut buf) {
                    Ok(l) => l,
                    Err(_) => break,
                };
                let idx = match line.find(':') {
                    Some(idx) => idx,
                    None => continue,
                };
                let prefix = &line[1..idx];
                if prefix != "DATA" {
                    if prefix == "EXIT" || prefix == "FAIL" {
                        break;
                    }
                    continue;
                }
                let len = match line[idx+1..].trim().parse::<usize>() {
                    Ok(len) => len,
                    Err(_) => continue,
                };
                let mut data_buf = vec![0u8; len];
                if self.stdin.read_exact(&mut data_buf).is_err() {
                    break;
                }
                unescape_data(&data_buf, &self.config.escape_chars)
            } else {
                match recv_binary(&mut self.stdin, "DATA", &mut Vec::new()) {
                    Ok(data) => data,
                    Err(_) => break,
                }
            };

            if first_chunk && current_compress_type == CompressAlgo::None {
                current_compress_type = crate::compress::detect_compress_type(&compressed_data);
                first_chunk = false;
            }

            let data = if current_compress_type != CompressAlgo::None {
                decompress_data(&compressed_data, current_compress_type)?
            } else {
                compressed_data.clone()
            };

            file.write_all(&data)?;
            md5_hash.consume(&data);

            send_integer(&mut self.stdout, "SUCC", compressed_data.len() as i64, &self.config.newline)?;

            total_received += data.len() as u64;
            progress.update(total_received);
        }

        Ok(md5_hash.compute().to_vec())
    }
}
