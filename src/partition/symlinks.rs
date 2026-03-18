//! Symlink creation for /dev/omnect/*
//!
//! Creates symbolic links to partition devices for consistent access
//! regardless of underlying device type.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use crate::error::PartitionError;
use crate::partition::layout::partition_names;
use crate::partition::{PartitionLayout, Result};

/// Base directory for omnect device symlinks
const OMNECT_DEV_DIR: &str = "/dev/omnect";

/// Create all /dev/omnect/* symlinks for the given partition layout.
pub fn create_omnect_symlinks(layout: &PartitionLayout) -> Result<()> {
    create_symlink_dir()?;

    // Create symlink to the base block device
    create_symlink(&layout.device.base, &symlink_path(partition_names::ROOTBLK))?;

    // Create partition symlinks
    for (name, device_path) in &layout.partitions {
        create_symlink(device_path, &symlink_path(name))?;
    }

    // Create rootCurrent symlink pointing to the active root partition
    let root_current_target = layout.root_current();
    create_symlink(
        &root_current_target,
        &symlink_path(partition_names::ROOT_CURRENT),
    )?;

    log::info!(
        "Created /dev/omnect symlinks for {} device {}, rootCurrent -> {}",
        layout.table_type,
        layout.device.base.display(),
        root_current_target.display()
    );

    Ok(())
}

/// Remove all /dev/omnect/* symlinks
///
/// Useful for cleanup on error or re-detection.
pub fn remove_omnect_symlinks() -> Result<()> {
    let omnect_dir = Path::new(OMNECT_DEV_DIR);

    if !omnect_dir.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(omnect_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_symlink() {
            fs::remove_file(&path).map_err(|e| PartitionError::SymlinkRemoveFailed {
                path: path.clone(),
                reason: e.to_string(),
            })?;
        }
    }

    Ok(())
}

/// Create the /dev/omnect directory if it doesn't exist
fn create_symlink_dir() -> Result<()> {
    let omnect_dir = Path::new(OMNECT_DEV_DIR);

    if !omnect_dir.exists() {
        fs::create_dir_all(omnect_dir).map_err(|e| PartitionError::SymlinkFailed {
            link: omnect_dir.to_path_buf(),
            target: PathBuf::new(),
            reason: format!("Failed to create directory: {}", e),
        })?;
    }

    Ok(())
}

/// Get the full path for a symlink in /dev/omnect
fn symlink_path(name: &str) -> PathBuf {
    PathBuf::from(OMNECT_DEV_DIR).join(name)
}

/// Create a symlink, removing any existing symlink first
fn create_symlink(target: &Path, link: &Path) -> Result<()> {
    // Remove existing symlink if present. Plain directories cannot be replaced
    // by a symlink — flag them clearly. Symlinks pointing to directories are fine.
    if link.is_symlink() || link.exists() {
        if !link.is_symlink() && link.is_dir() {
            return Err(PartitionError::SymlinkRemoveFailed {
                path: link.to_path_buf(),
                reason: "path is a directory, cannot replace with symlink".to_string(),
            });
        }
        fs::remove_file(link).map_err(|e| PartitionError::SymlinkRemoveFailed {
            path: link.to_path_buf(),
            reason: e.to_string(),
        })?;
    }

    // Create the symlink
    symlink(target, link).map_err(|e| PartitionError::SymlinkFailed {
        link: link.to_path_buf(),
        target: target.to_path_buf(),
        reason: e.to_string(),
    })?;

    log::debug!(
        "Created symlink: {} -> {}",
        link.display(),
        target.display()
    );

    Ok(())
}

/// Verify that all expected symlinks exist and are valid
pub fn verify_symlinks(layout: &PartitionLayout) -> Result<()> {
    // Check rootblk
    verify_symlink(&symlink_path(partition_names::ROOTBLK), &layout.device.base)?;

    // Check all partitions
    for (name, device_path) in &layout.partitions {
        verify_symlink(&symlink_path(name), device_path)?;
    }

    // Check rootCurrent
    verify_symlink(
        &symlink_path(partition_names::ROOT_CURRENT),
        &layout.root_current(),
    )?;

    Ok(())
}

/// Verify a single symlink points to the expected target
fn verify_symlink(link: &Path, expected_target: &Path) -> Result<()> {
    if !link.is_symlink() {
        return Err(PartitionError::SymlinkFailed {
            link: link.to_path_buf(),
            target: expected_target.to_path_buf(),
            reason: "Symlink does not exist".to_string(),
        });
    }

    let actual_target = fs::read_link(link)?;
    if actual_target != expected_target {
        return Err(PartitionError::SymlinkFailed {
            link: link.to_path_buf(),
            target: expected_target.to_path_buf(),
            reason: format!(
                "Symlink points to {} instead of {}",
                actual_target.display(),
                expected_target.display()
            ),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_symlink_path() {
        assert_eq!(symlink_path("boot"), PathBuf::from("/dev/omnect/boot"));
        assert_eq!(symlink_path("rootA"), PathBuf::from("/dev/omnect/rootA"));
        assert_eq!(
            symlink_path("rootblk"),
            PathBuf::from("/dev/omnect/rootblk")
        );
    }

    #[test]
    fn test_create_symlink_in_temp_dir() {
        let temp_dir = TempDir::new().unwrap();
        let target = temp_dir.path().join("target_file");
        let link = temp_dir.path().join("link");

        // Create a target file
        fs::write(&target, "test").unwrap();

        // Create symlink
        let result = create_symlink(&target, &link);
        assert!(result.is_ok());

        // Verify symlink exists and points to target
        assert!(link.is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), target);
    }

    #[test]
    fn test_create_symlink_replaces_existing() {
        let temp_dir = TempDir::new().unwrap();
        let target1 = temp_dir.path().join("target1");
        let target2 = temp_dir.path().join("target2");
        let link = temp_dir.path().join("link");

        fs::write(&target1, "test1").unwrap();
        fs::write(&target2, "test2").unwrap();

        // Create first symlink
        create_symlink(&target1, &link).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), target1);

        // Replace with second symlink
        create_symlink(&target2, &link).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), target2);
    }

    #[test]
    fn test_verify_symlink_success() {
        let temp_dir = TempDir::new().unwrap();
        let target = temp_dir.path().join("target");
        let link = temp_dir.path().join("link");

        fs::write(&target, "test").unwrap();
        symlink(&target, &link).unwrap();

        let result = verify_symlink(&link, &target);
        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_symlink_wrong_target() {
        let temp_dir = TempDir::new().unwrap();
        let target1 = temp_dir.path().join("target1");
        let target2 = temp_dir.path().join("target2");
        let link = temp_dir.path().join("link");

        fs::write(&target1, "test1").unwrap();
        fs::write(&target2, "test2").unwrap();
        symlink(&target1, &link).unwrap();

        let result = verify_symlink(&link, &target2);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_symlink_not_exists() {
        let temp_dir = TempDir::new().unwrap();
        let link = temp_dir.path().join("nonexistent");
        let target = temp_dir.path().join("target");

        let result = verify_symlink(&link, &target);
        assert!(result.is_err());
    }
}
