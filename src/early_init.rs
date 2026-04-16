//! Early initialization before logging is available
//!
//! This module mounts essential filesystems (/dev, /proc, /sys, /run)
//! that must be available before any other initialization can occur.

use nix::mount::{MsFlags, mount};

use crate::error::EarlyInitError;
use crate::logging::disable_printk_ratelimit;

pub type Result<T> = std::result::Result<T, EarlyInitError>;

/// Essential filesystem mount points and their configuration
mod mounts {
    pub const DEV_PATH: &str = "/dev";
    pub const DEV_FSTYPE: &str = "devtmpfs";

    pub const PROC_PATH: &str = "/proc";
    pub const PROC_FSTYPE: &str = "proc";

    pub const SYS_PATH: &str = "/sys";
    pub const SYS_FSTYPE: &str = "sysfs";

    pub const RUN_PATH: &str = "/run";
    pub const RUN_FSTYPE: &str = "tmpfs";
}

/// Mounts essential filesystems required before any other initialization.
///
/// Must be called as early as possible, before logging or device access.
/// Order matters: /dev must be first (needed for /dev/kmsg logging).
pub fn mount_essential_filesystems() -> Result<()> {
    do_mount(mounts::DEV_FSTYPE, mounts::DEV_PATH, mounts::DEV_FSTYPE)?;
    do_mount(mounts::PROC_FSTYPE, mounts::PROC_PATH, mounts::PROC_FSTYPE)?;
    do_mount(mounts::SYS_FSTYPE, mounts::SYS_PATH, mounts::SYS_FSTYPE)?;
    do_mount(mounts::RUN_FSTYPE, mounts::RUN_PATH, mounts::RUN_FSTYPE)?;

    disable_printk_ratelimit();

    Ok(())
}

fn do_mount(source: &str, target: &str, fstype: &str) -> Result<()> {
    mount(
        Some(source),
        target,
        Some(fstype),
        MsFlags::empty(),
        None::<&str>,
    )
    .map_err(|e| EarlyInitError::MountFailed {
        target: target.to_string(),
        reason: e.to_string(),
    })
}
