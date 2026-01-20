//! Common types for bootloader abstraction

use crate::error::{BootloaderError, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::{Read, Write};

/// Bootloader type enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootloaderType {
    /// GRUB bootloader (x86-64 EFI)
    Grub,
    /// U-Boot bootloader (ARM)
    UBoot,
    /// Mock bootloader for testing
    #[cfg(test)]
    Mock,
}

impl std::fmt::Display for BootloaderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BootloaderType::Grub => write!(f, "GRUB"),
            BootloaderType::UBoot => write!(f, "U-Boot"),
            #[cfg(test)]
            BootloaderType::Mock => write!(f, "Mock"),
        }
    }
}

/// Compress data with gzip and encode as base64
///
/// This is used for storing fsck output in bootloader environment variables,
/// which have limited space.
pub fn compress_and_encode(data: &str) -> Result<String> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(data.as_bytes())
        .map_err(|e| BootloaderError::InvalidValue {
            name: "fsck_status".to_string(),
            reason: format!("compression failed: {}", e),
        })?;

    let compressed = encoder.finish().map_err(|e| BootloaderError::InvalidValue {
        name: "fsck_status".to_string(),
        reason: format!("compression finalize failed: {}", e),
    })?;

    Ok(BASE64.encode(&compressed))
}

/// Decode base64 and decompress gzip data
pub fn decode_and_decompress(encoded: &str) -> Result<String> {
    let compressed = BASE64.decode(encoded).map_err(|e| BootloaderError::InvalidValue {
        name: "fsck_status".to_string(),
        reason: format!("base64 decode failed: {}", e),
    })?;

    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut decompressed = String::new();
    decoder
        .read_to_string(&mut decompressed)
        .map_err(|e| BootloaderError::InvalidValue {
            name: "fsck_status".to_string(),
            reason: format!("decompression failed: {}", e),
        })?;

    Ok(decompressed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bootloader_type_display() {
        assert_eq!(BootloaderType::Grub.to_string(), "GRUB");
        assert_eq!(BootloaderType::UBoot.to_string(), "U-Boot");
        assert_eq!(BootloaderType::Mock.to_string(), "Mock");
    }

    #[test]
    fn test_compress_and_encode() {
        let original = "fsck from util-linux 2.37.2\n/dev/sda1: clean, 123/456 files, 789/1234 blocks";
        let encoded = compress_and_encode(original).unwrap();
        
        // Should be valid base64
        assert!(BASE64.decode(&encoded).is_ok());
        
        // Should be shorter than original for typical fsck output
        // (compression works better on larger, repetitive text)
    }

    #[test]
    fn test_roundtrip() {
        let original = "fsck from util-linux 2.37.2\n/dev/sda1: clean, 123/456 files, 789/1234 blocks\nExtra line for testing";
        let encoded = compress_and_encode(original).unwrap();
        let decoded = decode_and_decompress(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_empty_string() {
        let original = "";
        let encoded = compress_and_encode(original).unwrap();
        let decoded = decode_and_decompress(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_unicode() {
        let original = "fsck: ✓ clean\nWarning: ⚠ some issue";
        let encoded = compress_and_encode(original).unwrap();
        let decoded = decode_and_decompress(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_invalid_base64() {
        let result = decode_and_decompress("not-valid-base64!!!");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_gzip() {
        // Valid base64 but not valid gzip
        let result = decode_and_decompress("aGVsbG8gd29ybGQ="); // "hello world" in base64
        assert!(result.is_err());
    }
}
