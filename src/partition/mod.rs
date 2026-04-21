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
#[cfg(feature = "grub")]
pub use device::root_device_from_blkid;
pub use device::{RootDevice, detect_root_device};
#[cfg(feature = "uboot")]
pub use device::{device_from_path, parse_device_path};
pub use layout::{PartitionLayout, partition_names};
pub use symlinks::{create_omnect_symlinks, verify_symlinks};
