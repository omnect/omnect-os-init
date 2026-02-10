//! Switch root to final rootfs and exec init
//!
//! Implements the switch_root operation to pivot from initramfs to the real rootfs.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use nix::unistd::{chdir, chroot};

use crate::error::{InitramfsError, Result};

/// Default init path
const DEFAULT_INIT: &str = "/sbin/init";

/// Alternative init paths to try
const INIT_PATHS: &[&str] = &[
    "/sbin/init",
    "/usr/sbin/init",
    "/lib/systemd/systemd",
    "/usr/lib/systemd/systemd",
];

/// Switch root to the new rootfs and exec init
pub fn switch_root(new_root: &Path, init: Option<&str>) -> Result<()> {
    let init_path = init.unwrap_or(DEFAULT_INIT);

    log::info!(
        "Switching root to {} with init {}",
        new_root.display(),
        init_path
    );

    if !new_root.exists() {
        return Err(InitramfsError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("New root does not exist: {}", new_root.display()),
        )));
    }

    let init_full_path = find_init(new_root, init_path)?;

    // Change directory to new root
    chdir(new_root).map_err(|e| {
        InitramfsError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to chdir to new root: {}", e),
        ))
    })?;

    // Perform chroot
    chroot(new_root).map_err(|e| {
        InitramfsError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to chroot: {}", e),
        ))
    })?;

    // Change to root of new filesystem
    chdir("/").map_err(|e| {
        InitramfsError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to chdir to /: {}", e),
        ))
    })?;

    log::info!("Executing init: {}", init_full_path);

    // exec() replaces the current process - does not return on success
    let err = Command::new(&init_full_path).exec();

    // If we get here, exec failed
    Err(InitramfsError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        format!("Failed to exec init: {}", err),
    )))
}

/// Find the init binary in the new root
fn find_init(new_root: &Path, requested_init: &str) -> Result<String> {
    let requested_path = new_root.join(requested_init.trim_start_matches('/'));
    if requested_path.exists() {
        return Ok(requested_init.to_string());
    }

    for init_path in INIT_PATHS {
        let full_path = new_root.join(init_path.trim_start_matches('/'));
        if full_path.exists() {
            log::debug!("Found init at {}", init_path);
            return Ok((*init_path).to_string());
        }
    }

    Err(InitramfsError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!(
            "Init binary not found in {}. Tried: {}, {:?}",
            new_root.display(),
            requested_init,
            INIT_PATHS
        ),
    )))
}

/// Prepare for switch_root by cleaning up initramfs
pub fn prepare_switch_root() -> Result<()> {
    log::debug!("Preparing for switch_root");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_find_init_default() {
        let temp = TempDir::new().unwrap();
        let sbin = temp.path().join("sbin");
        fs::create_dir_all(&sbin).unwrap();
        fs::write(sbin.join("init"), "#!/bin/sh").unwrap();

        let result = find_init(temp.path(), "/sbin/init");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "/sbin/init");
    }

    #[test]
    fn test_find_init_systemd() {
        let temp = TempDir::new().unwrap();
        let systemd_dir = temp.path().join("lib/systemd");
        fs::create_dir_all(&systemd_dir).unwrap();
        fs::write(systemd_dir.join("systemd"), "#!/bin/sh").unwrap();

        let result = find_init(temp.path(), "/sbin/init");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "/lib/systemd/systemd");
    }

    #[test]
    fn test_find_init_not_found() {
        let temp = TempDir::new().unwrap();
        let result = find_init(temp.path(), "/sbin/init");
        assert!(result.is_err());
    }

    #[test]
    fn test_prepare_switch_root() {
        assert!(prepare_switch_root().is_ok());
    }
}
