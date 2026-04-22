//! omnect-device-service integration
//!
//! Creates runtime files that omnect-device-service reads at startup.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use nix::unistd::{Gid, Uid, chown};
use serde::Serialize;

use crate::bootloader::{Bootloader, vars};
use crate::error::{InitramfsError, Result};
use crate::partition::PartitionName;

/// Directory for ODS runtime files.
/// Written to the initramfs /run tmpfs; switch_root moves /run into the new
/// root via MS_MOVE, so these files appear at the same path after boot.
pub const ODS_RUNTIME_DIR: &str = "/run/omnect-device-service";

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

/// Name of the omnect-device-service user and group in the rootfs
const ODS_USER: &str = "omnect_device_service";
const ODS_GROUP: &str = "omnect_device_service";

/// Permissions for the ODS runtime directory (rwxrwxr-x)
const DIR_MODE: u32 = 0o775;

/// Permissions for sensitive files readable only by ODS (rw-------)
const FILE_MODE_RESTRICTED: u32 = 0o600;

/// Permissions for trigger files readable by ODS and group (rw-r--r--)
const FILE_MODE_READABLE: u32 = 0o644;

/// Bootloader env value meaning the flag is set / requested
const BOOTLOADER_FLAG_SET: &str = "1";

/// Bootloader env value meaning update validation previously failed
const VALIDATE_UPDATE_FAILED_VALUE: &str = "failed";

/// Outcome codes for a factory reset operation.
///
/// Serialized as a plain integer so the JSON wire format that
/// `omnect-device-service` reads remains unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactoryResetStatusCode {
    Success = 0,
    Invalid = 1,
    Error = 2,
    ConfigError = 3,
}

impl serde::Serialize for FactoryResetStatusCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_u32(*self as u32)
    }
}

impl fmt::Display for FactoryResetStatusCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Success => write!(f, "success"),
            Self::Invalid => write!(f, "invalid"),
            Self::Error => write!(f, "error"),
            Self::ConfigError => write!(f, "config_error"),
        }
    }
}

/// Parsed value of the `omnect_validate_update` bootloader env variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidateUpdateState {
    /// Value `"1"` — update validation was requested before this boot.
    Requested,
    /// Value `"failed"` — the previous update validation failed.
    Failed,
    /// Any other value — no action required.
    Other,
}

impl From<&str> for ValidateUpdateState {
    fn from(s: &str) -> Self {
        match s {
            BOOTLOADER_FLAG_SET => Self::Requested,
            VALIDATE_UPDATE_FAILED_VALUE => Self::Failed,
            _ => Self::Other,
        }
    }
}

impl fmt::Display for ValidateUpdateState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Requested => write!(f, "requested"),
            Self::Failed => write!(f, "failed"),
            Self::Other => write!(f, "other"),
        }
    }
}

/// Status information for omnect-device-service
#[derive(Debug, Clone, Default, Serialize)]
pub struct OdsStatus {
    /// Fsck results for each partition
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub fsck: HashMap<PartitionName, FsckStatus>,

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
    /// Outcome of the factory reset operation.
    pub status: FactoryResetStatusCode,
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
    pub fn add_fsck_result(&mut self, partition: PartitionName, code: i32, output: String) {
        self.fsck.insert(partition, FsckStatus { code, output });
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
///
/// Ownership and permissions are set to match legacy bash:
/// - dir: omnect_device_service:omnect_device_service, 775
/// - status JSON: 600
/// - trigger files: 644
/// - bootloader_updated: 600
pub fn create_ods_runtime_files(
    status: &OdsStatus,
    bootloader: Option<&dyn Bootloader>,
    rootfs_dir: &Path,
    ods_dir: &Path,
) -> Result<()> {
    let uid = lookup_uid(rootfs_dir, ODS_USER)?;
    let gid = lookup_gid(rootfs_dir, ODS_GROUP)?;

    fs::create_dir_all(ods_dir).map_err(|e| {
        InitramfsError::Io(std::io::Error::other(format!(
            "Failed to create ODS runtime dir: {}",
            e
        )))
    })?;
    set_ownership(ods_dir, uid, gid)?;
    set_mode(ods_dir, DIR_MODE)?;

    write_status_file(ods_dir, status)?;
    set_ownership(&ods_dir.join(ODS_STATUS_FILE), uid, gid)?;
    set_mode(&ods_dir.join(ODS_STATUS_FILE), FILE_MODE_RESTRICTED)?;

    // Skipped if the bootloader failed to initialise at runtime (e.g. corrupted boot partition).
    if let Some(bl) = bootloader {
        handle_update_validation(ods_dir, bl, uid, gid)?;
    }

    // Copy factory reset status if exists
    if let Some(dst) = copy_factory_reset_status(ods_dir)? {
        set_ownership(&dst, uid, gid)?;
        set_mode(&dst, FILE_MODE_RESTRICTED)?;
    }

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

    fs::write(&status_path, json).map_err(|e| {
        InitramfsError::Io(std::io::Error::other(format!(
            "Failed to write ODS status to {}: {}",
            status_path.display(),
            e
        )))
    })?;
    log::debug!("Wrote ODS status to {}", status_path.display());

    Ok(())
}

/// Handle update validation workflow; applies ownership and permissions to any
/// trigger files it creates.
fn handle_update_validation(
    ods_dir: &Path,
    bootloader: &dyn Bootloader,
    uid: u32,
    gid: u32,
) -> Result<()> {
    let validate_update = bootloader
        .get_env(vars::OMNECT_VALIDATE_UPDATE)
        .map_err(|e| {
            InitramfsError::Io(std::io::Error::other(format!(
                "failed to read omnect_validate_update from bootloader: {e}"
            )))
        })?;

    if let Some(value) = validate_update {
        let state = ValidateUpdateState::from(value.as_str());
        log::debug!("omnect_validate_update: {state}");
        match state {
            ValidateUpdateState::Requested => {
                let trigger_path = ods_dir.join(UPDATE_VALIDATE_FILE);
                fs::write(&trigger_path, BOOTLOADER_FLAG_SET).map_err(|e| {
                    InitramfsError::Io(std::io::Error::other(format!(
                        "Failed to write {}: {}",
                        trigger_path.display(),
                        e
                    )))
                })?;
                set_ownership(&trigger_path, uid, gid)?;
                set_mode(&trigger_path, FILE_MODE_READABLE)?;
                log::info!("Update validation requested - created trigger file");
            }
            ValidateUpdateState::Failed => {
                let failed_path = ods_dir.join(UPDATE_VALIDATE_FAILED_FILE);
                fs::write(&failed_path, BOOTLOADER_FLAG_SET).map_err(|e| {
                    InitramfsError::Io(std::io::Error::other(format!(
                        "Failed to write {}: {}",
                        failed_path.display(),
                        e
                    )))
                })?;
                set_ownership(&failed_path, uid, gid)?;
                set_mode(&failed_path, FILE_MODE_READABLE)?;
                log::warn!("Update validation failed marker created");
            }
            ValidateUpdateState::Other => {}
        }
    }

    let bootloader_updated = bootloader
        .get_env(vars::OMNECT_BOOTLOADER_UPDATED)
        .map_err(|e| {
            InitramfsError::Io(std::io::Error::other(format!(
                "failed to read omnect_bootloader_updated from bootloader: {e}"
            )))
        })?;

    if let Some(value) = bootloader_updated
        && value == BOOTLOADER_FLAG_SET
    {
        let marker_path = ods_dir.join(BOOTLOADER_UPDATED_FILE);
        fs::write(&marker_path, BOOTLOADER_FLAG_SET).map_err(|e| {
            InitramfsError::Io(std::io::Error::other(format!(
                "Failed to write {}: {}",
                marker_path.display(),
                e
            )))
        })?;
        set_ownership(&marker_path, uid, gid)?;
        set_mode(&marker_path, FILE_MODE_RESTRICTED)?;
        log::info!("Bootloader update marker created");
    }

    Ok(())
}

/// Copy factory reset status from /tmp if it exists.
/// Returns the destination path if the file was copied.
fn copy_factory_reset_status(ods_dir: &Path) -> Result<Option<PathBuf>> {
    let src = PathBuf::from(FACTORY_RESET_STATUS_FILE);

    if !src.exists() {
        return Ok(None);
    }

    let dst = ods_dir.join("factory-reset.json");
    fs::copy(&src, &dst).map_err(|e| {
        InitramfsError::Io(std::io::Error::other(format!(
            "Failed to copy {} to {}: {}",
            src.display(),
            dst.display(),
            e
        )))
    })?;
    log::debug!("Copied factory reset status to ODS dir");

    Ok(Some(dst))
}

/// Look up the numeric UID for a user in the rootfs /etc/passwd.
fn lookup_uid(rootfs_dir: &Path, username: &str) -> Result<u32> {
    let passwd = rootfs_dir.join("etc/passwd");
    let content = fs::read_to_string(&passwd).map_err(|e| {
        InitramfsError::Io(std::io::Error::other(format!(
            "Failed to read {}: {}",
            passwd.display(),
            e
        )))
    })?;
    for line in content.lines() {
        let mut fields = line.splitn(7, ':');
        let name = fields.next().unwrap_or("");
        if name != username {
            continue;
        }
        let _password = fields.next();
        if let Some(uid_str) = fields.next() {
            return uid_str.parse::<u32>().map_err(|e| {
                InitramfsError::Io(std::io::Error::other(format!(
                    "Invalid UID for {}: {}",
                    username, e
                )))
            });
        }
    }
    Err(InitramfsError::Io(std::io::Error::other(format!(
        "user {} not found in {}",
        username,
        passwd.display()
    ))))
}

/// Look up the numeric GID for a group in the rootfs /etc/group.
fn lookup_gid(rootfs_dir: &Path, groupname: &str) -> Result<u32> {
    let group = rootfs_dir.join("etc/group");
    let content = fs::read_to_string(&group).map_err(|e| {
        InitramfsError::Io(std::io::Error::other(format!(
            "Failed to read {}: {}",
            group.display(),
            e
        )))
    })?;
    for line in content.lines() {
        let mut fields = line.splitn(4, ':');
        let name = fields.next().unwrap_or("");
        if name != groupname {
            continue;
        }
        let _password = fields.next();
        if let Some(gid_str) = fields.next() {
            return gid_str.parse::<u32>().map_err(|e| {
                InitramfsError::Io(std::io::Error::other(format!(
                    "Invalid GID for {}: {}",
                    groupname, e
                )))
            });
        }
    }
    Err(InitramfsError::Io(std::io::Error::other(format!(
        "group {} not found in {}",
        groupname,
        group.display()
    ))))
}

fn set_ownership(path: &Path, uid: u32, gid: u32) -> Result<()> {
    chown(path, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid))).map_err(|e| {
        InitramfsError::Io(std::io::Error::other(format!(
            "Failed to chown {}: {}",
            path.display(),
            e
        )))
    })
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|e| {
        InitramfsError::Io(std::io::Error::other(format!(
            "Failed to chmod {}: {}",
            path.display(),
            e
        )))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::partition::PartitionName;
    use tempfile::TempDir;

    fn current_uid() -> u32 {
        nix::unistd::getuid().as_raw()
    }

    fn current_gid() -> u32 {
        nix::unistd::getgid().as_raw()
    }

    /// Create a minimal rootfs with /etc/passwd and /etc/group for ODS user,
    /// using the current process's uid/gid so chown succeeds without root.
    fn make_fake_rootfs(uid: u32, gid: u32) -> TempDir {
        let rootfs = TempDir::new().unwrap();
        let etc = rootfs.path().join("etc");
        fs::create_dir_all(&etc).unwrap();
        fs::write(
            etc.join("passwd"),
            format!(
                "root:x:0:0:root:/root:/bin/sh\nomnect_device_service:x:{uid}:{gid}::/:/bin/sh\n"
            ),
        )
        .unwrap();
        fs::write(
            etc.join("group"),
            format!("root:x:0:\nomnect_device_service:x:{gid}:\n"),
        )
        .unwrap();
        rootfs
    }

    #[test]
    fn test_ods_status_default() {
        let status = OdsStatus::default();
        assert!(status.fsck.is_empty());
        assert!(status.factory_reset.is_none());
    }

    #[test]
    fn test_ods_status_add_fsck() {
        let mut status = OdsStatus::new();
        status.add_fsck_result(PartitionName::Boot, 0, "clean".to_string());
        status.add_fsck_result(PartitionName::Data, 1, "errors corrected".to_string());

        assert_eq!(status.fsck.len(), 2);
        assert_eq!(status.fsck.get(&PartitionName::Boot).unwrap().code, 0);
        assert_eq!(status.fsck.get(&PartitionName::Data).unwrap().code, 1);
    }

    #[test]
    fn test_ods_status_serialization() {
        let mut status = OdsStatus::new();
        status.add_fsck_result(PartitionName::Boot, 0, "clean".to_string());

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
            status: FactoryResetStatusCode::Success,
            error: None,
            context: Some("normal".to_string()),
            paths: vec!["/etc/hostname".to_string()],
        };

        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"status\":0"));
        assert!(json.contains("\"paths\""));
    }

    #[test]
    fn test_factory_reset_status_code_serializes_as_integer() {
        use serde_json::Value;
        let cases: &[(FactoryResetStatusCode, u64)] = &[
            (FactoryResetStatusCode::Success, 0),
            (FactoryResetStatusCode::Invalid, 1),
            (FactoryResetStatusCode::Error, 2),
            (FactoryResetStatusCode::ConfigError, 3),
        ];
        for (variant, expected) in cases {
            let s = FactoryResetStatus {
                status: *variant,
                error: None,
                context: None,
                paths: vec![],
            };
            let json: Value = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
            assert_eq!(json["status"], *expected, "variant {:?}", variant);
        }
    }

    #[test]
    fn test_factory_reset_status_code_display() {
        assert_eq!(FactoryResetStatusCode::Success.to_string(), "success");
        assert_eq!(FactoryResetStatusCode::Invalid.to_string(), "invalid");
        assert_eq!(FactoryResetStatusCode::Error.to_string(), "error");
        assert_eq!(
            FactoryResetStatusCode::ConfigError.to_string(),
            "config_error"
        );
    }

    #[test]
    fn test_validate_update_state_from_str() {
        assert_eq!(
            ValidateUpdateState::from("1"),
            ValidateUpdateState::Requested
        );
        assert_eq!(
            ValidateUpdateState::from("failed"),
            ValidateUpdateState::Failed
        );
        assert_eq!(
            ValidateUpdateState::from("true"),
            ValidateUpdateState::Other
        );
        assert_eq!(ValidateUpdateState::from("0"), ValidateUpdateState::Other);
        assert_eq!(ValidateUpdateState::from(""), ValidateUpdateState::Other);
        assert_eq!(
            ValidateUpdateState::from("unexpected"),
            ValidateUpdateState::Other
        );
    }

    #[test]
    fn test_validate_update_state_display() {
        assert_eq!(ValidateUpdateState::Requested.to_string(), "requested");
        assert_eq!(ValidateUpdateState::Failed.to_string(), "failed");
        assert_eq!(ValidateUpdateState::Other.to_string(), "other");
    }

    #[test]
    fn test_handle_update_validation_value_1() {
        let temp = TempDir::new().unwrap();
        let bl =
            crate::bootloader::create_mock_bootloader().with_env(vars::OMNECT_VALIDATE_UPDATE, "1");

        handle_update_validation(temp.path(), &bl, current_uid(), current_gid()).unwrap();

        assert!(temp.path().join(UPDATE_VALIDATE_FILE).exists());
        assert!(!temp.path().join(UPDATE_VALIDATE_FAILED_FILE).exists());
        assert!(!temp.path().join(BOOTLOADER_UPDATED_FILE).exists());
    }

    #[test]
    fn test_handle_update_validation_value_true_not_accepted() {
        // Only "1" is a valid truthy value; "true" must not create the trigger file.
        let temp = TempDir::new().unwrap();
        let bl = crate::bootloader::create_mock_bootloader()
            .with_env(vars::OMNECT_VALIDATE_UPDATE, "true");

        handle_update_validation(temp.path(), &bl, current_uid(), current_gid()).unwrap();

        assert!(!temp.path().join(UPDATE_VALIDATE_FILE).exists());
    }

    #[test]
    fn test_handle_update_validation_failed() {
        let temp = TempDir::new().unwrap();
        let bl = crate::bootloader::create_mock_bootloader()
            .with_env(vars::OMNECT_VALIDATE_UPDATE, "failed");

        handle_update_validation(temp.path(), &bl, current_uid(), current_gid()).unwrap();

        assert!(!temp.path().join(UPDATE_VALIDATE_FILE).exists());
        assert!(temp.path().join(UPDATE_VALIDATE_FAILED_FILE).exists());
    }

    #[test]
    fn test_handle_update_validation_unexpected_value_creates_nothing() {
        let temp = TempDir::new().unwrap();
        let bl = crate::bootloader::create_mock_bootloader()
            .with_env(vars::OMNECT_VALIDATE_UPDATE, "unexpected");

        handle_update_validation(temp.path(), &bl, current_uid(), current_gid()).unwrap();

        assert!(!temp.path().join(UPDATE_VALIDATE_FILE).exists());
        assert!(!temp.path().join(UPDATE_VALIDATE_FAILED_FILE).exists());
    }

    #[test]
    fn test_handle_update_validation_bootloader_updated() {
        let temp = TempDir::new().unwrap();
        let bl = crate::bootloader::create_mock_bootloader()
            .with_env(vars::OMNECT_BOOTLOADER_UPDATED, "1");

        handle_update_validation(temp.path(), &bl, current_uid(), current_gid()).unwrap();

        assert!(temp.path().join(BOOTLOADER_UPDATED_FILE).exists());
    }

    #[test]
    fn test_handle_update_validation_bootloader_updated_false_creates_nothing() {
        let temp = TempDir::new().unwrap();
        let bl = crate::bootloader::create_mock_bootloader()
            .with_env(vars::OMNECT_BOOTLOADER_UPDATED, "0");

        handle_update_validation(temp.path(), &bl, current_uid(), current_gid()).unwrap();

        assert!(!temp.path().join(BOOTLOADER_UPDATED_FILE).exists());
    }

    #[test]
    fn test_handle_update_validation_no_env_creates_nothing() {
        let temp = TempDir::new().unwrap();
        let bl = crate::bootloader::create_mock_bootloader();

        handle_update_validation(temp.path(), &bl, current_uid(), current_gid()).unwrap();

        assert!(!temp.path().join(UPDATE_VALIDATE_FILE).exists());
        assert!(!temp.path().join(UPDATE_VALIDATE_FAILED_FILE).exists());
        assert!(!temp.path().join(BOOTLOADER_UPDATED_FILE).exists());
    }

    #[test]
    fn test_lookup_uid_and_gid() {
        let uid = current_uid();
        let gid = current_gid();
        let rootfs = make_fake_rootfs(uid, gid);

        assert_eq!(lookup_uid(rootfs.path(), ODS_USER).unwrap(), uid);
        assert_eq!(lookup_gid(rootfs.path(), ODS_GROUP).unwrap(), gid);
    }

    #[test]
    fn test_lookup_uid_missing_user() {
        let rootfs = TempDir::new().unwrap();
        fs::create_dir_all(rootfs.path().join("etc")).unwrap();
        fs::write(
            rootfs.path().join("etc/passwd"),
            "root:x:0:0::/root:/bin/sh\n",
        )
        .unwrap();

        assert!(lookup_uid(rootfs.path(), ODS_USER).is_err());
    }

    #[test]
    fn test_lookup_gid_missing_group() {
        let rootfs = TempDir::new().unwrap();
        fs::create_dir_all(rootfs.path().join("etc")).unwrap();
        fs::write(rootfs.path().join("etc/group"), "root:x:0:\n").unwrap();

        assert!(lookup_gid(rootfs.path(), ODS_GROUP).is_err());
    }

    #[test]
    fn test_create_ods_runtime_files_end_to_end() {
        let uid = current_uid();
        let gid = current_gid();
        let rootfs = make_fake_rootfs(uid, gid);
        let ods_dir = TempDir::new().unwrap();

        let mut status = OdsStatus::new();
        status.add_fsck_result(PartitionName::Boot, 0, "clean".to_string());

        let bl =
            crate::bootloader::create_mock_bootloader().with_env(vars::OMNECT_VALIDATE_UPDATE, "1");

        create_ods_runtime_files(&status, Some(&bl), rootfs.path(), ods_dir.path()).unwrap();

        // Status JSON written and non-empty
        let status_file = ods_dir.path().join(ODS_STATUS_FILE);
        assert!(status_file.exists());
        let content = fs::read_to_string(&status_file).unwrap();
        assert!(content.contains("\"boot\""));

        // Update validation trigger created
        assert!(ods_dir.path().join(UPDATE_VALIDATE_FILE).exists());

        // No bootloader-updated marker
        assert!(!ods_dir.path().join(BOOTLOADER_UPDATED_FILE).exists());
    }

    #[test]
    fn test_create_ods_runtime_files_no_bootloader() {
        let uid = current_uid();
        let gid = current_gid();
        let rootfs = make_fake_rootfs(uid, gid);
        let ods_dir = TempDir::new().unwrap();

        create_ods_runtime_files(&OdsStatus::new(), None, rootfs.path(), ods_dir.path()).unwrap();

        assert!(ods_dir.path().join(ODS_STATUS_FILE).exists());
        assert!(!ods_dir.path().join(UPDATE_VALIDATE_FILE).exists());
    }
}
