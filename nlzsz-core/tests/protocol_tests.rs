use nlzsz_core::protocol::*;
use nlzsz_core::utils::*;
use nlzsz_core::compress::*;

#[test]
fn test_config_default() {
    let config = Config::default();
    assert_eq!(config.lang, "rs");
    assert!(!config.quiet);
    assert!(!config.binary);
    assert!(!config.directory);
    assert!(!config.overwrite);
    assert_eq!(config.timeout, 20);
    assert_eq!(config.newline, "\n");
    assert_eq!(config.protocol, PROTOCOL_VERSION);
    assert_eq!(config.max_buf_size, 10 * 1024 * 1024);
    assert_eq!(config.compress, CompressType::Auto);
    assert!(!config.fork);
}

#[test]
fn test_config_serialization() {
    let config = Config::default();
    let json = serde_json::to_string(&config).unwrap();
    let deserialized: Config = serde_json::from_str(&json).unwrap();
    assert_eq!(config.lang, deserialized.lang);
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
        newline: None,
        binary: None,
        tunnel: None,
        fork: None,
        tmuxcc: None,
        support_binary: Some(true),
        comp: Some(1),
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
    assert_eq!(chars[0].0, vec![0x00, 0xee]);
    assert_eq!(chars[0].1, vec![0x00, 0xee, 0xee]);
    
    let chars_all = get_escape_chars(true);
    assert_eq!(chars_all.len(), 15);
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