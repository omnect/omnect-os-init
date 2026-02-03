//! Error types for omnect-os-init
//!
//! This module defines the error hierarchy used throughout the init process.
//! Each subsystem has its own error type that can be converted to the main
//! `InitramfsError` type.

use std::path::PathBuf;
use thiserror::Error;

/// Result type alias using InitramfsError
pub type Result<T> = std::result::Result<T, InitramfsError>;

/// Main error type for the initramfs
#[derive(Error, Debug)]
pub enum InitramfsError {
    /// Bootloader-related errors
    #[error("Bootloader error: {0}")]
    Bootloader(#[from] BootloaderError),

    /// Partition-related errors
    #[error("Partition error: {0}")]
    Partition(#[from] PartitionError),

    /// Filesystem-related errors
    #[error("Filesystem error: {0}")]
    Filesystem(#[from] FilesystemError),

    /// Factory reset errors
    #[error("Factory reset error: {0}")]
    FactoryReset(#[from] FactoryResetError),

    /// Flash mode errors
    #[error("Flash mode error: {0}")]
    FlashMode(#[from] FlashModeError),

    /// Configuration errors
    #[error("Configuration error: {0}")]
    Config(#[from] ConfigError),

    /// Logging errors
    #[error("Logging error: {0}")]
    Logging(#[from] LoggingError),

    /// Early mount setup failed
    #[error("Mount setup failed: {0}")]
    MountSetupFailed(String),

    /// Generic I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Bootloader-specific errors
#[derive(Error, Debug)]
pub enum BootloaderError {
    /// Failed to detect bootloader type
    #[error("Failed to detect bootloader type")]
    DetectionFailed,

    /// Failed to mount boot partition
    #[error("Failed to mount boot partition: {0}")]
    MountFailed(String),

    /// Failed to read environment variable
    #[error("Failed to read bootloader variable '{name}': {reason}")]
    ReadFailed { name: String, reason: String },

    /// Failed to write environment variable
    #[error("Failed to write bootloader variable '{name}': {reason}")]
    WriteFailed { name: String, reason: String },

    /// Command execution failed
    #[error("Bootloader command '{command}' failed: {reason}")]
    CommandFailed { command: String, reason: String },

    /// Invalid variable value
    #[error("Invalid bootloader variable value for '{name}': {reason}")]
    InvalidValue { name: String, reason: String },

    /// I/O error
    #[error("Bootloader I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Partition-specific errors
#[derive(Error, Debug)]
pub enum PartitionError {
    /// Failed to detect root block device
    #[error("Failed to detect root block device: {0}")]
    RootDeviceNotFound(String),

    /// Invalid partition table
    #[error("Invalid partition table on {device}: {reason}")]
    InvalidPartitionTable { device: PathBuf, reason: String },

    /// Partition not found
    #[error("Partition '{name}' not found on {device}")]
    PartitionNotFound { device: PathBuf, name: String },

    /// Failed to create symlink
    #[error("Failed to create symlink {link} -> {target}: {reason}")]
    SymlinkFailed {
        link: PathBuf,
        target: PathBuf,
        reason: String,
    },

    /// Unsupported partition table type
    #[error("Unsupported partition table type: {0}")]
    UnsupportedTableType(String),

    /// I/O error
    #[error("Partition I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Filesystem-specific errors
#[derive(Error, Debug)]
pub enum FilesystemError {
    /// Mount failed
    #[error("Failed to mount {src_path} on {target}: {reason}")]
    MountFailed {
        src_path: PathBuf,
        target: PathBuf,
        reason: String,
    },

    /// Unmount failed
    #[error("Failed to unmount {target}: {reason}")]
    UnmountFailed { target: PathBuf, reason: String },

    /// Fsck failed with critical error
    #[error("Filesystem check failed for {device} with code {code}: {output}")]
    FsckFailed {
        device: PathBuf,
        code: i32,
        output: String,
    },

    /// Fsck requires reboot
    #[error("Filesystem check for {device} requires reboot (code 2)")]
    FsckRebootRequired { device: PathBuf },

    /// Overlayfs setup failed
    #[error("Overlayfs setup failed for {target}: {reason}")]
    OverlayFailed { target: PathBuf, reason: String },

    /// Format failed
    #[error("Failed to format {device} as {fstype}: {reason}")]
    FormatFailed {
        device: PathBuf,
        fstype: String,
        reason: String,
    },

    /// I/O error
    #[error("Filesystem I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Factory reset errors
#[derive(Error, Debug)]
pub enum FactoryResetError {
    /// Invalid factory reset configuration
    #[error("Invalid factory reset configuration: {0}")]
    InvalidConfig(String),

    /// Backup failed
    #[error("Failed to backup path '{path}': {reason}")]
    BackupFailed { path: String, reason: String },

    /// Restore failed
    #[error("Failed to restore path '{path}': {reason}")]
    RestoreFailed { path: String, reason: String },

    /// Wipe failed
    #[error("Wipe operation failed for {partition}: {reason}")]
    WipeFailed { partition: String, reason: String },

    /// Custom wipe script not found or failed
    #[error("Custom wipe script failed: {0}")]
    CustomWipeFailed(String),

    /// Invalid wipe mode
    #[error("Invalid wipe mode: {0}")]
    InvalidWipeMode(u32),

    /// JSON parsing error
    #[error("Failed to parse factory reset JSON: {0}")]
    JsonError(#[from] serde_json::Error),

    /// I/O error
    #[error("Factory reset I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Flash mode errors
#[derive(Error, Debug)]
pub enum FlashModeError {
    /// Invalid flash mode
    #[error("Invalid flash mode: {0}")]
    InvalidMode(String),

    /// Destination device not found
    #[error("Destination device not found: {0}")]
    DestinationNotFound(PathBuf),

    /// Clone operation failed
    #[error("Disk clone failed: {0}")]
    CloneFailed(String),

    /// Network setup failed
    #[error("Network setup failed: {0}")]
    NetworkFailed(String),

    /// Download failed
    #[error("Download failed from {url}: {reason}")]
    DownloadFailed { url: String, reason: String },

    /// Checksum verification failed
    #[error("Checksum verification failed for {url}")]
    ChecksumFailed { url: String },

    /// I/O error
    #[error("Flash mode I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Configuration errors
#[derive(Error, Debug)]
pub enum ConfigError {
    /// Missing required configuration
    #[error("Missing required configuration: {0}")]
    Missing(String),

    /// Invalid configuration value
    #[error("Invalid configuration value for '{key}': {reason}")]
    Invalid { key: String, reason: String },

    /// Failed to parse configuration file
    #[error("Failed to parse configuration file {}: {reason}", path.display())]
    ParseFailed { path: PathBuf, reason: String },

    /// I/O error
    #[error("Configuration I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Logging errors
#[derive(Error, Debug)]
pub enum LoggingError {
    /// Failed to open /dev/kmsg
    #[error("Failed to open /dev/kmsg: {0}")]
    KmsgOpenFailed(std::io::Error),

    /// Failed to initialize logger
    #[error("Failed to initialize logger: {0}")]
    InitFailed(String),

    /// Logger already initialized
    #[error("Logger already initialized")]
    AlreadyInitialized,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = InitramfsError::Bootloader(BootloaderError::DetectionFailed);
        assert!(err.to_string().contains("Bootloader"));

        let err = InitramfsError::Partition(PartitionError::RootDeviceNotFound(
            "no device".to_string(),
        ));
        assert!(err.to_string().contains("root block device"));
    }

    #[test]
    fn test_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: InitramfsError = io_err.into();
        assert!(matches!(err, InitramfsError::Io(_)));
    }

    #[test]
    fn test_bootloader_error_variants() {
        let err = BootloaderError::ReadFailed {
            name: "factory-reset".to_string(),
            reason: "not found".to_string(),
        };
        assert!(err.to_string().contains("factory-reset"));

        let err = BootloaderError::CommandFailed {
            command: "grub-editenv".to_string(),
            reason: "exit code 1".to_string(),
        };
        assert!(err.to_string().contains("grub-editenv"));
    }
}
