use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{FactoryResetError, Result};

const CP_CMD: &str = "/bin/cp";
const SYNC_CMD: &str = "/bin/sync";

/// Result of a restore pass over the full preserve list.
pub enum RestoreResult {
    Success,
    PartialFailure { context: String, error: String },
}

/// Backup all preserve-list paths from rootfs into backup_dir.
pub fn backup_all(rootfs: &Path, preserve_list: &[String], backup_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(backup_dir)?;
    for path in preserve_list {
        backup_path(rootfs, path, backup_dir)?;
    }
    Ok(())
}

/// Restore all preserve-list paths from backup_dir back into rootfs.
///
/// Partial restore failures are accumulated and returned as `RestoreResult::PartialFailure`
/// rather than aborting mid-restore. This ensures as many paths as possible are restored
/// even when individual files cannot be copied.
pub fn restore_all(
    rootfs: &Path,
    preserve_list: &[String],
    backup_dir: &Path,
) -> Result<RestoreResult> {
    let mut error_context: Vec<String> = Vec::new();
    let mut last_error: Option<String> = None;

    for path in preserve_list {
        if let Err(e) = restore_path(rootfs, path, backup_dir) {
            log::warn!("restore failed for {path}: {e}");
            error_context.push(format!("{}:restore", path.trim_start_matches('/')));
            last_error = Some(e.to_string());
        }
    }

    if let Some(error) = last_error {
        Ok(RestoreResult::PartialFailure {
            context: error_context.join(";"),
            error,
        })
    } else {
        Ok(RestoreResult::Success)
    }
}

fn backup_path(rootfs: &Path, path: &str, backup_dir: &Path) -> Result<()> {
    let src = rootfs.join(path.trim_start_matches('/'));

    if !src.exists() {
        log::info!("backup: {path} does not exist; skipping");
        return Ok(());
    }

    log::info!("backup: {path}");

    let output = Command::new(CP_CMD)
        .args(["--parents", "-a"])
        .arg(&src)
        .arg(backup_dir)
        .output()
        .map_err(|e| FactoryResetError::BackupFailed {
            path: PathBuf::from(path),
            reason: format!("Failed to run cp: {e}"),
        })?;

    if !output.status.success() {
        return Err(FactoryResetError::BackupFailed {
            path: PathBuf::from(path),
            reason: format!(
                "cp failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ),
        }
        .into());
    }

    run_sync()?;
    Ok(())
}

fn restore_path(rootfs: &Path, path: &str, backup_dir: &Path) -> Result<()> {
    let path_stripped = path.trim_start_matches('/');
    let backup_src = backup_dir
        .join(rootfs.strip_prefix("/").unwrap_or(rootfs))
        .join(path_stripped);

    if !backup_src.exists() {
        log::info!("restore: {path} does not exist in backup; skipping");
        return Ok(());
    }

    log::info!("restore: {path}");

    let dest_full = rootfs.join(path_stripped);
    let dest_dir = dest_full.parent().unwrap_or(rootfs);

    std::fs::create_dir_all(dest_dir).map_err(|e| FactoryResetError::RestoreFailed {
        path: PathBuf::from(path),
        reason: format!("Failed to create directory {}: {e}", dest_dir.display()),
    })?;

    let output = Command::new(CP_CMD)
        .arg("-a")
        .arg(&backup_src)
        .arg(dest_dir)
        .output()
        .map_err(|e| FactoryResetError::RestoreFailed {
            path: PathBuf::from(path),
            reason: format!("Failed to run cp: {e}"),
        })?;

    if !output.status.success() {
        return Err(FactoryResetError::RestoreFailed {
            path: PathBuf::from(path),
            reason: format!(
                "cp failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ),
        }
        .into());
    }

    run_sync()?;
    Ok(())
}

fn run_sync() -> Result<()> {
    let status = Command::new(SYNC_CMD)
        .status()
        .map_err(|e| FactoryResetError::Io(std::io::Error::other(format!("sync failed: {e}"))))?;
    if !status.success() {
        return Err(
            FactoryResetError::Io(std::io::Error::other(format!("sync exited with {status}")))
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn backup_path_nonexistent_source_silently_skips() {
        let temp = TempDir::new().unwrap();
        let rootfs = temp.path().join("rootfs");
        let backup = temp.path().join("backup");
        fs::create_dir_all(&rootfs).unwrap();
        fs::create_dir_all(&backup).unwrap();
        backup_path(&rootfs, "/etc/hostname", &backup).unwrap();
        assert!(!backup.join("etc/hostname").exists());
    }

    #[test]
    fn restore_path_restores_file_from_backup() {
        let temp = TempDir::new().unwrap();
        let rootfs = temp.path().join("rootfs");
        let backup = temp.path().join("backup");
        let backup_root = backup.join(rootfs.strip_prefix("/").unwrap_or(rootfs.as_path()));
        let restored = rootfs.join("etc/hostname");
        let backup_src = backup_root.join("etc/hostname");

        fs::create_dir_all(restored.parent().unwrap()).unwrap();
        fs::create_dir_all(backup_src.parent().unwrap()).unwrap();
        fs::write(&backup_src, "restored-hostname").unwrap();

        restore_path(&rootfs, "/etc/hostname", &backup).unwrap();

        assert_eq!(fs::read_to_string(restored).unwrap(), "restored-hostname");
    }

    #[test]
    fn restore_path_nonexistent_backup_silently_skips() {
        let temp = TempDir::new().unwrap();
        let rootfs = temp.path().join("rootfs");
        let backup = temp.path().join("backup");
        fs::create_dir_all(&rootfs).unwrap();
        fs::create_dir_all(&backup).unwrap();

        restore_path(&rootfs, "/etc/hostname", &backup).unwrap();
        assert!(!rootfs.join("etc/hostname").exists());
    }

    #[test]
    fn restore_all_success_on_empty_list() {
        let temp = TempDir::new().unwrap();
        let rootfs = temp.path().join("rootfs");
        let backup = temp.path().join("backup");
        fs::create_dir_all(&rootfs).unwrap();
        fs::create_dir_all(&backup).unwrap();

        let result = restore_all(&rootfs, &[], &backup).unwrap();
        assert!(matches!(result, RestoreResult::Success));
    }

    #[test]
    fn restore_all_partial_failure_accumulates_context() {
        let temp = TempDir::new().unwrap();
        let rootfs = temp.path().join("rootfs");
        let backup = temp.path().join("backup");
        let backup_root = backup.join(rootfs.strip_prefix("/").unwrap_or(rootfs.as_path()));

        // Backup file exists
        fs::create_dir_all(backup_root.join("etc")).unwrap();
        fs::write(backup_root.join("etc/hostname"), "host").unwrap();

        // Place a regular file at rootfs/etc so create_dir_all(rootfs/etc) fails,
        // triggering RestoreFailed for the path
        fs::create_dir_all(&rootfs).unwrap();
        fs::write(rootfs.join("etc"), "not-a-dir").unwrap();

        let result = restore_all(&rootfs, &["/etc/hostname".to_string()], &backup).unwrap();
        assert!(matches!(result, RestoreResult::PartialFailure { .. }));
        if let RestoreResult::PartialFailure { context, .. } = result {
            assert!(context.contains("etc/hostname:restore"));
        }
    }
}
