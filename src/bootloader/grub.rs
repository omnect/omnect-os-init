//! GRUB bootloader implementation
//!
//! This module provides access to GRUB bootloader environment variables
//! using the `grub-editenv` command.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::bootloader::{
    Bootloader, Result,
    types::{decode_fsck_output, encode_fsck_output},
};
use crate::error::BootloaderError;

/// Command name for GRUB environment manipulation
const GRUB_EDITENV_CMD: &str = "/bin/grub-editenv";

/// Path to grubenv file relative to boot partition
const GRUBENV_RELATIVE_PATH: &str = "EFI/BOOT/grubenv";

/// grubenv key used for boot partition fsck status
const BOOT_FSCK_VAR: &str = "omnect_fsck_boot";

/// fsck exit code 2: fsck requests a reboot (filesystem still in inconsistent state)
const FSCK_REBOOT_REQUESTED: i32 = 2;

/// GRUB bootloader implementation
///
/// Uses `grub-editenv` to read/write environment variables from the grubenv file.
pub struct GrubBootloader {
    grubenv_path: PathBuf,
    /// Mount point of the boot partition (parent of EFI/BOOT/grubenv)
    boot_dir: PathBuf,
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

        // boot_dir = rootfs/boot (3 levels above grubenv)
        let boot_dir = rootfs_dir.join("boot");

        Ok(Self {
            grubenv_path,
            boot_dir,
        })
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
        let encoded = encode_fsck_output(code, output);

        if partition == "boot" {
            // When code==2, fsck requests a reboot because the boot partition itself
            // is in an inconsistent state. Attempting to write to it at this point
            // is unreliable — match legacy bash behaviour and skip.
            if code == FSCK_REBOOT_REQUESTED {
                log::warn!(
                    "Skipping fsck status save for boot partition (code 2 — reboot requested)"
                );
                return Ok(());
            }
            self.set_env(BOOT_FSCK_VAR, Some(&encoded))
        } else {
            // For non-boot partitions: write to a file on the boot partition instead
            // of grubenv. grubenv is a fixed 1024-byte block — storing multiple large
            // encoded blobs there would overflow it. Matches legacy bash behaviour.
            let file_path = self.boot_dir.join(format!("fsck.{partition}"));
            fs::write(&file_path, &encoded).map_err(|e| BootloaderError::CommandFailed {
                command: format!("write {}", file_path.display()),
                reason: e.to_string(),
            })
        }
    }

    fn get_fsck_status(&self, partition: &str) -> Result<Option<(i32, String)>> {
        if partition == "boot" {
            Ok(self
                .get_env(BOOT_FSCK_VAR)?
                .and_then(|v| decode_fsck_output(&v)))
        } else {
            let file_path = self.boot_dir.join(format!("fsck.{partition}"));
            if !file_path.is_file() {
                return Ok(None);
            }
            let encoded =
                fs::read_to_string(&file_path).map_err(|e| BootloaderError::CommandFailed {
                    command: format!("read {}", file_path.display()),
                    reason: e.to_string(),
                })?;
            // Remove file after reading — matches legacy behaviour
            let _ = fs::remove_file(&file_path);
            Ok(decode_fsck_output(&encoded))
        }
    }

    fn clear_fsck_status(&mut self, partition: &str) -> Result<()> {
        if partition == "boot" {
            self.set_env(BOOT_FSCK_VAR, None)
        } else {
            let file_path = self.boot_dir.join(format!("fsck.{partition}"));
            if file_path.exists() {
                fs::remove_file(&file_path).map_err(|e| BootloaderError::CommandFailed {
                    command: format!("remove {}", file_path.display()),
                    reason: e.to_string(),
                })?;
            }
            Ok(())
        }
    }
}

