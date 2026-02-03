//! U-Boot bootloader implementation
//!
//! This module provides bootloader environment access for U-Boot-based systems
//! (typically ARM). It uses the `fw_printenv` and `fw_setenv` commands to read
//! and write environment variables.
//!
//! Note: A future improvement could use libubootenv bindings for direct access.

use super::types::{compress_and_encode, decode_and_decompress, BootloaderType};
use super::Bootloader;
use crate::error::{BootloaderError, Result};
use std::process::Command;

/// U-Boot bootloader implementation
pub struct UBootBootloader {
    // No state needed - fw_printenv/fw_setenv handle everything
}

impl UBootBootloader {
    /// Create a new UBootBootloader instance
    pub fn new() -> Result<Self> {
        // Verify fw_printenv is available (suppress output)
        let output = Command::new("which")
            .arg("fw_printenv")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| BootloaderError::CommandFailed {
                command: "which fw_printenv".to_string(),
                reason: e.to_string(),
            })?;

        if !output.success() {
            log::warn!("fw_printenv not found in PATH, U-Boot operations may fail");
        }

        Ok(Self {})
    }

    /// Run fw_printenv to get a variable
    fn run_fw_printenv(&self, var_name: &str) -> Result<Option<String>> {
        let output = Command::new("fw_printenv")
            .arg("-n")
            .arg(var_name)
            .output()
            .map_err(|e| BootloaderError::CommandFailed {
                command: "fw_printenv".to_string(),
                reason: e.to_string(),
            })?;

        if !output.status.success() {
            // fw_printenv returns non-zero if variable doesn't exist
            return Ok(None);
        }

        let value = String::from_utf8_lossy(&output.stdout)
            .trim()
            .to_string();

        if value.is_empty() {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }

    /// Run fw_setenv to set or delete a variable
    fn run_fw_setenv(&self, var_name: &str, value: Option<&str>) -> Result<()> {
        let status = match value {
            Some(v) => Command::new("fw_setenv")
                .arg(var_name)
                .arg(v)
                .status(),
            None => Command::new("fw_setenv")
                .arg(var_name)
                .status(),
        };

        let status = status.map_err(|e| BootloaderError::CommandFailed {
            command: "fw_setenv".to_string(),
            reason: e.to_string(),
        })?;

        if !status.success() {
            return Err(BootloaderError::WriteFailed {
                name: var_name.to_string(),
                reason: format!("fw_setenv returned exit code {}", status.code().unwrap_or(-1)),
            }
            .into());
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

    fn save_fsck_status(&mut self, partition: &str, output: &str, _code: i32) -> Result<()> {
        let key = format!("omnect_fsck_{}", partition);
        let encoded = compress_and_encode(output)?;
        self.run_fw_setenv(&key, Some(&encoded))
    }

    fn get_fsck_status(&self, partition: &str) -> Result<Option<String>> {
        let key = format!("omnect_fsck_{}", partition);
        
        match self.run_fw_printenv(&key)? {
            Some(encoded) => Ok(Some(decode_and_decompress(&encoded)?)),
            None => Ok(None),
        }
    }

    fn clear_fsck_status(&mut self, partition: &str) -> Result<()> {
        let key = format!("omnect_fsck_{}", partition);
        self.run_fw_setenv(&key, None)
    }

    fn bootloader_type(&self) -> BootloaderType {
        BootloaderType::UBoot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require fw_printenv/fw_setenv to be available
    // They are disabled by default and run only in integration tests

    #[test]
    fn test_uboot_bootloader_type() {
        // This test doesn't require the actual commands
        let bl = UBootBootloader {};
        assert_eq!(bl.bootloader_type(), BootloaderType::UBoot);
    }

    #[test]
    fn test_fsck_key_format() {
        // Verify the key format matches the bash script
        let partition = "data";
        let expected_key = "omnect_fsck_data";
        let actual_key = format!("omnect_fsck_{}", partition);
        assert_eq!(actual_key, expected_key);
    }
}
