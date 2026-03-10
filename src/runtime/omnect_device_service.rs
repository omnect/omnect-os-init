//! omnect-device-service integration
//!
//! Creates runtime files that omnect-device-service reads at startup.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::bootloader::Bootloader;
use crate::error::{InitramfsError, Result};

/// Directory for ODS runtime files.
/// Written to the initramfs /run tmpfs; switch_root moves /run into the new
/// root via MS_MOVE, so these files appear at the same path after boot.
const ODS_RUNTIME_DIR: &str = "/run/omnect-device-service";

/// Main status file name
const ODS_STATUS_FILE: &str = "omnect-os-initramfs.json";

/// Update validation trigger file
const UPDATE_VALIDATE_FILE: &str = "omnect_validate_update";

/// Failed update validation marker
const UPDATE_VALIDATE_FAILED_FILE: &str = "omnect_validate_update_failed";

/// Bootloader updated marker
const BOOTLOADER_UPDATED_FILE: &str = "omnect_bootloader_updated";

/// Factory reset status file (in /tmp)
const FACTORY_RESET_STATUS_FILE: &str = "/tmp/factory-reset.json";

/// Status information for omnect-device-service
#[derive(Debug, Clone, Default, Serialize)]
pub struct OdsStatus {
    /// Fsck results for each partition
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub fsck: HashMap<String, FsckStatus>,

    /// Factory reset status (if performed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub factory_reset: Option<FactoryResetStatus>,
}

/// Fsck status for a single partition
#[derive(Debug, Clone, Serialize)]
pub struct FsckStatus {
    /// Exit code from fsck
    pub code: i32,
    /// Output from fsck (may be compressed in bootloader)
    pub output: String,
}

/// Factory reset execution status
#[derive(Debug, Clone, Serialize)]
pub struct FactoryResetStatus {
    /// Status code: 0=success, 1=invalid, 2=error, 3=config_error
    pub status: u32,
    /// Error message if failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Additional context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Paths that were preserved
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

impl OdsStatus {
    /// Create a new empty status
    pub fn new() -> Self {
        Self::default()
    }

    /// Add fsck result for a partition
    pub fn add_fsck_result(&mut self, partition: &str, code: i32, output: String) {
        self.fsck
            .insert(partition.to_string(), FsckStatus { code, output });
    }

    /// Set factory reset status
    pub fn set_factory_reset(&mut self, status: FactoryResetStatus) {
        self.factory_reset = Some(status);
    }
}

/// Create all runtime files for omnect-device-service
///
/// Files are written directly to the initramfs `/run` tmpfs. `switch_root`
/// moves that mount into the new root via `MS_MOVE`, so they remain visible
/// to ODS at the same path after the root pivot.
pub fn create_ods_runtime_files(status: &OdsStatus, bootloader: &dyn Bootloader) -> Result<()> {
    let ods_dir = Path::new(ODS_RUNTIME_DIR);

    // Ensure directory exists
    fs::create_dir_all(ods_dir).map_err(|e| {
        InitramfsError::Io(std::io::Error::other(format!(
            "Failed to create ODS runtime dir: {}",
            e
        )))
    })?;

    // Write main status file
    write_status_file(ods_dir, status)?;

    // Handle update validation
    handle_update_validation(ods_dir, bootloader)?;

    // Copy factory reset status if exists
    copy_factory_reset_status(ods_dir)?;

    log::info!("Created ODS runtime files in {}", ods_dir.display());

    Ok(())
}

/// Write the main status JSON file
fn write_status_file(ods_dir: &Path, status: &OdsStatus) -> Result<()> {
    let status_path = ods_dir.join(ODS_STATUS_FILE);
    let json = serde_json::to_string_pretty(status).map_err(|e| {
        InitramfsError::Io(std::io::Error::other(format!(
            "Failed to serialize ODS status: {}",
            e
        )))
    })?;

    fs::write(&status_path, json)?;
    log::debug!("Wrote ODS status to {}", status_path.display());

    Ok(())
}

/// Handle update validation workflow
fn handle_update_validation(ods_dir: &Path, bootloader: &dyn Bootloader) -> Result<()> {
    // Check if update validation is requested
    let validate_update = bootloader.get_env("omnect_validate_update").unwrap_or(None);

    if let Some(value) = validate_update {
        if value == "1" || value.to_lowercase() == "true" {
            // Create trigger file for ODS
            let trigger_path = ods_dir.join(UPDATE_VALIDATE_FILE);
            fs::write(&trigger_path, "1")?;
            log::info!("Update validation requested - created trigger file");
        } else if value == "failed" {
            // Mark validation as failed
            let failed_path = ods_dir.join(UPDATE_VALIDATE_FAILED_FILE);
            fs::write(&failed_path, "1")?;
            log::warn!("Update validation failed marker created");
        }
    }

    // Check for bootloader updated flag
    let bootloader_updated = bootloader
        .get_env("omnect_bootloader_updated")
        .unwrap_or(None);

    if bootloader_updated.is_some() {
        let marker_path = ods_dir.join(BOOTLOADER_UPDATED_FILE);
        fs::write(&marker_path, "1")?;
        log::info!("Bootloader update marker created");
    }

    Ok(())
}

/// Copy factory reset status from /tmp if it exists
fn copy_factory_reset_status(ods_dir: &Path) -> Result<()> {
    let src = PathBuf::from(FACTORY_RESET_STATUS_FILE);

    if src.exists() {
        let dst = ods_dir.join("factory-reset.json");
        fs::copy(&src, &dst)?;
        log::debug!("Copied factory reset status to ODS dir");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_ods_status_default() {
        let status = OdsStatus::default();
        assert!(status.fsck.is_empty());
        assert!(status.factory_reset.is_none());
    }

    #[test]
    fn test_ods_status_add_fsck() {
        let mut status = OdsStatus::new();
        status.add_fsck_result("boot", 0, "clean".to_string());
        status.add_fsck_result("data", 1, "errors corrected".to_string());

        assert_eq!(status.fsck.len(), 2);
        assert_eq!(status.fsck.get("boot").unwrap().code, 0);
        assert_eq!(status.fsck.get("data").unwrap().code, 1);
    }

    #[test]
    fn test_ods_status_serialization() {
        let mut status = OdsStatus::new();
        status.add_fsck_result("boot", 0, "clean".to_string());

        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"boot\""));
        assert!(json.contains("\"code\":0"));
    }

    #[test]
    fn test_write_status_file() {
        let temp = TempDir::new().unwrap();
        let status = OdsStatus::new();

        write_status_file(temp.path(), &status).unwrap();

        let status_path = temp.path().join(ODS_STATUS_FILE);
        assert!(status_path.exists());

        let content = fs::read_to_string(status_path).unwrap();
        assert!(content.contains("{"));
    }

    #[test]
    fn test_factory_reset_status_serialization() {
        let status = FactoryResetStatus {
            status: 0,
            error: None,
            context: Some("normal".to_string()),
            paths: vec!["/etc/hostname".to_string()],
        };

        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"status\":0"));
        assert!(json.contains("\"paths\""));
    }
}
