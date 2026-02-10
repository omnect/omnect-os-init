//! Filesystem operations
//!
//! This module handles:
//! - Mounting and unmounting filesystems
//! - Running fsck before mounting
//! - Tracking mounts for cleanup on error

mod fsck;
mod mount;

pub use self::fsck::{FsckResult, check_filesystem};
pub use self::mount::{MountManager, MountOptions, MountPoint};

use crate::error::FilesystemError;

pub type Result<T> = std::result::Result<T, FilesystemError>;
