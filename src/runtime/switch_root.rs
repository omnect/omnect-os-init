//! Switch root to final rootfs and exec init
//!
//! Implements the switch_root operation using MS_MOVE + chroot to transition
//! from initramfs to the real rootfs. pivot_root(2) is not used because ramfs
//! does not support it (returns EINVAL).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use nix::mount::{MsFlags, mount};
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

    // Verify the init binary exists BEFORE moving any mounts. If init is
    // missing we want to fail while /dev, /proc, /sys, /run are still on
    // the initramfs so the debug shell / fatal-error path still works.
    let init_full_path = find_init(new_root, init_path)?;

    // Ensure target mountpoint directories exist under new_root.
    // MS_MOVE fails with ENOENT if the target directory is missing.
    for dir in &["dev", "proc", "sys", "run"] {
        fs::create_dir_all(new_root.join(dir)).map_err(|e| {
            InitramfsError::Io(std::io::Error::other(format!(
                "Failed to create mountpoint {}/{}: {}",
                new_root.display(),
                dir,
                e
            )))
        })?;
    }

    // Move critical mounts to new root before switching.
    // Track which mounts succeeded so any intermediate failure can be rolled back,
    // preserving the debug/emergency shell environment on the initramfs.
    let critical_mounts = [
        ("/dev", "dev"),
        ("/proc", "proc"),
        ("/sys", "sys"),
        ("/run", "run"), // moved so ODS can read its runtime state after root switch
    ];
    let mut moved: Vec<&str> = Vec::new();
    for (src, name) in &critical_mounts {
        if let Err(e) = move_mount(src, &new_root.join(name)) {
            rollback_critical_mounts(&moved, new_root);
            return Err(e);
        }
        moved.push(name);
    }

    chdir(new_root).map_err(|e| {
        rollback_critical_mounts(&moved, new_root);
        InitramfsError::Io(std::io::Error::other(format!(
            "Failed to chdir to new root: {}",
            e
        )))
    })?;

    // MS_MOVE re-mounts the new root at /. This is the correct approach for
    // initramfs: ramfs does not support pivot_root (EINVAL). busybox and
    // systemd use the same MS_MOVE + chroot pattern.
    //
    // On failure: restore all moved mounts so the debug/emergency shell still
    // has access to /dev, /proc, /sys, /run on the initramfs.
    if let Err(e) = mount(Some("."), "/", None::<&str>, MsFlags::MS_MOVE, None::<&str>) {
        rollback_critical_mounts(&moved, new_root);
        return Err(InitramfsError::Io(std::io::Error::other(format!(
            "Failed to MS_MOVE new root to /: {}",
            e
        ))));
    }

    chroot(".").map_err(|e| {
        InitramfsError::Io(std::io::Error::other(format!("Failed to chroot: {}", e)))
    })?;

    chdir("/").map_err(|e| {
        InitramfsError::Io(std::io::Error::other(format!(
            "Failed to chdir to /: {}",
            e
        )))
    })?;

    log::info!("Executing init: {}", init_full_path);

    // exec() replaces the current process - does not return on success
    let err = Command::new(&init_full_path).exec();

    // If we get here, exec failed
    Err(InitramfsError::Io(std::io::Error::other(format!(
        "Failed to exec init: {}",
        err
    ))))
}

fn move_mount(source: &str, target: &Path) -> Result<()> {
    use nix::mount::{MsFlags, mount};

    mount(
        Some(source),
        target,
        None::<&str>,
        MsFlags::MS_MOVE,
        None::<&str>,
    )
    .map_err(|e| {
        InitramfsError::Io(std::io::Error::other(format!(
            "Failed to move {} → {}: {}",
            source,
            target.display(),
            e
        )))
    })?;

    Ok(())
}

/// Attempt to restore already-moved critical mounts back to the initramfs.
///
/// Called on any failure before MS_MOVE succeeds — at that point `/` is still
/// the initramfs root, so moving from `new_root/{name}` back to `/{name}`
/// restores the debug/emergency shell environment. Best-effort: individual
/// failures are logged and do not abort.
fn rollback_critical_mounts(moved: &[&str], new_root: &Path) {
    for name in moved.iter().rev() {
        let src = new_root.join(name);
        let dst = format!("/{name}");
        if let Err(e) = move_mount(&src.to_string_lossy(), Path::new(&dst)) {
            log::warn!("Failed to restore /{name} to initramfs during rollback: {e}");
        } else {
            log::debug!("Restored /{name} to initramfs");
        }
    }
}

/// Returns true if `path` is a regular file with at least one executable bit set.
///
/// Checking only `is_file()` is insufficient — a non-executable file would cause
/// `exec` to fail after critical mounts have already been moved to the new root.
fn is_executable_file(path: &Path) -> bool {
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Find the init binary in the new root.
///
/// Always returns an absolute path string (starts with `/`) so that
/// `Command::new` on PID 1 does not fall back to PATH lookup.
fn find_init(new_root: &Path, requested_init: &str) -> Result<String> {
    // Reject paths containing ".." to prevent escaping new_root before chroot.
    if requested_init.split('/').any(|c| c == "..") {
        return Err(InitramfsError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("init path must not contain '..': {requested_init}"),
        )));
    }

    // Ensure the caller-supplied path is absolute to avoid PATH lookup on exec.
    let requested_init = if requested_init.starts_with('/') {
        requested_init.to_string()
    } else {
        format!("/{}", requested_init)
    };

    let requested_path = new_root.join(requested_init.trim_start_matches('/'));
    if is_executable_file(&requested_path) {
        return Ok(requested_init);
    }

    for init_path in INIT_PATHS {
        let full_path = new_root.join(init_path.trim_start_matches('/'));
        if is_executable_file(&full_path) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn write_executable(path: &std::path::Path, content: &str) {
        fs::write(path, content).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn test_find_init_default() {
        let temp = TempDir::new().unwrap();
        let sbin = temp.path().join("sbin");
        fs::create_dir_all(&sbin).unwrap();
        write_executable(&sbin.join("init"), "#!/bin/sh");

        let result = find_init(temp.path(), "/sbin/init");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "/sbin/init");
    }

    #[test]
    fn test_find_init_systemd() {
        let temp = TempDir::new().unwrap();
        let systemd_dir = temp.path().join("lib/systemd");
        fs::create_dir_all(&systemd_dir).unwrap();
        write_executable(&systemd_dir.join("systemd"), "#!/bin/sh");

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
    fn test_find_init_non_executable() {
        // A file without +x should not be accepted as init.
        let temp = TempDir::new().unwrap();
        let sbin = temp.path().join("sbin");
        fs::create_dir_all(&sbin).unwrap();
        fs::write(sbin.join("init"), "#!/bin/sh").unwrap();
        fs::set_permissions(sbin.join("init"), fs::Permissions::from_mode(0o644)).unwrap();

        let result = find_init(temp.path(), "/sbin/init");
        assert!(result.is_err());
    }
}
