//! Filesystem operations
//!
//! This module handles:
//! - Mounting and unmounting filesystems
//! - Running fsck before mounting
//! - Overlayfs setup for etc and home

mod boot_sequence;
mod fsck;
mod mount;
mod overlayfs;

pub use self::boot_sequence::{fsck_and_record, mount_partitions, persist_fsck_results};
pub use self::fsck::{FsckExitCode, FsckResult, check_filesystem_lenient};
pub use self::mount::{
    MountOptions, MountPoint, is_path_mounted, mount, mount_bind, mount_bind_private,
    mount_readwrite, mount_tmpfs,
};
pub use self::overlayfs::{
    mount_points, setup_data_overlay, setup_etc_overlay, setup_raw_rootfs_mount,
};

use crate::error::FilesystemError;

pub type Result<T> = std::result::Result<T, FilesystemError>;
