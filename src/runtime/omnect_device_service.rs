//! omnect-device-service integration
//!
//! Creates runtime files that omnect-device-service reads at startup.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::Path;

use nix::unistd::{Gid, Uid, chown};
use serde::Serialize;

use crate::bootloader::{BootEnv, BootEnvKey};
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

/// BootEnv updated marker
const BOOTLOADER_UPDATED_FILE: &str = "omnect_bootloader_updated";

/// Name of the omnect-device-service user and group in the rootfs
const ODS_USER: &str = "omnect_device_service";
const ODS_GROUP: &str = "omnect_device_service";

/// File and directory permission modes for ODS runtime files.
#[derive(Debug, Clone, Copy)]
enum FilePermission {
    /// `rwxrwxr-x` (0o775) — readable and executable by all, writable by owner and group
    DirStandard,
    /// `rw-------` (0o600) — readable and writable only by owner (ODS)
    FileRestricted,
    /// `rw-r--r--` (0o644) — readable by all, writable only by owner (ODS)
    FileReadable,
}

impl FilePermission {
    fn bits(self) -> u32 {
        match self {
            Self::DirStandard => 0o775,
            Self::FileRestricted => 0o600,
            Self::FileReadable => 0o644,
        }
    }
}

/// BootEnv env value meaning the flag is set / requested
const BOOTLOADER_FLAG_SET: &str = "1";

/// BootEnv env value meaning update validation previously failed
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

    /// Carries the boot-env error cause when the env was unavailable on this
    /// boot, so ODS consumers can diagnose which tool or file failed rather
    /// than receiving an opaque signal. `None` on the happy path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_boot: Option<DegradedBootStatus>,

    /// `true` if this boot is the first boot since flashing (i.e. the
    /// `omnect_first_boot_done` marker was absent at run_init time).
    /// Always serialized — absence of the key would itself be a bug.
    pub first_boot: bool,

    /// Carries the resize-data failure cause when resize did not complete on
    /// this boot, so ODS consumers can diagnose and notify the cloud. `None`
    /// on the happy path (resize succeeded or guard already present).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resize_data: Option<ResizeStatus>,
}

/// Fsck status for a single partition
#[derive(Debug, Clone, Serialize)]
pub struct FsckStatus {
    /// Exit code from fsck
    pub code: i32,
    /// Output from fsck (may be compressed in bootloader)
    pub output: String,
}

/// Reason the boot env was unavailable on this boot.
///
/// Present only on release images where env-unavailable is treated as a
/// degraded-continue rather than a fatal abort. Debug images abort immediately,
/// so this struct is never populated there.
#[derive(Debug, Clone, Serialize)]
pub struct DegradedBootStatus {
    /// The `Display` of the underlying `BootEnvError`.
    pub reason: String,
}

/// Why resize-data did not complete on this boot.
///
/// Serialized as snake_case so ODS and cloud consumers can match exact strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResizeOutcome {
    /// Data partition fsck reported uncorrected errors before resize.
    SkippedFsck,
    /// An external tool (parted / sgdisk / resize2fs / sync) failed.
    ToolError,
    /// Layout problem (missing data partition, non-UTF-8 path, …) —
    /// the init setup step could not run at all.
    InvalidLayout,
}

/// Reason resize-data did not complete on this boot.
///
/// Present only when resize was attempted and could not finish.
/// `None` means resize succeeded or the guard was already present.
#[derive(Debug, Clone, Serialize)]
pub struct ResizeStatus {
    /// Why resize did not complete.
    pub outcome: ResizeOutcome,
    /// The `Display` of the underlying error — one line for operator diagnosis.
    pub reason: String,
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
    /// `true` once the destructive phase (partition reformat) began. On
    /// `status: Error` this distinguishes a safe no-op abort (nothing touched)
    /// from a failure after data was already wiped. Always serialized — absence
    /// of the key would itself be a bug (mirrors `OdsStatus.first_boot`).
    pub data_wiped: bool,
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

    /// Stores the boot-env error cause for ODS. `reason` is the `Display`
    /// of the `BootEnvError` returned by `open_boot_env()`.
    pub fn set_degraded_boot(&mut self, reason: String) {
        self.degraded_boot = Some(DegradedBootStatus { reason });
    }

    /// Record a resize-data failure indicator for ODS.
    pub fn set_resize_status(&mut self, status: ResizeStatus) {
        self.resize_data = Some(status);
    }
}

/// Create all runtime files for omnect-device-service
///
/// Files are written directly to the initramfs `/run` tmpfs. `switch_root`
/// moves that mount into the new root via `MS_MOVE`, so they remain visible
/// to ODS at the same path after the root pivot.
///
/// Ownership and permissions:
/// - dir: omnect_device_service:omnect_device_service, 775
/// - status JSON: 600
/// - trigger files: 644
/// - bootloader_updated: 600
pub fn create_ods_runtime_files(
    status: &OdsStatus,
    bootloader: Option<&dyn BootEnv>,
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
    set_mode(ods_dir, FilePermission::DirStandard)?;

    write_status_file(ods_dir, status)?;
    set_ownership(&ods_dir.join(ODS_STATUS_FILE), uid, gid)?;
    set_mode(
        &ods_dir.join(ODS_STATUS_FILE),
        FilePermission::FileRestricted,
    )?;

    // Skipped if the bootloader failed to initialise at runtime (e.g. corrupted boot partition).
    if let Some(bl) = bootloader {
        handle_update_validation(ods_dir, bl, uid, gid)?;
    } else {
        log::warn!(
            "update validation skipped — bootloader unavailable; \
             any in-flight A/B update will roll back on timer expiry"
        );
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
    bootloader: &dyn BootEnv,
    uid: Uid,
    gid: Gid,
) -> Result<()> {
    let validate_update = bootloader
        .get_env(BootEnvKey::ValidateUpdate)
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
                set_mode(&trigger_path, FilePermission::FileReadable)?;
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
                set_mode(&failed_path, FilePermission::FileReadable)?;
                log::warn!("Update validation failed marker created");
            }
            ValidateUpdateState::Other => {
                log::warn!("omnect_validate_update: unexpected value {value:?}");
            }
        }
    }

    let bootloader_updated = bootloader
        .get_env(BootEnvKey::BootloaderUpdated)
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
        set_mode(&marker_path, FilePermission::FileRestricted)?;
        log::info!("BootEnv update marker created");
    }

    Ok(())
}

/// Look up the UID for a user in the rootfs /etc/passwd.
fn lookup_uid(rootfs_dir: &Path, username: &str) -> Result<Uid> {
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
            return uid_str.parse::<u32>().map(Uid::from_raw).map_err(|e| {
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
fn lookup_gid(rootfs_dir: &Path, groupname: &str) -> Result<Gid> {
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
            return gid_str.parse::<u32>().map(Gid::from_raw).map_err(|e| {
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

fn set_ownership(path: &Path, uid: Uid, gid: Gid) -> Result<()> {
    chown(path, Some(uid), Some(gid)).map_err(|e| {
        InitramfsError::Io(std::io::Error::other(format!(
            "Failed to chown {}: {}",
            path.display(),
            e
        )))
    })
}

fn set_mode(path: &Path, mode: FilePermission) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode.bits())).map_err(|e| {
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

    fn current_uid() -> Uid {
        nix::unistd::getuid()
    }

    fn current_gid() -> Gid {
        nix::unistd::getgid()
    }

    /// Create a minimal rootfs with /etc/passwd and /etc/group for ODS user,
    /// using the current process's uid/gid so chown succeeds without root.
    fn make_fake_rootfs(uid: Uid, gid: Gid) -> TempDir {
        let rootfs = TempDir::new().unwrap();
        let etc = rootfs.path().join("etc");
        fs::create_dir_all(&etc).unwrap();
        fs::write(
            etc.join("passwd"),
            format!(
                "root:x:0:0:root:/root:/bin/sh\nomnect_device_service:x:{}:{}::/:/bin/sh\n",
                uid.as_raw(),
                gid.as_raw()
            ),
        )
        .unwrap();
        fs::write(
            etc.join("group"),
            format!("root:x:0:\nomnect_device_service:x:{}:\n", gid.as_raw()),
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
            data_wiped: true,
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
                data_wiped: false,
            };
            let json: Value = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
            assert_eq!(json["status"], *expected, "variant {:?}", variant);
        }
    }

    #[test]
    fn factory_reset_data_wiped_always_serialized() {
        // Absence of the key would itself be a bug (mirrors first_boot_always_serialized) —
        // ODS/cloud must be able to distinguish a safe pre-reformat abort (false) from a
        // failure after data was already wiped (true).
        for wiped in [false, true] {
            let s = FactoryResetStatus {
                status: FactoryResetStatusCode::Error,
                error: Some("test".to_string()),
                context: None,
                paths: vec![],
                data_wiped: wiped,
            };
            let json = serde_json::to_string(&s).unwrap();
            assert!(
                json.contains(&format!("\"data_wiped\":{wiped}")),
                "data_wiped={wiped} must be serialized; got: {json}"
            );
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
            crate::bootloader::create_mock_bootloader().with_env(BootEnvKey::ValidateUpdate, "1");

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
            .with_env(BootEnvKey::ValidateUpdate, "true");

        handle_update_validation(temp.path(), &bl, current_uid(), current_gid()).unwrap();

        assert!(!temp.path().join(UPDATE_VALIDATE_FILE).exists());
    }

    #[test]
    fn test_handle_update_validation_failed() {
        let temp = TempDir::new().unwrap();
        let bl = crate::bootloader::create_mock_bootloader()
            .with_env(BootEnvKey::ValidateUpdate, "failed");

        handle_update_validation(temp.path(), &bl, current_uid(), current_gid()).unwrap();

        assert!(!temp.path().join(UPDATE_VALIDATE_FILE).exists());
        assert!(temp.path().join(UPDATE_VALIDATE_FAILED_FILE).exists());
    }

    #[test]
    fn test_handle_update_validation_unexpected_value_creates_nothing() {
        let temp = TempDir::new().unwrap();
        let bl = crate::bootloader::create_mock_bootloader()
            .with_env(BootEnvKey::ValidateUpdate, "unexpected");

        handle_update_validation(temp.path(), &bl, current_uid(), current_gid()).unwrap();

        assert!(!temp.path().join(UPDATE_VALIDATE_FILE).exists());
        assert!(!temp.path().join(UPDATE_VALIDATE_FAILED_FILE).exists());
    }

    #[test]
    fn test_handle_update_validation_bootloader_updated() {
        let temp = TempDir::new().unwrap();
        let bl = crate::bootloader::create_mock_bootloader()
            .with_env(BootEnvKey::BootloaderUpdated, "1");

        handle_update_validation(temp.path(), &bl, current_uid(), current_gid()).unwrap();

        assert!(temp.path().join(BOOTLOADER_UPDATED_FILE).exists());
    }

    #[test]
    fn test_handle_update_validation_bootloader_updated_false_creates_nothing() {
        let temp = TempDir::new().unwrap();
        let bl = crate::bootloader::create_mock_bootloader()
            .with_env(BootEnvKey::BootloaderUpdated, "0");

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
            crate::bootloader::create_mock_bootloader().with_env(BootEnvKey::ValidateUpdate, "1");

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

    #[test]
    fn degraded_boot_serializes_only_when_set() {
        let status_normal = OdsStatus::new();
        let json_normal = serde_json::to_string(&status_normal).unwrap();
        assert!(
            !json_normal.contains("degraded_boot"),
            "degraded_boot must be absent when None; got: {json_normal}"
        );

        let mut status_degraded = OdsStatus::new();
        status_degraded.set_degraded_boot("grubenv missing".to_string());
        let json_degraded = serde_json::to_string(&status_degraded).unwrap();
        assert!(
            json_degraded.contains("\"degraded_boot\":{\"reason\":\"grubenv missing\"}"),
            "degraded_boot must be a nested object with reason; got: {json_degraded}"
        );
    }

    #[test]
    fn resize_status_absent_on_clean_run() {
        let status = OdsStatus::new();
        let json = serde_json::to_string(&status).unwrap();
        assert!(
            !json.contains("resize_data"),
            "resize_data must be absent when None; got: {json}"
        );
    }

    #[test]
    fn resize_status_present_with_outcome_and_reason() {
        let mut status = OdsStatus::new();
        status.set_resize_status(ResizeStatus {
            outcome: ResizeOutcome::SkippedFsck,
            reason: "data partition fsck reported uncorrected errors".to_string(),
        });
        let json = serde_json::to_string(&status).unwrap();
        assert!(
            json.contains("\"resize_data\""),
            "resize_data must be present when Some; got: {json}"
        );
        assert!(
            json.contains("\"outcome\":\"skipped_fsck\""),
            "outcome must serialize as snake_case; got: {json}"
        );
        assert!(
            json.contains("\"reason\""),
            "reason must be present; got: {json}"
        );
    }

    #[test]
    fn resize_outcome_variants_serialize_snake_case() {
        let cases: &[(ResizeOutcome, &str)] = &[
            (ResizeOutcome::SkippedFsck, "\"skipped_fsck\""),
            (ResizeOutcome::ToolError, "\"tool_error\""),
            (ResizeOutcome::InvalidLayout, "\"invalid_layout\""),
        ];
        for (variant, expected) in cases {
            let s = serde_json::to_string(variant).unwrap();
            assert_eq!(&s, expected, "{variant:?} must serialize as {expected}");
        }
    }

    #[test]
    fn first_boot_always_serialized() {
        // Plain bool, always in the JSON. Absence of the key would itself
        // be diagnostic of a bug — see spec §7.
        let s = OdsStatus::new();
        let j = serde_json::to_string(&s).unwrap();
        assert!(
            j.contains("\"first_boot\":false"),
            "default first_boot must be false and serialized; got: {j}"
        );

        let mut s = OdsStatus::new();
        s.first_boot = true;
        let j = serde_json::to_string(&s).unwrap();
        assert!(
            j.contains("\"first_boot\":true"),
            "first_boot=true must be serialized; got: {j}"
        );
    }
}
