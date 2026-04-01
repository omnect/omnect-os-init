//! Filesystem operations
//!
//! This module handles:
//! - Mounting and unmounting filesystems
//! - Running fsck before mounting
//! - Overlayfs setup for etc and home
//! - Tracking mounts for cleanup on error

mod boot_sequence;
mod fsck;
mod mount;
mod overlayfs;

pub use self::boot_sequence::{fsck_and_record, mount_partitions, persist_fsck_results};
pub use self::fsck::{
    FsckResult, check_filesystem, check_filesystem_lenient, describe_fsck_exit_code,
};
pub use self::mount::{MountManager, MountOptions, MountPoint, is_path_mounted};
pub use self::overlayfs::{
    OverlayConfig, mount_points, setup_data_overlay, setup_etc_overlay, setup_raw_rootfs_mount,
};

use crate::error::FilesystemError;

pub type Result<T> = std::result::Result<T, FilesystemError>;
