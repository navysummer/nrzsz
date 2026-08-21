use std::io::{Read, Write};
use std::fs::{self, File};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use anyhow::Result;

use crate::utils::*;
use crate::compress::{CompressAlgo, compress_data, decompress_data};

/// 3-stage concurrent send pipeline:
/// 1. Read file chunks (thread 1)
/// 2. Compress + escape encode (thread 2)
/// 3. Send + ACK + MD5 (main thread)
#[allow(clippy::too_many_arguments)]
pub fn send_file_data_pipeline(
    stdin: &mut impl Read,
    stdout: &mut impl Write,
    file_path: &Path,
    size: u64,
    progress: &TransferProgress,
    use_compress: bool,
    escape_chars: Vec<(Vec<u8>, Vec<u8>)>,
    newline: &str,
) -> Result<Vec<u8>> {
    let compress_type = if use_compress { CompressAlgo::Zstd } else { CompressAlgo::None };

    let (data_tx, data_rx) = mpsc::sync_channel::<Vec<u8>>(5);
    let (md5_source_tx, md5_source_rx) = mpsc::sync_channel::<Vec<u8>>(5);
    let (encoded_tx, encoded_rx) = mpsc::sync_channel::<Vec<u8>>(5);
    let (md5_tx, md5_rx) = mpsc::channel();

    // Thread 1: Read file
    let fp = file_path.to_path_buf();
    thread::spawn(move || {
        let mut file = match File::open(&fp) {
            Ok(f) => f,
            Err(_) => return,
        };
        let mut buf = vec![0u8; 32 * 1024];
        let mut sent = 0u64;
        while sent < size {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let data = buf[..n].to_vec();
                    if data_tx.send(data.clone()).is_err() {
                        break;
                    }
                    if md5_source_tx.send(data).is_err() {
                        break;
                    }
                    sent += n as u64;
                }
                Err(_) => break,
            }
        }
    });

    // Thread 2: MD5 computation
    thread::spawn(move || {
        let mut hasher = md5::Context::new();
        for data in md5_source_rx {
            hasher.consume(&data);
        }
        md5_tx.send(hasher.compute().to_vec()).ok();
    });

    // Thread 3: Compress + encode
    let ct = compress_type;
    thread::spawn(move || {
        for data in data_rx {
            let compressed = if ct != CompressAlgo::None {
                compress_data(&data, ct).unwrap_or(data)
            } else {
                data
            };
            let escaped = escape_data(&compressed, &escape_chars);
            if encoded_tx.send(escaped).is_err() {
                break;
            }
        }
    });

    // Main thread: Send + ACK
    let mut total_sent = 0u64;
    for encoded in encoded_rx {
        let len = encoded.len();
        let line = format!("#DATA:{}{}", len, newline);
        stdout.write_all(line.as_bytes())?;
        stdout.write_all(&encoded)?;
        stdout.flush()?;

        let _ack_step = check_pipeline_ack(stdin, len as i64, &mut Vec::new(), newline)?;
        total_sent += len as u64;
        progress.update(total_sent);
    }

    Ok(md5_rx.recv().unwrap_or_default())
}

/// 3-stage concurrent receive pipeline:
/// 1. Receive + decode + ACK (main thread)
/// 2. Decompress + write file + MD5 (thread)
#[allow(clippy::too_many_arguments)]
pub fn recv_file_data_pipeline(
    stdin: &mut impl Read,
    stdout: &mut impl Write,
    file: &mut File,
    _size: u64,
    progress: &TransferProgress,
    use_compress: bool,
    binary: bool,
    escape_chars: Vec<(Vec<u8>, Vec<u8>)>,
    newline: &str,
) -> Result<Vec<u8>> {
    let compress_type = if use_compress { CompressAlgo::Zstd } else { CompressAlgo::None };

    let (decoded_tx, decoded_rx) = mpsc::sync_channel::<Vec<u8>>(5);
    let (md5_result_tx, md5_result_rx) = mpsc::channel();

    // Background thread: decompress + write file + MD5
    let tmp_file = tempfile::Builder::new()
        .prefix("nrzsz_pipeline_")
        .tempfile()?;
    let tmp_path = tmp_file.path().to_path_buf();
    let tmp_path_clone = tmp_path.clone();
    thread::spawn(move || {
        let mut out_file = match fs::File::create(&tmp_path_clone) {
            Ok(f) => f,
            Err(_) => return,
        };
let mut hasher = md5::Context::new();
        let mut current_compress = compress_type;
        let mut first_chunk = true;
        for data in decoded_rx {
            if current_compress == CompressAlgo::None || first_chunk {
                if first_chunk {
                    current_compress = crate::compress::detect_compress_type(&data);
                    first_chunk = false;
                }
                if current_compress != CompressAlgo::None {
                    if let Ok(raw) = decompress_data(&data, current_compress) {
                        hasher.consume(&raw);
                        out_file.write_all(&raw).ok();
                    }
                } else {
                    hasher.consume(&data);
                    out_file.write_all(&data).ok();
                }
            } else {
                if let Ok(raw) = decompress_data(&data, current_compress) {
                    hasher.consume(&raw);
                    out_file.write_all(&raw).ok();
                }
            }
        }
        md5_result_tx.send(hasher.compute().to_vec()).ok();
    });

    // Main thread: receive + decode + ACK
    let mut total_received = 0u64;
    loop {
        let compressed_data = if binary {
            let mut buf = Vec::new();
            let line = match recv_line(stdin, &mut buf, newline) {
                Ok(l) => l,
                Err(_) => break,
            };
            let idx = match line.find(':') {
                Some(idx) => idx,
                None => continue,
            };
            let prefix = &line[1..idx];
            if prefix != "DATA" {
                if prefix == "EXIT" || prefix == "FAIL" || prefix == "fail" {
                    break;
                }
                continue;
            }
            let value = line[idx+1..].trim();
            if value == "=" {
                continue;
            }
            let len: usize = match value.parse() {
                Ok(len) => len,
                Err(_) => continue,
            };
            let mut data_buf = vec![0u8; len];
            if stdin.read_exact(&mut data_buf).is_err() {
                break;
            }
            unescape_data(&data_buf, &escape_chars)
        } else {
            match recv_binary(stdin, "DATA", &mut Vec::new(), newline) {
                Ok(data) => data,
                Err(_) => break,
            }
        };

        let len = compressed_data.len() as i64;
        send_pipeline_ack(stdout, len, total_received as i64, newline)?;
        total_received += compressed_data.len() as u64;
        progress.update(total_received);

        if decoded_tx.send(compressed_data).is_err() {
            break;
        }
    }

    // Copy temp file to actual file
    let digest = md5_result_rx.recv().unwrap_or_default();
    let mut temp_file = fs::File::open(&tmp_path)?;
    let mut temp_buf = Vec::new();
    temp_file.read_to_end(&mut temp_buf)?;
    file.write_all(&temp_buf)?;
    let _ = fs::remove_file(&tmp_path);

    Ok(digest)
}

use crate::progress::TransferProgress;