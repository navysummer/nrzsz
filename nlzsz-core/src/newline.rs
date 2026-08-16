
pub struct WindowsNewline {
    use_newline_protocol: bool,
}

impl WindowsNewline {
    pub fn new() -> Self {
        Self {
            use_newline_protocol: cfg!(windows),
        }
    }
    
    pub fn encode(&self, data: &[u8]) -> Vec<u8> {
        if !self.use_newline_protocol {
            return data.to_vec();
        }
        
        let mut encoded = Vec::with_capacity(data.len());
        let mut i = 0;
        
        while i < data.len() {
            if data[i] == b'\n' {
                // Check if this is already a \r\n
                if i > 0 && data[i - 1] == b'\r' {
                    // Already \r\n, keep as is
                    encoded.push(b'\r');
                    encoded.push(b'\n');
                } else {
                    // Just \n, encode as !\n
                    encoded.push(b'!');
                    encoded.push(b'\n');
                }
            } else if data[i] == b'\r' {
                // Check if this is \r\n
                if i + 1 < data.len() && data[i + 1] == b'\n' {
                    // \r\n, encode as !\n
                    encoded.push(b'!');
                    encoded.push(b'\n');
                    i += 1; // Skip the \n
                } else {
                    // Just \r
                    encoded.push(b'\r');
                }
            } else {
                encoded.push(data[i]);
            }
            
            i += 1;
        }
        
        encoded
    }
    
    pub fn decode(&self, data: &[u8]) -> Vec<u8> {
        if !self.use_newline_protocol {
            return data.to_vec();
        }
        
        let mut decoded = Vec::with_capacity(data.len());
        let mut i = 0;
        
        while i < data.len() {
            if data[i] == b'!' && i + 1 < data.len() && data[i + 1] == b'\n' {
                // Encoded newline: !\n -> \r\n (Windows style)
                decoded.push(b'\r');
                decoded.push(b'\n');
                i += 2;
            } else {
                decoded.push(data[i]);
                i += 1;
            }
        }
        
        decoded
    }
    
    pub fn is_newline_protocol(&self) -> bool {
        self.use_newline_protocol
    }
    
    pub fn set_newline_protocol(&mut self, use_protocol: bool) {
        self.use_newline_protocol = use_protocol;
    }
    
    pub fn detect_platform() -> bool {
        cfg!(windows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_windows_newline_creation() {
        let newline = WindowsNewline::new();
        // On non-Windows, should be false
        assert_eq!(newline.is_newline_protocol(), cfg!(windows));
    }
    
    #[test]
    fn test_encode_decode() {
        let mut newline = WindowsNewline::new();
        newline.set_newline_protocol(true);
        
        let data = b"hello\nworld\r\nfoo\rbar";
        let encoded = newline.encode(data);
        let decoded = newline.decode(&encoded);
        
        // The decoded output should have Windows-style newlines
        let expected = b"hello\r\nworld\r\nfoo\rbar";
        assert_eq!(expected.to_vec(), decoded);
    }
    
    #[test]
    fn test_encode_no_protocol() {
        let newline = WindowsNewline::new();
        
        let data = b"hello\nworld";
        let encoded = newline.encode(data);
        
        assert_eq!(data.to_vec(), encoded);
    }
}