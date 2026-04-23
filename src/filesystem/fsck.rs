//! Filesystem check (fsck) operations
//!
//! Runs fsck on partitions before mounting and handles exit codes appropriately.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::FilesystemError;
use crate::filesystem::{FsType, Result};
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

/// Type-safe wrapper for fsck(8) exit codes.
///
/// The value is a bitmask; individual bits can be tested with the predicate
/// methods below. `UNKNOWN` (-1) is set only when fsck cannot be executed at
/// all (spawn failure); signal-killed processes map to `OPERATIONAL_ERROR`
/// via `From<Option<i32>>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsckExitCode(i32);

impl FsckExitCode {
    /// No errors detected.
    pub const OK: Self = Self(0);
    /// Filesystem errors corrected (safe to mount with `-y`).
    pub const CORRECTED: Self = Self(1);
    /// System should be rebooted before mounting.
    pub const REBOOT_REQUIRED: Self = Self(2);
    /// Filesystem errors left uncorrected.
    pub const ERRORS_UNCORRECTED: Self = Self(4);
    /// Operational error in fsck itself.
    pub const OPERATIONAL_ERROR: Self = Self(8);
    /// Usage or syntax error.
    pub const USAGE_ERROR: Self = Self(16);
    /// Cancelled by user request.
    pub const CANCELLED: Self = Self(32);
    /// Shared library error.
    pub const LIBRARY_ERROR: Self = Self(128);
    /// Sentinel: set only when fsck cannot be executed at all (spawn failure).
    ///
    /// Signal-killed processes map to `OPERATIONAL_ERROR` at construction via
    /// `From<Option<i32>>` to avoid the two's-complement all-bits-set footgun.
    pub const UNKNOWN: Self = Self(-1);

    /// The raw integer value (for wire-format serialization into FilesystemError fields).
    pub fn bits(self) -> i32 {
        self.0
    }

    pub fn is_clean(self) -> bool {
        self.0 == 0
    }

    /// Returns `true` if the corrected-errors bit (bit 0) is set.
    ///
    /// Note: this is a bitmask test, not equality with `CORRECTED`. A code of 5
    /// (CORRECTED | ERRORS_UNCORRECTED) returns `true` here even though the
    /// filesystem is not safe to mount. Use `is_mount_safe` for mount decisions.
    pub fn has_corrected_bit(self) -> bool {
        self.0 & 1 != 0
    }

    pub fn is_reboot_required(self) -> bool {
        self.0 & 2 != 0
    }

    pub fn has_uncorrected_errors(self) -> bool {
        self.0 & 4 != 0
    }

    pub fn has_operational_error(self) -> bool {
        self.0 & 8 != 0
    }

    pub fn is_usage_error(self) -> bool {
        self.0 & 16 != 0
    }

    pub fn is_cancelled(self) -> bool {
        self.0 & 32 != 0
    }

    pub fn is_library_error(self) -> bool {
        self.0 & 128 != 0
    }

    /// Returns `true` if the filesystem is safe to mount.
    ///
    /// Strict equality: only codes 0 (clean) and 1 (errors corrected by -y) are
    /// safe. Combined codes such as 3 (CORRECTED | REBOOT_REQUIRED) or 5
    /// (CORRECTED | ERRORS_UNCORRECTED) are rejected even though the corrected-bit
    /// is set. The reboot check is implicit — any code with bit 1 set has value ≥ 2
    /// and cannot equal OK or CORRECTED.
    pub fn is_mount_safe(self) -> bool {
        self == Self::OK || self == Self::CORRECTED
    }
}

impl From<i32> for FsckExitCode {
    fn from(code: i32) -> Self {
        Self(code)
    }
}

impl From<Option<i32>> for FsckExitCode {
    /// Construct from process exit status.
    /// `None` means killed by signal — mapped to `OPERATIONAL_ERROR` to avoid
    /// the two's-complement all-bits-set footgun of the `UNKNOWN` sentinel.
    fn from(code: Option<i32>) -> Self {
        code.map(Self).unwrap_or(Self::OPERATIONAL_ERROR)
    }
}

impl fmt::Display for FsckExitCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == Self::UNKNOWN {
            return write!(f, "unknown (fsck could not be spawned)");
        }
        if self.is_clean() {
            return write!(f, "No errors");
        }
        let mut parts: Vec<&str> = Vec::new();
        if self.has_corrected_bit() {
            parts.push("errors corrected");
        }
        if self.is_reboot_required() {
            parts.push("reboot required");
        }
        if self.has_uncorrected_errors() {
            parts.push("uncorrected errors");
        }
        if self.has_operational_error() {
            parts.push("operational error");
        }
        if self.is_usage_error() {
            parts.push("usage error");
        }
        if self.is_cancelled() {
            parts.push("cancelled");
        }
        if self.is_library_error() {
            parts.push("library error");
        }
        if parts.is_empty() {
            write!(f, "unknown error (code {})", self.0)
        } else {
            write!(f, "{}", parts.join(", "))
        }
    }
}

/// Result of a filesystem check.
#[derive(Debug, Clone)]
pub struct FsckResult {
    /// Device that was checked.
    pub device: PathBuf,
    /// Parsed exit code. Use predicate methods (`is_mount_safe`, `is_reboot_required`, …).
    pub exit_code: FsckExitCode,
    /// Combined stdout + stderr output from fsck.
    pub output: String,
}

impl FsckResult {
    /// Returns `true` if uncorrected filesystem errors remain.
    pub fn has_uncorrected_errors(&self) -> bool {
        self.exit_code.has_uncorrected_errors()
    }

    /// Returns `true` if fsck encountered an operational (tool-level) error.
    pub fn has_operational_error(&self) -> bool {
        self.exit_code.has_operational_error()
    }
}

/// Run fsck on a device
///
/// # Arguments
/// * `device` - Path to the block device to check
/// * `fstype` - Filesystem type
///
/// # Returns
/// * `Ok(FsckResult)` - Result of the check (including exit code 1: errors corrected, safe to mount)
/// * `Err(FilesystemError::FsckRequiresReboot)` - If fsck requests a reboot (exit code 2 only)
/// * `Err(FilesystemError::FsckFailed)` - If check failed with uncorrectable errors
fn check_filesystem(device: &Path, fstype: FsType) -> Result<FsckResult> {
    log::info!("Running fsck on {}", device.display());

    // Disable kernel message rate limiting during fsck — RAII guard restores on all exit paths.
    disable_kmsg_ratelimit();
    let _ratelimit_guard = KmsgRatelimitGuard;

    let mut cmd = Command::new(FSCK_CMD);
    cmd.arg(FSCK_AUTO_REPAIR_FLAG);

    // Explicitly specify the filesystem type so the wrapper dispatches
    // directly to fsck.ext4 / fsck.fat without needing blkid probing.
    cmd.args([FSCK_TYPE_FLAG, fstype.as_str()]);

    cmd.arg(device);

    let output = cmd.output().map_err(|e| FilesystemError::FsckFailed {
        device: device.to_path_buf(),
        code: FsckExitCode::UNKNOWN,
        output: format!("Failed to execute fsck: {}", e),
    })?;

    let exit_code = FsckExitCode::from(output.status.code());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined_output = format!("{}{}", stdout, stderr);

    if exit_code.is_clean() {
        log::debug!("fsck: {} is clean", device.display());
    } else if exit_code == FsckExitCode::CORRECTED {
        log::info!(
            "fsck corrected errors on {} (code 1) — filesystem is clean, continuing",
            device.display()
        );
    } else if exit_code.is_reboot_required() {
        log::warn!(
            "fsck on {} requires reboot ({})",
            device.display(),
            exit_code
        );
    } else {
        log::error!(
            "fsck failed on {} with {}: {}",
            device.display(),
            exit_code,
            combined_output.lines().next().unwrap_or("(no output)")
        );
    }

    if exit_code.is_reboot_required() {
        return Err(FilesystemError::FsckRequiresReboot {
            device: device.to_path_buf(),
            code: exit_code,
            output: combined_output,
        });
    }

    if !exit_code.is_mount_safe() {
        return Err(FilesystemError::FsckFailed {
            device: device.to_path_buf(),
            code: exit_code,
            output: combined_output,
        });
    }

    Ok(FsckResult {
        device: device.to_path_buf(),
        exit_code,
        output: combined_output,
    })
}

/// Run fsck on a device, tolerating non-critical errors.
///
/// Returns `Ok` even if fsck reports correctable errors, unless a reboot is required.
/// Useful for partitions where errors should be logged but boot should continue.
pub fn check_filesystem_lenient(device: &Path, fstype: FsType) -> Result<FsckResult> {
    match check_filesystem(device, fstype) {
        Ok(result) => Ok(result),
        Err(e @ FilesystemError::FsckRequiresReboot { .. }) => Err(e),
        Err(FilesystemError::FsckFailed {
            device,
            code,
            output,
        }) => {
            log::warn!(
                "fsck on {} had errors ({}), continuing anyway",
                device.display(),
                code
            );
            Ok(FsckResult {
                device,
                exit_code: code,
                output,
            })
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for FsckExitCode newtype
    #[test]
    fn test_fsck_exit_code_clean() {
        let code = FsckExitCode::from(Some(0i32));
        assert!(code.is_clean());
        assert!(!code.is_reboot_required());
        assert!(code.is_mount_safe());
        assert_eq!(format!("{code}"), "No errors");
    }

    #[test]
    fn test_fsck_exit_code_corrected() {
        let code = FsckExitCode::from(Some(1i32));
        assert!(code.has_corrected_bit());
        assert!(!code.is_reboot_required());
        assert!(code.is_mount_safe());
        assert_eq!(format!("{code}"), "errors corrected");
    }

    #[test]
    fn test_fsck_exit_code_reboot_required() {
        let code = FsckExitCode::from(Some(2i32));
        assert!(code.is_reboot_required());
        assert!(!code.is_mount_safe());
        assert_eq!(format!("{code}"), "reboot required");
    }

    #[test]
    fn test_fsck_exit_code_combined() {
        let code = FsckExitCode::from(Some(3i32));
        assert!(code.has_corrected_bit());
        assert!(code.is_reboot_required());
        assert!(!code.is_mount_safe());
        assert_eq!(format!("{code}"), "errors corrected, reboot required");
    }

    #[test]
    fn test_fsck_exit_code_unknown_sentinel() {
        let code = FsckExitCode::from(None::<i32>);
        // None (signal-killed) normalizes to OPERATIONAL_ERROR, not UNKNOWN.
        assert_eq!(code, FsckExitCode::OPERATIONAL_ERROR);
        assert!(!code.is_mount_safe());
    }

    #[test]
    fn test_fsck_exit_code_unknown_const_predicates() {
        // UNKNOWN = Self(-1): all bits set in two's complement — document this explicitly
        // so any future change to the sentinel value is intentional.
        assert!(FsckExitCode::UNKNOWN.has_corrected_bit());
        assert!(FsckExitCode::UNKNOWN.is_reboot_required());
        assert!(FsckExitCode::UNKNOWN.has_uncorrected_errors());
        assert!(FsckExitCode::UNKNOWN.has_operational_error());
        // is_mount_safe uses strict equality — UNKNOWN is never OK or CORRECTED.
        assert!(!FsckExitCode::UNKNOWN.is_mount_safe());
    }

    #[test]
    fn test_fsck_exit_code_5_corrected_with_uncorrected_errors() {
        // Code 5 = CORRECTED | ERRORS_UNCORRECTED: corrected-bit is set but
        // uncorrected errors remain — must NOT be considered mount-safe.
        let code = FsckExitCode::from(5i32);
        assert!(code.has_corrected_bit());
        assert!(code.has_uncorrected_errors());
        assert!(!code.is_mount_safe());
    }

    #[test]
    fn test_fsck_exit_code_9_corrected_with_operational_error() {
        // Code 9 = CORRECTED | OPERATIONAL_ERROR: must NOT be considered mount-safe.
        let code = FsckExitCode::from(9i32);
        assert!(code.has_corrected_bit());
        assert!(code.has_operational_error());
        assert!(!code.is_mount_safe());
    }

    #[test]
    fn test_fsck_exit_code_display_unknown() {
        assert_eq!(
            format!("{}", FsckExitCode::UNKNOWN),
            "unknown (fsck could not be spawned)"
        );
    }

    #[test]
    fn test_fsck_exit_code_display_ok() {
        assert_eq!(format!("{}", FsckExitCode::OK), "No errors");
    }

    #[test]
    fn test_fsck_exit_code_display_corrected() {
        assert_eq!(format!("{}", FsckExitCode::CORRECTED), "errors corrected");
    }

    #[test]
    fn test_fsck_exit_code_display_reboot() {
        assert_eq!(
            format!("{}", FsckExitCode::REBOOT_REQUIRED),
            "reboot required"
        );
    }

    #[test]
    fn test_fsck_exit_code_display_combined() {
        assert_eq!(
            format!("{}", FsckExitCode::from(3i32)),
            "errors corrected, reboot required"
        );
    }

    #[test]
    fn test_fsck_exit_code_display_errors() {
        assert_eq!(
            format!("{}", FsckExitCode::ERRORS_UNCORRECTED),
            "uncorrected errors"
        );
    }

    #[test]
    fn test_fsck_result_has_uncorrected_errors() {
        let result = FsckResult {
            device: PathBuf::from("/dev/sda1"),
            exit_code: FsckExitCode::ERRORS_UNCORRECTED,
            output: String::new(),
        };
        assert!(result.has_uncorrected_errors());

        let clean = FsckResult {
            device: PathBuf::from("/dev/sda1"),
            exit_code: FsckExitCode::OK,
            output: String::new(),
        };
        assert!(!clean.has_uncorrected_errors());
    }

    #[test]
    fn test_fsck_result_has_operational_error() {
        let result = FsckResult {
            device: PathBuf::from("/dev/sda1"),
            exit_code: FsckExitCode::OPERATIONAL_ERROR,
            output: String::new(),
        };
        assert!(result.has_operational_error());
    }
}
