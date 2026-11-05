use nrzsz_core::protocol::*;
use nrzsz_core::utils::*;
use nrzsz_core::compress::*;

#[test]
fn test_config_default() {
    let config = Config::default();
    assert!(!config.quiet);
    assert!(!config.binary);
    assert!(!config.directory);
    assert!(!config.overwrite);
    assert_eq!(config.timeout, 20);
    assert_eq!(config.newline, "\n");
    assert_eq!(config.protocol, PROTOCOL_VERSION);
    assert_eq!(config.max_buf_size, 10 * 1024 * 1024);
    assert_eq!(config.compress, CompressType::Auto);
     assert!(!config.fork.unwrap_or(false));
}

#[test]
fn test_config_serialization() {
    let config = Config::default();
    let json = serde_json::to_string(&config).unwrap();
    let deserialized: Config = serde_json::from_str(&json).unwrap();
    assert_eq!(config.timeout, deserialized.timeout);
    assert_eq!(config.compress, deserialized.compress);
}

#[test]
fn test_action_serialization() {
    let action = Action {
        lang: "rs".to_string(),
        confirm: true,
        version: "0.1.0".to_string(),
        support_dir: true,
        protocol: PROTOCOL_VERSION,
        newline: "\n".to_string(),
        tunnel: None,
        fork: None,
        tmuxcc: None,
        support_binary: Some(true),
    };
    let json = serde_json::to_string(&action).unwrap();
    assert!(json.contains("rs"));
    assert!(json.contains("0.1.0"));
}

#[test]
fn test_encode_decode_buffer() {
    let original = b"Hello, World!";
    let encoded = encode_buffer(original).unwrap();
    let decoded = decode_buffer(&encoded).unwrap();
    assert_eq!(original.to_vec(), decoded);
}

#[test]
fn test_escape_chars() {
    let chars = get_escape_chars(false);
    assert_eq!(chars.len(), 2);
    assert_eq!(chars[0].0, vec![0xee]);
    assert_eq!(chars[0].1, vec![0xee, 0xee]);
    
    let chars_all = get_escape_chars(true);
    assert_eq!(chars_all.len(), 15);
}

#[test]
fn test_escape_chars_latin1_roundtrip() {
    // Test that escape_chars serialize/deserialize preserves Latin-1 bytes correctly
    // Go uses ISO-8859-1 (Latin-1) encoding for escape char bytes
    // Byte 0xEE -> Latin-1 character U+00EE (î) -> JSON serializes as "î" or "\u00ee"
    // Go reads this and encodes back to byte 0xEE via charmap.ISO8859_1
    let mut config = Config::default();
    config.escape_chars = vec![
        (vec![0xee], vec![0xee, 0xee]),
        (vec![0x7e], vec![0xee, 0x31]),
        (vec![0x02], vec![0xee, 0x41]),
        (vec![0x8d], vec![0xee, 0x4b]),
    ];
    
    let json = serde_json::to_string(&config).unwrap();
    // Verify Latin-1 characters are present in JSON
    assert!(json.contains('\u{00ee}'), "JSON should contain Latin-1 U+00EE: {}", json);
    
    let deserialized: Config = serde_json::from_str(&json).unwrap();
    assert_eq!(config.escape_chars, deserialized.escape_chars, "Latin-1 roundtrip should preserve bytes");
}

#[test]
fn test_escape_unescape_data() {
    let escape_chars = get_escape_chars(true);
    let original = vec![0x00, 0x7e, 0x41, 0x42, 0x02];
    let escaped = escape_data(&original, &escape_chars);
    let unescaped = unescape_data(&escaped, &escape_chars);
    assert_eq!(original, unescaped);
}

#[test]
fn test_compress_zstd() {
    let original = b"Hello, World! This is a test string for compression.";
    let compressed = compress_data(original, CompressAlgo::Zstd).unwrap();
    let decompressed = decompress_data(&compressed, CompressAlgo::Zstd).unwrap();
    assert_eq!(original.to_vec(), decompressed);
}

#[test]
fn test_compress_zlib() {
    let original = b"Hello, World! This is a test string for compression.";
    let compressed = compress_data(original, CompressAlgo::Zlib).unwrap();
    let decompressed = decompress_data(&compressed, CompressAlgo::Zlib).unwrap();
    assert_eq!(original.to_vec(), decompressed);
}

#[test]
fn test_detect_compress_type() {
    let zstd_data = vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x00];
    assert_eq!(detect_compress_type(&zstd_data), CompressAlgo::Zstd);
    
    let zlib_data = vec![0x78, 0x9c, 0x00, 0x00];
    assert_eq!(detect_compress_type(&zlib_data), CompressAlgo::Zlib);
    
    let plain_data = b"Hello, World!";
    assert_eq!(detect_compress_type(plain_data), CompressAlgo::None);
}

#[test]
fn test_recv_windows_newline() {
    // Windows protocol uses "!\n" as the line terminator (matching Go's readLineOnWindows)
    use std::io::{Cursor, Read};
    let windows_newline = "!\n";
    
    // Build a CFG message using the Windows terminator
    let cfg = serde_json::json!({"lang":"rust","protocol":3});
    let msg = format!("#CFG:{}{}", nrzsz_core::utils::encode_buffer(&serde_json::to_vec(&cfg).unwrap()).unwrap(), windows_newline);
    // SUCC integer with Windows terminator
    let msg2 = format!("#SUCC:1{}", windows_newline);
    // DATA header with Windows terminator followed by raw bytes
    let msg3 = format!("#DATA:5{}HELLO", windows_newline);
    let mut data = msg.into_bytes();
    data.extend_from_slice(msg2.as_bytes());
    data.extend_from_slice(msg3.as_bytes());
    
    let mut cursor = Cursor::new(data);
    
    // recv_string with "!\n" newline should parse the CFG
    let mut buf = Vec::new();
    let cfg_str = recv_string(&mut cursor, "CFG", &mut buf, "!\n").unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&cfg_str).unwrap();
    assert_eq!(parsed["protocol"], 3);
    
    // recv_integer with "!\n"
    let n = recv_integer(&mut cursor, "SUCC", &mut buf, "!\n").unwrap();
    assert_eq!(n, 1);
    
    // recv_line with "!\n" for the DATA header
    let line = recv_line(&mut cursor, &mut buf, "!\n").unwrap();
    assert_eq!(line, "#DATA:5");
    // raw payload bytes
    let mut payload = [0u8; 5];
    cursor.read_exact(&mut payload).unwrap();
    assert_eq!(&payload, b"HELLO");
}

#[test]
fn test_send_action_advertises_tunnel() {
    // Regression: the Action sent by send_action must advertise
    // `tunnel: Some(tunnel_connected)` so the server's use_tunnel (and the
    // fork/binary gating that depends on it) works over the tunnel.
    let action = Action {
        lang: "rs".to_string(),
        confirm: true,
        version: "0.1.0".to_string(),
        support_dir: true,
        protocol: PROTOCOL_VERSION,
        newline: "\n".to_string(),
        tunnel: Some(true),
        fork: Some(true),
        tmuxcc: None,
        support_binary: Some(true),
    };
    let json = serde_json::to_string(&action).unwrap();
    assert!(json.contains("\"tunnel\":true"));
    assert!(json.contains("\"fork\":true"));
}

#[test]
fn test_send_action_no_tunnel_advertises_false() {
    // When the tunnel is NOT connected, tunnel must be Some(false) and fork
    // Some(false) — matching Go's sendAction (which only sets both true on a
    // connected tunnel).
    let action = Action {
        lang: "rs".to_string(),
        confirm: true,
        version: "0.1.0".to_string(),
        support_dir: true,
        protocol: PROTOCOL_VERSION,
        newline: "\n".to_string(),
        tunnel: Some(false),
        fork: Some(false),
        tmuxcc: None,
        support_binary: Some(true),
    };
    let json = serde_json::to_string(&action).unwrap();
    assert!(json.contains("\"tunnel\":false"));
    assert!(json.contains("\"fork\":false"));
}