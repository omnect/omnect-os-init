//! Error types for the initramfs
//!
//! This module defines a hierarchy of error types for different subsystems.

use std::path::PathBuf;

use thiserror::Error;

use crate::filesystem::FsckExitCode;
use crate::recovery::RecoveryClass;

/// Result type alias for the initramfs
pub type Result<T> = std::result::Result<T, InitramfsError>;

/// Top-level error type for the initramfs
#[derive(Error, Debug)]
pub enum InitramfsError {
    #[error("BootEnv error: {0}")]
    Bootloader(#[from] BootEnvError),

    #[error("degraded boot: {0}")]
    DegradedBoot(#[source] BootEnvError),

    #[error("Early init error: {0}")]
    EarlyInit(#[from] EarlyInitError),

    #[error("Partition error: {0}")]
    Partition(#[from] PartitionError),

    #[error("Filesystem error: {0}")]
    Filesystem(#[from] FilesystemError),

    #[error("Logging error: {0}")]
    Logging(#[from] LoggingError),

    #[error("Config error: {0}")]
    Config(#[from] ConfigError),

    #[cfg(feature = "resize-data")]
    #[error("Resize data error: {0}")]
    ResizeData(#[from] ResizeDataError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl InitramfsError {
    /// Classify the error for the recovery policy. The match is exhaustive
    /// so a new variant fails to compile until classified.
    ///
    /// Spec: docs/superpowers/specs/2026-05-27-boot-failure-recovery-policy-design.md §2.1
    pub fn recovery_class(&self) -> RecoveryClass {
        match self {
            Self::Bootloader(_) => RecoveryClass::Fatal,
            Self::DegradedBoot(_) => RecoveryClass::Fatal,
            Self::EarlyInit(_) => RecoveryClass::Fatal,
            Self::Partition(_) => RecoveryClass::Fatal,
            Self::Filesystem(FilesystemError::FsckRequiresReboot { .. }) => {
                RecoveryClass::RebootToApply
            }
            Self::Filesystem(FilesystemError::MountFailed { .. }) => RecoveryClass::Fatal,
            Self::Filesystem(FilesystemError::UnmountFailed { .. }) => RecoveryClass::Fatal,
            Self::Filesystem(FilesystemError::FsckFailed { .. }) => RecoveryClass::Fatal,
            Self::Filesystem(FilesystemError::OverlayFailed { .. }) => RecoveryClass::Fatal,
            Self::Filesystem(FilesystemError::FormatFailed { .. }) => RecoveryClass::Fatal,
            Self::Filesystem(FilesystemError::Io(_)) => RecoveryClass::Fatal,
            Self::Logging(_) => RecoveryClass::Fatal,
            Self::Config(_) => RecoveryClass::Fatal,
            #[cfg(feature = "resize-data")]
            Self::ResizeData(_) => RecoveryClass::ContinueDegraded,
            Self::Io(_) => RecoveryClass::Fatal,
        }
    }
}

/// Errors during early initialization (before logging is available)
#[derive(Error, Debug)]
pub enum EarlyInitError {
    #[error("Failed to mount {target}: {reason}")]
    MountFailed { target: String, reason: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Errors related to bootloader environment access
#[derive(Error, Debug)]
pub enum BootEnvError {
    #[error("Boot environment file not found: {}", path.display())]
    EnvFileNotFound { path: PathBuf },

    #[error("Command '{command}' failed: {reason}")]
    CommandFailed { command: String, reason: String },

    #[error("Command '{command}' exited with code {code:?}: {stderr}")]
    CommandExitCode {
        command: String,
        code: Option<i32>,
        stderr: String,
    },

    #[error("Compression failed: {0}")]
    CompressionFailed(String),

    #[error("Decompression failed: {0}")]
    DecompressionFailed(String),

    #[error("Invalid environment value for '{key}': {reason}")]
    InvalidValue { key: String, reason: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Errors related to partition detection and management
#[derive(Error, Debug)]
pub enum PartitionError {
    #[error("device detection failed: {0}")]
    DeviceDetection(String),

    #[error("invalid partition table on {}: {reason}", device.display())]
    InvalidPartitionTable { device: PathBuf, reason: String },

    #[error("symlink creation failed for {} -> {}: {reason}", link.display(), target.display())]
    SymlinkFailed {
        link: PathBuf,
        target: PathBuf,
        reason: String,
    },

    #[error("symlink removal failed for {}: {reason}", path.display())]
    SymlinkRemoveFailed { path: PathBuf, reason: String },

    #[error("unknown root partition {}: expected root_a or root_b", path.display())]
    UnknownRootPartition { path: PathBuf },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Errors related to filesystem operations
#[derive(Error, Debug)]
pub enum FilesystemError {
    #[error("Failed to mount {} on {}: {reason}", src_path.display(), target.display())]
    MountFailed {
        src_path: PathBuf,
        target: PathBuf,
        reason: String,
    },

    #[error("Failed to unmount {}: {reason}", target.display())]
    UnmountFailed { target: PathBuf, reason: String },

    #[error("Filesystem check failed for {} with code {code}: {output}", device.display())]
    FsckFailed {
        device: PathBuf,
        code: FsckExitCode,
        output: String,
    },

    #[error("Filesystem check for {} requires reboot (fsck exit code {code})", device.display())]
    FsckRequiresReboot {
        device: PathBuf,
        code: FsckExitCode,
        output: String,
    },

    #[error("Overlayfs setup failed for {}: {reason}", target.display())]
    OverlayFailed { target: PathBuf, reason: String },

    #[error("Failed to format {} as {fstype}: {reason}", device.display())]
    FormatFailed {
        device: PathBuf,
        fstype: String,
        reason: String,
    },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Errors related to configuration loading
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("failed to read /proc/cmdline: {0}")]
    CmdlineReadFailed(#[source] std::io::Error),
}

/// Errors during data partition resize
#[cfg(feature = "resize-data")]
#[derive(Error, Debug)]
pub enum ResizeDataError {
    #[error("Command '{command}' failed with code {code}: {output}")]
    CommandFailed {
        command: String,
        code: i32,
        output: String,
    },

    #[error("Could not determine partition number from device path: {}", .0.display())]
    InvalidDevicePath(PathBuf),

    #[error("Could not find extended partition on {}", .0.display())]
    ExtendedPartitionNotFound(PathBuf),

    #[error("Device path is not valid UTF-8: {}", .0.display())]
    NonUtf8Path(PathBuf),

    #[error("Filesystem error: {0}")]
    Filesystem(#[from] FilesystemError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Errors related to logging
#[derive(Error, Debug)]
pub enum LoggingError {
    #[error("Failed to open kmsg: {0}")]
    KmsgOpenFailed(String),

    #[error("Failed to initialize logger: {0}")]
    InitFailed(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod recovery_class_tests {
    use super::*;
    use crate::filesystem::FsckExitCode;
    use crate::recovery::RecoveryClass;

    #[test]
    fn fsck_requires_reboot_is_reboot_to_apply() {
        let err = InitramfsError::Filesystem(FilesystemError::FsckRequiresReboot {
            device: std::path::PathBuf::from("/dev/sda7"),
            code: FsckExitCode::REBOOT_REQUIRED,
            output: String::new(),
        });
        assert_eq!(err.recovery_class(), RecoveryClass::RebootToApply);
    }

    #[test]
    fn fsck_failed_is_fatal() {
        let err = InitramfsError::Filesystem(FilesystemError::FsckFailed {
            device: std::path::PathBuf::from("/dev/sda7"),
            code: FsckExitCode::ERRORS_UNCORRECTED,
            output: String::new(),
        });
        assert_eq!(err.recovery_class(), RecoveryClass::Fatal);
    }

    #[test]
    fn mount_failed_is_fatal() {
        let err = InitramfsError::Filesystem(FilesystemError::MountFailed {
            src_path: std::path::PathBuf::from("/dev/sda2"),
            target: std::path::PathBuf::from("/rootfs"),
            reason: "test".into(),
        });
        assert_eq!(err.recovery_class(), RecoveryClass::Fatal);
    }

    #[test]
    fn degraded_boot_is_fatal() {
        let err = InitramfsError::DegradedBoot(BootEnvError::CommandFailed {
            command: "grub-editenv".into(),
            reason: "test".into(),
        });
        assert_eq!(err.recovery_class(), RecoveryClass::Fatal);
    }

    #[test]
    fn early_init_is_fatal() {
        let err = InitramfsError::EarlyInit(EarlyInitError::MountFailed {
            target: "/dev".into(),
            reason: "test".into(),
        });
        assert_eq!(err.recovery_class(), RecoveryClass::Fatal);
    }

    #[test]
    fn config_is_fatal() {
        let err = InitramfsError::Config(ConfigError::CmdlineReadFailed(std::io::Error::from(
            std::io::ErrorKind::NotFound,
        )));
        assert_eq!(err.recovery_class(), RecoveryClass::Fatal);
    }

    #[test]
    fn bootloader_error_is_fatal() {
        let err = InitramfsError::Bootloader(BootEnvError::CommandFailed {
            command: "grub-editenv".into(),
            reason: "test".into(),
        });
        assert_eq!(err.recovery_class(), RecoveryClass::Fatal);
    }

    #[test]
    fn partition_error_is_fatal() {
        let err = InitramfsError::Partition(PartitionError::DeviceDetection("test".into()));
        assert_eq!(err.recovery_class(), RecoveryClass::Fatal);
    }

    #[test]
    fn logging_error_is_fatal() {
        let err = InitramfsError::Logging(LoggingError::KmsgOpenFailed("test".into()));
        assert_eq!(err.recovery_class(), RecoveryClass::Fatal);
    }

    #[test]
    fn io_error_is_fatal() {
        let err = InitramfsError::Io(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert_eq!(err.recovery_class(), RecoveryClass::Fatal);
    }

    #[cfg(feature = "resize-data")]
    #[test]
    fn resize_data_is_continue_degraded() {
        // ResizeData is classified ContinueDegraded: a resize failure is non-fatal by policy.
        let err = InitramfsError::ResizeData(ResizeDataError::InvalidDevicePath(
            std::path::PathBuf::from("/dev/sda"),
        ));
        assert_eq!(err.recovery_class(), RecoveryClass::ContinueDegraded);
    }
}
