//! Partition management for omnect-os initramfs.
//!
//! Handles root device detection, partition layout, and symlink creation.

pub mod device;
pub mod layout;
pub mod symlinks;

// Re-export error type from crate::error
pub use crate::error::PartitionError;

/// Result type for partition operations.
pub type Result<T> = std::result::Result<T, PartitionError>;

// Re-export main types
pub use device::{RootDevice, detect_root_device};
pub use layout::{PartitionLayout, PartitionTableType};
pub use symlinks::{create_omnect_symlinks, verify_symlinks};
