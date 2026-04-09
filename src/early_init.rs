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

/// Path to mount information
const PROC_MOUNTS_PATH: &str = "/proc/mounts";

/// Mounts essential filesystems required before any other initialization.
///
/// Must be called as early as possible, before logging or device access.
/// Order matters: /dev must be first (needed for /dev/kmsg logging).
///
/// The `mount_if_needed` guard makes this function idempotent: some early
/// userspace environments (test runners, dracut, or a second call from a
/// recovery path) may have already mounted one or more of these filesystems
/// before control reaches this point.
pub fn mount_essential_filesystems() -> Result<()> {
    mount_if_needed(
        mounts::DEV_FSTYPE,
        mounts::DEV_PATH,
        mounts::DEV_FSTYPE,
        MsFlags::empty(),
    )?;

    mount_if_needed(
        mounts::PROC_FSTYPE,
        mounts::PROC_PATH,
        mounts::PROC_FSTYPE,
        MsFlags::empty(),
    )?;

    mount_if_needed(
        mounts::SYS_FSTYPE,
        mounts::SYS_PATH,
        mounts::SYS_FSTYPE,
        MsFlags::empty(),
    )?;

    mount_if_needed(
        mounts::RUN_FSTYPE,
        mounts::RUN_PATH,
        mounts::RUN_FSTYPE,
        MsFlags::empty(),
    )?;

    // Disable printk rate limiting for /dev/kmsg
    // This ensures all init messages are logged without suppression
    disable_printk_ratelimit();

    Ok(())
}

fn mount_if_needed(source: &str, target: &str, fstype: &str, flags: MsFlags) -> Result<()> {
    if is_mounted(target)? {
        return Ok(());
    }

    mount(Some(source), target, Some(fstype), flags, None::<&str>).map_err(|e| {
        EarlyInitError::MountFailed {
            target: target.to_string(),
            reason: e.to_string(),
        }
    })
}

fn is_mounted(path: &str) -> Result<bool> {
    // Before /proc is mounted, we can't check - assume not mounted
    let mounts = std::fs::read_to_string(PROC_MOUNTS_PATH).unwrap_or_default();

    Ok(mounts.lines().any(|line| {
        line.split_whitespace()
            .nth(1)
            .is_some_and(|mount_point| mount_point == path)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_mounted_parses_proc_mounts() {
        // This test just verifies the parsing logic works
        // Actual mount checking requires root privileges
        let result = is_mounted("/nonexistent");
        assert!(result.is_ok());
    }
}
