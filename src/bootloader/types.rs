//! Common types for bootloader implementations

use std::fmt;
use std::io::{Read, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

use crate::bootloader::Result;
use crate::error::BootloaderError;

/// Compression level for fsck output
const COMPRESSION_LEVEL: u32 = 6;

/// Bootloader type enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootloaderType {
    /// GRUB bootloader (typically x86-64 EFI systems)
    Grub,
    /// U-Boot bootloader (typically ARM systems)
    UBoot,
    /// Mock bootloader for testing
    #[cfg(test)]
    Mock,
}

impl fmt::Display for BootloaderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Grub => write!(f, "GRUB"),
            Self::UBoot => write!(f, "U-Boot"),
            #[cfg(test)]
            Self::Mock => write!(f, "Mock"),
        }
    }
}

/// Compress and base64 encode data for storage
///
/// Used by U-Boot implementation and mock bootloader.
pub fn compress_and_encode(data: &str) -> Result<String> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::new(COMPRESSION_LEVEL));
    encoder
        .write_all(data.as_bytes())
        .map_err(|e| BootloaderError::CompressionFailed(e.to_string()))?;

    let compressed = encoder
        .finish()
        .map_err(|e| BootloaderError::CompressionFailed(e.to_string()))?;

    Ok(BASE64_STANDARD.encode(&compressed))
}

/// Decode and decompress base64-encoded data
///
/// Used by U-Boot implementation and mock bootloader.
pub fn decode_and_decompress(encoded: &str) -> Result<String> {
    let compressed = BASE64_STANDARD
        .decode(encoded)
        .map_err(|e| BootloaderError::DecompressionFailed(e.to_string()))?;

    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut decompressed = String::new();
    decoder
        .read_to_string(&mut decompressed)
        .map_err(|e| BootloaderError::DecompressionFailed(e.to_string()))?;

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
    fn test_compress_decompress_roundtrip() {
        let original = "fsck from util-linux 2.37.2\n/dev/sda1: clean, 100/1000 files";

        let encoded = compress_and_encode(original).unwrap();
        let decoded = decode_and_decompress(&encoded).unwrap();

        assert_eq!(original, decoded);
    }

    #[test]
    fn test_encoded_is_valid_base64() {
        let original = "test data for encoding";
        let encoded = compress_and_encode(original).unwrap();

        assert!(BASE64_STANDARD.decode(&encoded).is_ok());
    }

    #[test]
    fn test_compress_reduces_size_for_repetitive_data() {
        let original = "a".repeat(1000);

        let encoded = compress_and_encode(&original).unwrap();

        // Compressed + base64 should still be smaller than original for repetitive data
        assert!(encoded.len() < original.len());
    }
}
