//! GRUB bootloader implementation
//!
//! This module provides access to GRUB bootloader environment variables
//! using the `grub-editenv` command.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::bootloader::{
    Bootloader, FsckRecord, Result,
    types::{decode_fsck_output, encode_fsck_output},
};
use crate::error::BootloaderError;
use crate::filesystem::FsckExitCode;
use crate::partition::PartitionName;

/// Command name for GRUB environment manipulation
const GRUB_EDITENV_CMD: &str = "/bin/grub-editenv";

/// Path to the boot partition mount point
const BOOT_DIR_PATH: &str = "/rootfs/boot";

/// Absolute path to the grubenv file
const GRUBENV_PATH: &str = "/rootfs/boot/EFI/BOOT/grubenv";

/// grubenv key used for boot partition fsck status
const BOOT_FSCK_VAR: &str = "omnect_fsck_boot";

/// Constructs the fsck status file path for a non-boot partition on the boot volume.
fn fsck_file_path(partition: PartitionName) -> std::path::PathBuf {
    Path::new(BOOT_DIR_PATH).join(format!("fsck.{partition}"))
}

fn save_fsck_to_file(partition: PartitionName, encoded: &str) -> crate::bootloader::Result<()> {
    let file_path = fsck_file_path(partition);
    fs::write(&file_path, encoded).map_err(|e| BootloaderError::CommandFailed {
        command: format!("write {}", file_path.display()),
        reason: e.to_string(),
    })
}

fn get_fsck_from_file(
    partition: PartitionName,
) -> crate::bootloader::Result<Option<FsckRecord>> {
    let file_path = fsck_file_path(partition);
    if !file_path.is_file() {
        return Ok(None);
    }
    let encoded =
        fs::read_to_string(&file_path).map_err(|e| BootloaderError::CommandFailed {
            command: format!("read {}", file_path.display()),
            reason: e.to_string(),
        })?;
    // Remove file after reading — matches legacy behaviour
    if let Err(e) = fs::remove_file(&file_path) {
        log::warn!(
            "Failed to remove fsck status file {}: {}",
            file_path.display(),
            e
        );
    }
    Ok(decode_fsck_output(&encoded))
}

fn clear_fsck_file(partition: PartitionName) -> crate::bootloader::Result<()> {
    let file_path = fsck_file_path(partition);
    if file_path.exists() {
        fs::remove_file(&file_path).map_err(|e| BootloaderError::CommandFailed {
            command: format!("remove {}", file_path.display()),
            reason: e.to_string(),
        })?;
    }
    Ok(())
}

/// GRUB bootloader implementation
///
/// Uses `grub-editenv` to read/write environment variables from the grubenv file.
pub struct GrubBootloader;

impl GrubBootloader {
    /// Create a new GRUB bootloader instance.
    ///
    /// # Errors
    /// Returns an error if the grubenv file doesn't exist (indicates a corrupted
    /// boot partition, not a missing file on first boot).
    pub fn new() -> Result<Self> {
        if !Path::new(GRUBENV_PATH).is_file() {
            return Err(BootloaderError::EnvFileNotFound {
                path: GRUBENV_PATH.into(),
            });
        }

        Ok(Self)
    }

    /// Run grub-editenv with the given arguments
    fn run_grub_editenv(&self, args: &[&str]) -> Result<String> {
        let output = Command::new(GRUB_EDITENV_CMD)
            .arg(GRUBENV_PATH)
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

    fn save_fsck_status(
        &mut self,
        partition: PartitionName,
        code: FsckExitCode,
        output: &str,
    ) -> Result<()> {
        let encoded = encode_fsck_output(code.bits(), output);

        match partition {
            PartitionName::Boot => {
                // When the boot partition's own fsck requests a reboot, writing to
                // grubenv is unreliable — the filesystem is in an inconsistent state.
                // Match legacy behaviour and skip; a clean check runs on next boot.
                if code.is_reboot_required() {
                    log::warn!(
                        "Skipping grubenv write for boot partition (fsck exit code {code} — reboot required)"
                    );
                    return Ok(());
                }
                self.set_env(BOOT_FSCK_VAR, Some(&encoded))
            }
            PartitionName::RootA
            | PartitionName::RootB
            | PartitionName::RootCurrent
            | PartitionName::Factory
            | PartitionName::Cert
            | PartitionName::Etc
            | PartitionName::Data => {
                // Non-boot partitions: write diagnostic to a file on the boot partition
                // instead of grubenv. grubenv is a fixed 1024-byte block — storing multiple
                // large encoded blobs there would overflow it. Boot is healthy at this point
                // (its own fsck ran first), so this write is safe regardless of this
                // partition's exit code. Matches legacy bash behaviour.
                save_fsck_to_file(partition, &encoded)
            }
            #[cfg(feature = "dos")]
            PartitionName::Extended => save_fsck_to_file(partition, &encoded),
        }
    }

    fn get_fsck_status(&self, partition: PartitionName) -> Result<Option<FsckRecord>> {
        match partition {
            PartitionName::Boot => Ok(self
                .get_env(BOOT_FSCK_VAR)?
                .and_then(|v| decode_fsck_output(&v))),
            PartitionName::RootA
            | PartitionName::RootB
            | PartitionName::RootCurrent
            | PartitionName::Factory
            | PartitionName::Cert
            | PartitionName::Etc
            | PartitionName::Data => get_fsck_from_file(partition),
            #[cfg(feature = "dos")]
            PartitionName::Extended => get_fsck_from_file(partition),
        }
    }

    fn clear_fsck_status(&mut self, partition: PartitionName) -> Result<()> {
        match partition {
            PartitionName::Boot => self.set_env(BOOT_FSCK_VAR, None),
            PartitionName::RootA
            | PartitionName::RootB
            | PartitionName::RootCurrent
            | PartitionName::Factory
            | PartitionName::Cert
            | PartitionName::Etc
            | PartitionName::Data => clear_fsck_file(partition),
            #[cfg(feature = "dos")]
            PartitionName::Extended => clear_fsck_file(partition),
        }
    }
}
