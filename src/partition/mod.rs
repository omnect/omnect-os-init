//! Partition management for omnect-os initramfs.
//!
//! Handles root device detection, partition layout, and symlink creation.

pub mod device;
pub mod layout;
pub mod symlinks;

use thiserror::Error;

/// Partition-related errors.
#[derive(Debug, Error)]
pub enum PartitionError {
    #[error("device detection failed: {0}")]
    DeviceDetection(String),

    #[error("partition layout error: {0}")]
    Layout(String),

    #[error("symlink creation failed: {0}")]
    Symlink(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result type for partition operations.
pub type Result<T> = std::result::Result<T, PartitionError>;

// Re-export main types
pub use device::{detect_root_device, RootDevice};
pub use layout::{PartitionLayout, PartitionTableType};
pub use symlinks::{create_omnect_symlinks, verify_symlinks};
