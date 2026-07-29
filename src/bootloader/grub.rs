//! GRUB boot environment implementation
//!
//! This module provides access to GRUB bootloader environment variables
//! using the `grub-editenv` command.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::bootloader::{
    BootEnv, BootEnvKey, FsckRecord, Result,
    types::{decode_fsck_output, encode_fsck_output},
};
use crate::error::BootEnvError;
use crate::filesystem::FsckExitCode;
use crate::partition::PartitionName;

/// Command name for GRUB environment manipulation
const GRUB_EDITENV_CMD: &str = "/bin/grub-editenv";

/// Path to the boot partition mount point
const BOOT_DIR_PATH: &str = "/rootfs/boot";

/// Absolute path to the grubenv file
const GRUBENV_PATH: &str = "/rootfs/boot/EFI/BOOT/grubenv";

/// Replaces the fsck output when the encoded record does not fit the grubenv
/// block. Kept short on purpose — it shares the block with every other variable.
const FSCK_OUTPUT_TOO_LARGE: &str = "fsck output too large for the boot env";

/// Constructs the fsck status file path for a non-boot partition on the boot volume.
fn fsck_file_path(partition: PartitionName) -> std::path::PathBuf {
    Path::new(BOOT_DIR_PATH).join(format!("fsck.{partition}"))
}

/// Flush pending writes to disk.
///
/// Some callers persist state and then reboot or halt; neither reboot(2) nor the
/// halt loop syncs, so an unflushed write would be lost exactly there.
fn sync_disk() {
    nix::unistd::sync();
}

/// Store the boot partition's encoded fsck record, retrying with the exit code
/// alone when the full record does not fit.
///
/// grubenv is a fixed-size block shared by all variables, and a verbose
/// `fsck.vfat` run can exceed it. Keeping the code without the output leaves the
/// result visible in the ODS JSON instead of dropping the record entirely.
fn write_boot_fsck_record<W>(
    code: FsckExitCode,
    encoded: &str,
    mut write: W,
) -> crate::bootloader::Result<()>
where
    W: FnMut(&str) -> crate::bootloader::Result<()>,
{
    match write(encoded) {
        Ok(()) => Ok(()),
        Err(e) => {
            log::warn!("boot fsck record rejected by grubenv ({e}); storing exit code only");
            write(&encode_fsck_output(code.bits(), FSCK_OUTPUT_TOO_LARGE))
        }
    }
}

fn save_fsck_to_file(partition: PartitionName, encoded: &str) -> crate::bootloader::Result<()> {
    let file_path = fsck_file_path(partition);
    fs::write(&file_path, encoded).map_err(|e| BootEnvError::CommandFailed {
        command: format!("write {}", file_path.display()),
        reason: e.to_string(),
    })?;
    sync_disk();
    Ok(())
}

fn get_fsck_from_file(partition: PartitionName) -> crate::bootloader::Result<Option<FsckRecord>> {
    let file_path = fsck_file_path(partition);
    if !file_path.is_file() {
        return Ok(None);
    }
    let encoded = fs::read_to_string(&file_path).map_err(|e| BootEnvError::CommandFailed {
        command: format!("read {}", file_path.display()),
        reason: e.to_string(),
    })?;
    // Remove file after reading; each fsck result is consumed once.
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
        fs::remove_file(&file_path).map_err(|e| BootEnvError::CommandFailed {
            command: format!("remove {}", file_path.display()),
            reason: e.to_string(),
        })?;
        sync_disk();
    }
    Ok(())
}

/// GRUB boot environment implementation
///
/// Uses `grub-editenv` to read/write environment variables from the grubenv file.
pub struct GrubBootEnv;

impl GrubBootEnv {
    /// Create a new GRUB boot environment accessor.
    ///
    /// # Errors
    /// Returns an error if the grubenv file doesn't exist (indicates a corrupted
    /// boot partition, not a missing file on first boot).
    pub fn new() -> Result<Self> {
        if !Path::new(GRUBENV_PATH).is_file() {
            return Err(BootEnvError::EnvFileNotFound {
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
            .map_err(|e| BootEnvError::CommandFailed {
                command: GRUB_EDITENV_CMD.to_string(),
                reason: e.to_string(),
            })?;

        if !output.status.success() {
            return Err(BootEnvError::CommandExitCode {
                command: GRUB_EDITENV_CMD.to_string(),
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

impl BootEnv for GrubBootEnv {
    fn get_env(&self, key: BootEnvKey) -> Result<Option<String>> {
        let key_str = key.as_str();
        let output = self.run_grub_editenv(&["list"])?;

        for line in output.lines() {
            if let Some((k, v)) = line.split_once('=')
                && k == key_str.as_ref()
            {
                return Ok(Some(v.to_string()));
            }
        }

        Ok(None)
    }

    fn set_env(&mut self, key: BootEnvKey, value: Option<&str>) -> Result<()> {
        let key_str = key.as_str();
        match value {
            Some(v) => {
                let assignment = format!("{}={}", key_str, v);
                self.run_grub_editenv(&["set", &assignment])?;
            }
            None => {
                self.run_grub_editenv(&["unset", key_str.as_ref()])?;
            }
        }
        // grub-editenv leaves the write in the page cache.
        sync_disk();
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
                // Skip; a clean check runs on next boot.
                if code.is_reboot_required() {
                    log::warn!(
                        "Skipping grubenv write for boot partition (fsck exit code {code} — reboot required)"
                    );
                    return Ok(());
                }
                write_boot_fsck_record(code, &encoded, |payload| {
                    self.set_env(BootEnvKey::FsckStatus(PartitionName::Boot), Some(payload))
                })
            }
            PartitionName::RootA
            | PartitionName::RootB
            | PartitionName::RootCurrent
            | PartitionName::Factory
            | PartitionName::Cert
            | PartitionName::Etc
            | PartitionName::Data => {
                // Non-boot partitions: write diagnostic to a file on the boot partition
                // instead of grubenv. grubenv is a small, fixed-size block — storing multiple
                // large encoded blobs there would overflow it. Boot is healthy at this point
                // (its own fsck ran first), so this write is safe regardless of this
                // partition's exit code.
                save_fsck_to_file(partition, &encoded)
            }
            #[cfg(feature = "dos")]
            PartitionName::Extended => save_fsck_to_file(partition, &encoded),
        }
    }

    fn get_fsck_status(&self, partition: PartitionName) -> Result<Option<FsckRecord>> {
        match partition {
            PartitionName::Boot => Ok(self
                .get_env(BootEnvKey::FsckStatus(PartitionName::Boot))?
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
            PartitionName::Boot => self.set_env(BootEnvKey::FsckStatus(PartitionName::Boot), None),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn block_too_small() -> BootEnvError {
        BootEnvError::CommandFailed {
            command: "grub-editenv".into(),
            reason: "environment block too small".into(),
        }
    }

    #[test]
    fn full_record_is_written_once_when_it_fits() {
        let mut seen: Vec<String> = Vec::new();

        let result = write_boot_fsck_record(FsckExitCode::CORRECTED, "encoded", |payload| {
            seen.push(payload.to_string());
            Ok(())
        });

        assert!(result.is_ok());
        assert_eq!(seen, ["encoded"]);
    }

    #[test]
    fn rejected_record_falls_back_to_the_exit_code_alone() {
        let code = FsckExitCode::CORRECTED;
        let mut seen: Vec<String> = Vec::new();

        let result = write_boot_fsck_record(code, "too-long", |payload| {
            seen.push(payload.to_string());
            if seen.len() == 1 {
                Err(block_too_small())
            } else {
                Ok(())
            }
        });

        assert!(result.is_ok(), "the fallback write must succeed");
        assert_eq!(seen.len(), 2, "expected one retry, got {seen:?}");
        assert_eq!(seen[0], "too-long");
        assert_eq!(
            seen[1],
            encode_fsck_output(code.bits(), FSCK_OUTPUT_TOO_LARGE),
            "the retry must carry the exit code with the placeholder output"
        );
    }

    #[test]
    fn a_broken_env_still_surfaces_as_an_error() {
        // Both attempts failing means grubenv itself is unusable, not just full.
        let mut seen: Vec<String> = Vec::new();

        let result =
            write_boot_fsck_record(FsckExitCode::ERRORS_UNCORRECTED, "payload", |payload| {
                seen.push(payload.to_string());
                Err(block_too_small())
            });

        assert!(result.is_err());
        assert_eq!(seen.len(), 2, "must not retry more than once");
    }
}
