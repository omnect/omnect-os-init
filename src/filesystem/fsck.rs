//! Filesystem check (fsck) operations
//!
//! Runs fsck on partitions before mounting and handles exit codes appropriately.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::FilesystemError;
use crate::filesystem::Result;
use crate::logging::{KmsgRatelimitGuard, disable_kmsg_ratelimit};

/// fsck command — util-linux wrapper that dispatches to fsck.ext4 / fsck.fat
/// based on the -t argument. Requires e2fsprogs-e2fsck in the initramfs image
/// to provide the fsck.ext4 backend.
const FSCK_CMD: &str = "/sbin/fsck";

/// Always pass -y (auto-repair): this binary runs unattended in initramfs.
/// Kept separate from FSCK_CMD because Command::new takes only the executable path.
const FSCK_AUTO_REPAIR_FLAG: &str = "-y";

/// Filesystem type flag: tells the wrapper which backend to dispatch to (fsck.ext4 / fsck.fat).
/// Without -t, the wrapper falls back to blkid probing which is absent in initramfs.
const FSCK_TYPE_FLAG: &str = "-t";

/// fsck exit codes
mod exit_code {
    /// No errors
    pub const OK: i32 = 0;
    /// Filesystem errors corrected
    pub const CORRECTED: i32 = 1;
    /// System should be rebooted
    pub const REBOOT_REQUIRED: i32 = 2;
    /// Filesystem errors left uncorrected
    pub const ERRORS_UNCORRECTED: i32 = 4;
    /// Operational error
    pub const OPERATIONAL_ERROR: i32 = 8;
    /// Usage or syntax error
    pub const USAGE_ERROR: i32 = 16;
    /// Cancelled by user
    pub const CANCELLED: i32 = 32;
    /// Shared library error
    pub const LIBRARY_ERROR: i32 = 128;
    /// Sentinel for when the process was killed by a signal (no exit code)
    pub const UNKNOWN: i32 = -1;
}

/// Result of a filesystem check
#[derive(Debug, Clone)]
pub struct FsckResult {
    /// Device that was checked
    pub device: PathBuf,
    /// Exit code from fsck
    pub exit_code: i32,
    /// Output from fsck (stdout + stderr)
    pub output: String,
    /// Whether the check was successful (code 0: clean, or code 1: errors corrected by -y)
    pub success: bool,
    /// Whether a reboot is required (code 2 only: fsck explicitly requests reboot)
    pub reboot_required: bool,
}

impl FsckResult {
    /// Check if there were uncorrected errors
    pub fn has_uncorrected_errors(&self) -> bool {
        self.exit_code & exit_code::ERRORS_UNCORRECTED != 0
    }

    /// Check if there was an operational error
    pub fn has_operational_error(&self) -> bool {
        self.exit_code & exit_code::OPERATIONAL_ERROR != 0
    }
}

/// Run fsck on a device
///
/// # Arguments
/// * `device` - Path to the block device to check
/// * `fstype` - Filesystem type (e.g. "ext4", "vfat")
///
/// # Returns
/// * `Ok(FsckResult)` - Result of the check (including exit code 1: errors corrected, safe to mount)
/// * `Err(FilesystemError::FsckRequiresReboot)` - If fsck requests a reboot (exit code 2 only)
/// * `Err(FilesystemError::FsckFailed)` - If check failed with uncorrectable errors
fn check_filesystem(device: &Path, fstype: &str) -> Result<FsckResult> {
    log::info!("Running fsck on {}", device.display());

    // Disable kernel message rate limiting during fsck — RAII guard restores on all exit paths.
    disable_kmsg_ratelimit();
    let _ratelimit_guard = KmsgRatelimitGuard;

    let mut cmd = Command::new(FSCK_CMD);
    cmd.arg(FSCK_AUTO_REPAIR_FLAG);

    // Explicitly specify the filesystem type so the wrapper dispatches
    // directly to fsck.ext4 / fsck.fat without needing blkid probing.
    cmd.args([FSCK_TYPE_FLAG, fstype]);

    cmd.arg(device);

    let output = cmd.output().map_err(|e| FilesystemError::FsckFailed {
        device: device.to_path_buf(),
        code: exit_code::UNKNOWN,
        output: format!("Failed to execute fsck: {}", e),
    })?;

    let exit_code = output.status.code().unwrap_or(exit_code::UNKNOWN);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined_output = format!("{}{}", stdout, stderr);

    let reboot_required = exit_code & exit_code::REBOOT_REQUIRED != 0;
    let result = FsckResult {
        device: device.to_path_buf(),
        exit_code,
        output: combined_output.clone(),
        // Use exact match (not bitwise) so exit code 3 (CORRECTED | REBOOT_REQUIRED)
        // never sets success=true. When REBOOT_REQUIRED is set, reboot takes precedence.
        success: (exit_code == exit_code::OK || exit_code == exit_code::CORRECTED)
            && !reboot_required,
        reboot_required,
    };

    // Log the result
    if exit_code == exit_code::OK {
        log::debug!("fsck: {} is clean", device.display());
    } else if exit_code == exit_code::CORRECTED {
        log::info!(
            "fsck corrected errors on {} (code 1) — filesystem is clean, continuing",
            device.display()
        );
    } else if result.reboot_required {
        log::warn!(
            "fsck on {} requires reboot (code {})",
            device.display(),
            exit_code
        );
    } else {
        log::error!(
            "fsck failed on {} with code {}: {}",
            device.display(),
            exit_code,
            combined_output.lines().next().unwrap_or("(no output)")
        );
    }

    // Exit code 2: fsck requests a reboot before mounting
    if result.reboot_required {
        return Err(FilesystemError::FsckRequiresReboot {
            device: device.to_path_buf(),
            code: exit_code,
            output: combined_output,
        });
    }

    // Exit codes ≥4: uncorrectable errors
    if !result.success {
        return Err(FilesystemError::FsckFailed {
            device: device.to_path_buf(),
            code: exit_code,
            output: combined_output,
        });
    }

    Ok(result)
}

/// Run fsck on a device, tolerating non-critical errors.
///
/// Returns `Ok` even if fsck reports correctable errors, unless a reboot is required.
/// Useful for partitions where errors should be logged but boot should continue.
pub fn check_filesystem_lenient(device: &Path, fstype: &str) -> Result<FsckResult> {
    match check_filesystem(device, fstype) {
        Ok(result) => Ok(result),
        Err(FilesystemError::FsckRequiresReboot {
            device,
            code,
            output,
        }) => Err(FilesystemError::FsckRequiresReboot {
            device,
            code,
            output,
        }),
        Err(FilesystemError::FsckFailed {
            device,
            code,
            output,
        }) => {
            log::warn!(
                "fsck on {} had errors (code {}), continuing anyway",
                device.display(),
                code
            );
            Ok(FsckResult {
                device,
                exit_code: code,
                output,
                success: false,
                reboot_required: false,
            })
        }
        Err(e) => Err(e),
    }
}

/// Parse fsck exit code into human-readable description
pub fn describe_fsck_exit_code(code: i32) -> String {
    let mut descriptions = Vec::new();

    if code == exit_code::OK {
        return "No errors".to_string();
    }

    if code & exit_code::CORRECTED != 0 {
        descriptions.push("errors corrected");
    }
    if code & exit_code::REBOOT_REQUIRED != 0 {
        descriptions.push("reboot required");
    }
    if code & exit_code::ERRORS_UNCORRECTED != 0 {
        descriptions.push("uncorrected errors");
    }
    if code & exit_code::OPERATIONAL_ERROR != 0 {
        descriptions.push("operational error");
    }
    if code & exit_code::USAGE_ERROR != 0 {
        descriptions.push("usage error");
    }
    if code & exit_code::CANCELLED != 0 {
        descriptions.push("cancelled");
    }
    if code & exit_code::LIBRARY_ERROR != 0 {
        descriptions.push("library error");
    }

    if descriptions.is_empty() {
        format!("unknown error (code {})", code)
    } else {
        descriptions.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_describe_fsck_exit_code_ok() {
        assert_eq!(describe_fsck_exit_code(0), "No errors");
    }

    #[test]
    fn test_describe_fsck_exit_code_corrected() {
        assert_eq!(describe_fsck_exit_code(1), "errors corrected");
    }

    #[test]
    fn test_describe_fsck_exit_code_reboot() {
        assert_eq!(describe_fsck_exit_code(2), "reboot required");
    }

    #[test]
    fn test_describe_fsck_exit_code_combined() {
        // Code 3 = CORRECTED | REBOOT_REQUIRED
        assert_eq!(
            describe_fsck_exit_code(3),
            "errors corrected, reboot required"
        );
    }

    #[test]
    fn test_describe_fsck_exit_code_errors() {
        assert_eq!(describe_fsck_exit_code(4), "uncorrected errors");
    }

    #[test]
    fn test_fsck_result_has_uncorrected_errors() {
        let result = FsckResult {
            device: PathBuf::from("/dev/sda1"),
            exit_code: 4,
            output: String::new(),
            success: false,
            reboot_required: false,
        };
        assert!(result.has_uncorrected_errors());

        let clean = FsckResult {
            device: PathBuf::from("/dev/sda1"),
            exit_code: 0,
            output: String::new(),
            success: true,
            reboot_required: false,
        };
        assert!(!clean.has_uncorrected_errors());
    }

    #[test]
    fn test_fsck_result_has_operational_error() {
        let result = FsckResult {
            device: PathBuf::from("/dev/sda1"),
            exit_code: 8,
            output: String::new(),
            success: false,
            reboot_required: false,
        };
        assert!(result.has_operational_error());
    }
}
