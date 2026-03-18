//! GRUB bootloader implementation
//!
//! This module provides access to GRUB bootloader environment variables
//! using the `grub-editenv` command.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::bootloader::types::{decode_fsck_output, encode_fsck_output};
use crate::bootloader::{Bootloader, BootloaderType, FSCK_VAR_PREFIX, Result};
use crate::error::BootloaderError;

/// Command name for GRUB environment manipulation
const GRUB_EDITENV_CMD: &str = "/bin/grub-editenv";

/// Path to grubenv file relative to boot partition
const GRUBENV_RELATIVE_PATH: &str = "EFI/BOOT/grubenv";

/// GRUB bootloader implementation
///
/// Uses `grub-editenv` to read/write environment variables from the grubenv file.
pub struct GrubBootloader {
    grubenv_path: PathBuf,
}

impl GrubBootloader {
    /// Create a new GRUB bootloader instance
    ///
    /// # Arguments
    /// * `rootfs_dir` - Path to the mounted rootfs (e.g., `/rootfs`)
    ///
    /// # Errors
    /// Returns an error if the grubenv file doesn't exist
    pub fn new(rootfs_dir: &Path) -> Result<Self> {
        let grubenv_path = rootfs_dir.join("boot").join(GRUBENV_RELATIVE_PATH);

        if !grubenv_path.is_file() {
            return Err(BootloaderError::EnvFileNotFound { path: grubenv_path });
        }

        Ok(Self { grubenv_path })
    }

    /// Run grub-editenv with the given arguments
    fn run_grub_editenv(&self, args: &[&str]) -> Result<String> {
        let output = Command::new(GRUB_EDITENV_CMD)
            .arg(&self.grubenv_path)
            .args(args)
            .output()
            .map_err(|e| BootloaderError::CommandFailed {
                command: GRUB_EDITENV_CMD.to_string(),
                reason: e.to_string(),
            })?;

        if !output.status.success() {
            return Err(BootloaderError::CommandExitCode {
                command: GRUB_EDITENV_CMD.to_string(),
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

impl Bootloader for GrubBootloader {
    fn get_env(&self, key: &str) -> Result<Option<String>> {
        let output = self.run_grub_editenv(&["list"])?;

        for line in output.lines() {
            if let Some((k, v)) = line.split_once('=')
                && k == key
            {
                return Ok(Some(v.to_string()));
            }
        }

        Ok(None)
    }

    fn set_env(&mut self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(v) => {
                let assignment = format!("{}={}", key, v);
                self.run_grub_editenv(&["set", &assignment])?;
            }
            None => {
                self.run_grub_editenv(&["unset", key])?;
            }
        }
        Ok(())
    }

    fn save_fsck_status(&mut self, partition: &str, code: i32, output: &str) -> Result<()> {
        let var_name = format!("{}{}", FSCK_VAR_PREFIX, partition);
        self.set_env(&var_name, Some(&encode_fsck_output(code, output)))
    }

    fn get_fsck_status(&self, partition: &str) -> Result<Option<(i32, String)>> {
        let var_name = format!("{}{}", FSCK_VAR_PREFIX, partition);
        Ok(self
            .get_env(&var_name)?
            .and_then(|v| decode_fsck_output(&v)))
    }

    fn clear_fsck_status(&mut self, partition: &str) -> Result<()> {
        let var_name = format!("{}{}", FSCK_VAR_PREFIX, partition);
        self.set_env(&var_name, None)
    }

    fn bootloader_type(&self) -> BootloaderType {
        BootloaderType::Grub
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grubenv_path_construction() {
        let rootfs = PathBuf::from("/rootfs");
        let expected = PathBuf::from("/rootfs/boot/EFI/BOOT/grubenv");

        // Can't actually test new() without the file existing
        // but we can verify the path construction logic
        let path = rootfs.join("boot").join(GRUBENV_RELATIVE_PATH);
        assert_eq!(path, expected);
    }
}
