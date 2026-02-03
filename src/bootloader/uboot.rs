//! U-Boot bootloader implementation
//!
//! This module provides access to U-Boot bootloader environment variables
//! using `fw_printenv` and `fw_setenv` commands.

use std::io::{Read, Write};
use std::process::Command;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

use crate::bootloader::{Bootloader, BootloaderType, FSCK_VAR_PREFIX, Result};
use crate::error::BootloaderError;

/// Command to read U-Boot environment variables
const FW_PRINTENV_CMD: &str = "fw_printenv";

/// Command to write U-Boot environment variables
const FW_SETENV_CMD: &str = "fw_setenv";

/// Compression level for fsck output (balance between size and speed)
const COMPRESSION_LEVEL: u32 = 6;

/// U-Boot bootloader implementation
///
/// Uses `fw_printenv` and `fw_setenv` to access environment variables.
/// Fsck status is compressed (gzip) and base64 encoded to fit in the
/// limited U-Boot environment space.
pub struct UBootBootloader {
    // No state needed - commands access environment directly
}

impl UBootBootloader {
    /// Create a new U-Boot bootloader instance
    pub fn new() -> Result<Self> {
        Ok(Self {})
    }

    /// Compress and base64 encode data for storage
    fn compress_and_encode(data: &str) -> Result<String> {
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
    fn decode_and_decompress(encoded: &str) -> Result<String> {
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

    /// Run fw_printenv to get a variable
    fn run_fw_printenv(&self, var: &str) -> Result<Option<String>> {
        let output = Command::new(FW_PRINTENV_CMD)
            .arg("-n")
            .arg(var)
            .output()
            .map_err(|e| BootloaderError::CommandFailed {
                command: FW_PRINTENV_CMD.to_string(),
                reason: e.to_string(),
            })?;

        // Exit code 1 typically means variable not found
        if !output.status.success() {
            return Ok(None);
        }

        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if value.is_empty() {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }

    /// Run fw_setenv to set or unset a variable
    fn run_fw_setenv(&self, var: &str, value: Option<&str>) -> Result<()> {
        let mut cmd = Command::new(FW_SETENV_CMD);
        cmd.arg(var);

        if let Some(v) = value {
            cmd.arg(v);
        }

        let output = cmd.output().map_err(|e| BootloaderError::CommandFailed {
            command: FW_SETENV_CMD.to_string(),
            reason: e.to_string(),
        })?;

        if !output.status.success() {
            return Err(BootloaderError::CommandExitCode {
                command: FW_SETENV_CMD.to_string(),
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        Ok(())
    }
}

impl Bootloader for UBootBootloader {
    fn get_env(&self, key: &str) -> Result<Option<String>> {
        self.run_fw_printenv(key)
    }

    fn set_env(&mut self, key: &str, value: Option<&str>) -> Result<()> {
        self.run_fw_setenv(key, value)
    }

    fn save_fsck_status(&mut self, partition: &str, output: &str, code: i32) -> Result<()> {
        let var_name = format!("{}{}", FSCK_VAR_PREFIX, partition);
        let value = format!("{}:{}", code, output);
        let encoded = Self::compress_and_encode(&value)?;
        self.run_fw_setenv(&var_name, Some(&encoded))
    }

    fn get_fsck_status(&self, partition: &str) -> Result<Option<String>> {
        let var_name = format!("{}{}", FSCK_VAR_PREFIX, partition);
        match self.run_fw_printenv(&var_name)? {
            Some(encoded) => Ok(Some(Self::decode_and_decompress(&encoded)?)),
            None => Ok(None),
        }
    }

    fn clear_fsck_status(&mut self, partition: &str) -> Result<()> {
        let var_name = format!("{}{}", FSCK_VAR_PREFIX, partition);
        self.run_fw_setenv(&var_name, None)
    }

    fn bootloader_type(&self) -> BootloaderType {
        BootloaderType::UBoot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_decompress_roundtrip() {
        let original = "fsck from util-linux 2.37.2\n/dev/sda1: clean, 100/1000 files";

        let encoded = UBootBootloader::compress_and_encode(original).unwrap();
        let decoded = UBootBootloader::decode_and_decompress(&encoded).unwrap();

        assert_eq!(original, decoded);
    }

    #[test]
    fn test_compress_reduces_size() {
        let original = "a]".repeat(1000);

        let encoded = UBootBootloader::compress_and_encode(&original).unwrap();

        // Compressed + base64 should still be smaller than original for repetitive data
        assert!(encoded.len() < original.len());
    }
}
