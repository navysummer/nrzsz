use std::io::{self, Read, Write, BufReader, BufWriter};
use std::time::{Duration, Instant};
use anyhow::{Result, anyhow};
use log::debug;

pub struct DynamicBuffer {
    reader: BufReader<Box<dyn Read>>,
    writer: BufWriter<Box<dyn Write>>,
    buffer_size: usize,
    min_buffer_size: usize,
    max_buffer_size: usize,
    timeout: Duration,
    last_chunk_time: Instant,
    max_chunk_time: f64,
}

impl DynamicBuffer {
    pub fn new(
        reader: Box<dyn Read>,
        writer: Box<dyn Write>,
        initial_buffer_size: usize,
        timeout_secs: u32,
    ) -> Self {
        Self {
            reader: BufReader::with_capacity(initial_buffer_size, reader),
            writer: BufWriter::with_capacity(initial_buffer_size, writer),
            buffer_size: initial_buffer_size,
            min_buffer_size: 1024,
            max_buffer_size: 10 * 1024 * 1024, // 10MB
            timeout: Duration::from_secs(timeout_secs as u64),
            last_chunk_time: Instant::now(),
            max_chunk_time: 0.0,
        }
    }
    
    pub fn read_line(&mut self) -> Result<String> {
        let mut line = String::new();
        let start_time = Instant::now();
        
        loop {
            if start_time.elapsed() > self.timeout {
                return Err(anyhow!("Read timeout"));
            }
            
            let mut byte = [0u8; 1];
            match self.reader.read_exact(&mut byte) {
                Ok(()) => {
                    if byte[0] == b'\n' {
                        return Ok(line);
                    }
                    line.push(byte[0] as char);
                }
                Err(e) => {
                    if e.kind() == io::ErrorKind::UnexpectedEof {
                        return Ok(line);
                    }
                    return Err(e.into());
                }
            }
        }
    }
    
    pub fn read_binary(&mut self, size: usize) -> Result<Vec<u8>> {
        let mut buffer = vec![0u8; size];
        let start_time = Instant::now();
        
        if start_time.elapsed() > self.timeout {
            return Err(anyhow!("Read timeout"));
        }
        
        match self.reader.read_exact(&mut buffer) {
            Ok(()) => Ok(buffer),
            Err(e) => {
                if e.kind() == io::ErrorKind::UnexpectedEof {
                    Ok(buffer)
                } else {
                    Err(e.into())
                }
            }
        }
    }
    
    pub fn write_line(&mut self, line: &str) -> Result<()> {
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
    
    pub fn write_binary(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }
    
    pub fn adjust_buffer_size(&mut self, chunk_time: f64) {
        self.last_chunk_time = Instant::now();
        
        if chunk_time > self.max_chunk_time {
            self.max_chunk_time = chunk_time;
        }
        
        // Adaptive buffer sizing based on chunk transfer time
        if chunk_time < 0.5 && self.buffer_size < self.max_buffer_size {
            // Fast transfer, increase buffer size
            self.buffer_size = (self.buffer_size * 2).min(self.max_buffer_size);
            debug!("Buffer size increased to {}", self.buffer_size);
        } else if chunk_time >= 2.0 && self.buffer_size > self.min_buffer_size {
            // Slow transfer, decrease buffer size
            self.buffer_size = (self.buffer_size / 2).max(self.min_buffer_size);
            debug!("Buffer size decreased to {}", self.buffer_size);
        }
    }
    
    pub fn get_buffer_size(&self) -> usize {
        self.buffer_size
    }
    
    pub fn is_timeout(&self) -> bool {
        self.last_chunk_time.elapsed() > self.timeout
    }
}

pub struct TimeoutReader {
    reader: Box<dyn Read>,
    timeout: Duration,
}

impl TimeoutReader {
    pub fn new(reader: Box<dyn Read>, timeout: Duration) -> Self {
        Self { reader, timeout }
    }
    
    pub fn read_with_timeout(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let start_time = Instant::now();
        
        loop {
            if start_time.elapsed() > self.timeout {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "Read timeout"));
            }
            
            match self.reader.read(buf) {
                Ok(n) => return Ok(n),
                Err(e) => {
                    if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut {
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }
}
