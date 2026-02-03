//! Early initialization for initramfs
//!
//! This module handles mounting essential virtual filesystems
//! that must be available before anything else can work:
//! - /dev (devtmpfs) - for device nodes including /dev/kmsg
//! - /proc (proc) - for /proc/cmdline and /proc/mounts
//! - /sys (sysfs) - for device information

use std::fs;
use std::os::unix::fs::PermissionsExt;

use nix::mount::{mount, MsFlags};

use crate::error::InitramfsError;

/// Mount essential virtual filesystems for early init
///
/// This must be called before any other initialization, as it sets up:
/// - /dev - needed for /dev/kmsg logging and device access
/// - /proc - needed for /proc/cmdline parsing
/// - /sys - needed for device enumeration
///
/// The function is idempotent - it will skip filesystems that are already mounted.
pub fn mount_essential_filesystems() -> Result<(), InitramfsError> {
    // Mount /dev if not already mounted
    if !is_mounted("/dev") {
        ensure_directory_exists("/dev")?;
        mount_devtmpfs("/dev")?;
    }

    // Mount /proc if not already mounted
    if !is_mounted("/proc") {
        ensure_directory_exists("/proc")?;
        mount_proc("/proc")?;
    }

    // Mount /sys if not already mounted
    if !is_mounted("/sys") {
        ensure_directory_exists("/sys")?;
        mount_sysfs("/sys")?;
    }

    // Disable printk rate limiting for /dev/kmsg
    // This ensures all init messages are logged without suppression
    disable_printk_ratelimit();

    Ok(())
}

/// Disable printk rate limiting for /dev/kmsg
///
/// By default, the kernel rate-limits messages written to /dev/kmsg.
/// For the init process, we want all messages to be logged.
fn disable_printk_ratelimit() {
    // Try to set printk_devkmsg to "on" to disable rate limiting
    // This is a best-effort operation - if it fails, we continue anyway
    let _ = fs::write("/proc/sys/kernel/printk_devkmsg", "on\n");
}

/// Check if a path is a mount point by comparing device IDs
fn is_mounted(path: &str) -> bool {
    // Try to read /proc/mounts first (most reliable)
    if let Ok(mounts) = fs::read_to_string("/proc/mounts") {
        return mounts.lines().any(|line| {
            line.split_whitespace().nth(1) == Some(path)
        });
    }

    // Fallback: try to stat the path and its parent
    // If they have different device IDs, path is a mount point
    use std::os::unix::fs::MetadataExt;
    if let (Ok(path_meta), Ok(parent_meta)) = (
        fs::metadata(path),
        fs::metadata(format!("{}/..", path))
    ) {
        return path_meta.dev() != parent_meta.dev();
    }

    false
}

/// Ensure a directory exists, creating it if necessary
fn ensure_directory_exists(path: &str) -> Result<(), InitramfsError> {
    if !std::path::Path::new(path).exists() {
        fs::create_dir_all(path).map_err(|e| {
            InitramfsError::MountSetupFailed(format!(
                "failed to create directory {}: {}", path, e
            ))
        })?;
        // Set permissions to 0755
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|e| {
            InitramfsError::MountSetupFailed(format!(
                "failed to set permissions on {}: {}", path, e
            ))
        })?;
    }
    Ok(())
}

/// Mount devtmpfs at the specified path
fn mount_devtmpfs(target: &str) -> Result<(), InitramfsError> {
    mount(
        Some("devtmpfs"),
        target,
        Some("devtmpfs"),
        MsFlags::empty(),
        None::<&str>,
    ).map_err(|e| {
        InitramfsError::MountSetupFailed(format!(
            "failed to mount devtmpfs at {}: {}", target, e
        ))
    })
}

/// Mount proc filesystem at the specified path
fn mount_proc(target: &str) -> Result<(), InitramfsError> {
    mount(
        Some("proc"),
        target,
        Some("proc"),
        MsFlags::empty(),
        None::<&str>,
    ).map_err(|e| {
        InitramfsError::MountSetupFailed(format!(
            "failed to mount proc at {}: {}", target, e
        ))
    })
}

/// Mount sysfs at the specified path
fn mount_sysfs(target: &str) -> Result<(), InitramfsError> {
    mount(
        Some("sysfs"),
        target,
        Some("sysfs"),
        MsFlags::empty(),
        None::<&str>,
    ).map_err(|e| {
        InitramfsError::MountSetupFailed(format!(
            "failed to mount sysfs at {}: {}", target, e
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_mounted_proc() {
        // /proc should be mounted in any normal Linux environment
        // This test will pass in development but may need adjustment for CI
        assert!(is_mounted("/proc"));
    }
}
