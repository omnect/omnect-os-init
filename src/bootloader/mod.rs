//! Bootloader abstraction module
//!
//! This module provides a trait-based abstraction over different bootloaders
//! (GRUB and U-Boot) to allow unified access to bootloader environment variables.

#[cfg(feature = "grub")]
mod grub;
mod types;
#[cfg(feature = "uboot")]
mod uboot;

use std::borrow::Cow;

use crate::error::BootloaderError;
use crate::filesystem::FsckExitCode;
use crate::partition::PartitionName;

#[cfg(feature = "grub")]
pub use self::grub::GrubBootloader;
#[cfg(feature = "uboot")]
pub use self::uboot::UBootBootloader;

pub type Result<T> = std::result::Result<T, BootloaderError>;

/// Decoded fsck result stored in the bootloader environment.
#[derive(Debug, Clone, PartialEq)]
pub struct FsckRecord {
    /// Typed exit code from fsck.
    pub exit_code: FsckExitCode,
    /// Combined stdout + stderr output from fsck.
    pub output: String,
}

/// Typed key for bootloader environment variables.
///
/// Use this instead of raw `&str` keys to prevent typos and make all
/// known env-var names visible in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootloaderEnvKey {
    /// `omnect_validate_update` — OTA update validation state.
    ValidateUpdate,
    /// `omnect_bootloader_updated` — whether the bootloader itself was updated.
    BootloaderUpdated,
    /// `omnect_fsck_<partition>` — fsck result for the given partition.
    FsckStatus(PartitionName),
    /// `omnect_resized_data` — set to `"1"` after data partition has been resized.
    #[cfg(feature = "resize-data")]
    ResizedData,
}

impl BootloaderEnvKey {
    /// Returns the env-var name as it is stored in the bootloader environment.
    pub fn as_str(&self) -> Cow<'static, str> {
        match self {
            Self::ValidateUpdate => Cow::Borrowed("omnect_validate_update"),
            Self::BootloaderUpdated => Cow::Borrowed("omnect_bootloader_updated"),
            Self::FsckStatus(p) => Cow::Owned(format!("omnect_fsck_{p}")),
            #[cfg(feature = "resize-data")]
            Self::ResizedData => Cow::Borrowed("omnect_resized_data"),
        }
    }
}

/// Trait for bootloader environment access
///
/// This trait abstracts the differences between GRUB and U-Boot bootloader
/// environment access, allowing the rest of the codebase to work with
/// bootloader variables in a unified way.
///
/// The minimal required interface is `get_env` and `set_env`. The fsck helpers
/// (`save_fsck_status`, `get_fsck_status`, `clear_fsck_status`) have default
/// implementations that encode/decode via `get_env`/`set_env`. Bootloader
/// backends with custom storage strategies (e.g. GRUB's per-partition files)
/// should override the relevant methods.
pub trait Bootloader: Send + Sync {
    /// Get the value of a bootloader environment variable
    ///
    /// Returns `Ok(None)` if the variable doesn't exist.
    /// Returns `Err` if there was an error accessing the bootloader environment.
    fn get_env(&self, key: BootloaderEnvKey) -> Result<Option<String>>;

    /// Set or delete a bootloader environment variable
    ///
    /// Pass `Some(value)` to set the variable, or `None` to delete it.
    fn set_env(&mut self, key: BootloaderEnvKey, value: Option<&str>) -> Result<()>;

    /// Save fsck result to bootloader environment.
    ///
    /// Stores exit code and full fsck output as gzip+base64 encoded string so the
    /// diagnostic text survives the reboot required after fsck corrects errors.
    fn save_fsck_status(
        &mut self,
        partition: PartitionName,
        code: FsckExitCode,
        output: &str,
    ) -> Result<()> {
        let encoded = types::encode_fsck_output(code.bits(), output);
        self.set_env(BootloaderEnvKey::FsckStatus(partition), Some(&encoded))
    }

    /// Get fsck status from bootloader environment.
    ///
    /// Returns the decoded `FsckRecord` if a value is present,
    /// or `None` if no status was stored for this partition.
    fn get_fsck_status(&self, partition: PartitionName) -> Result<Option<FsckRecord>> {
        Ok(self
            .get_env(BootloaderEnvKey::FsckStatus(partition))?
            .and_then(|v| types::decode_fsck_output(&v)))
    }

    /// Clear fsck status from bootloader environment
    fn clear_fsck_status(&mut self, partition: PartitionName) -> Result<()> {
        self.set_env(BootloaderEnvKey::FsckStatus(partition), None)
    }
}

/// Opens the appropriate bootloader environment implementation based on the build-time feature flag.
///
/// The bootloader type is a build-time property of the target platform:
/// - `grub` feature: x86-64 EFI targets using GRUB (`grub-editenv`)
/// - `uboot` feature: ARM targets using U-Boot (`fw_printenv`/`fw_setenv`)
///
/// Exactly one of `grub` or `uboot` must be enabled; build.rs enforces this.
pub fn open_bootloader_env() -> Result<Box<dyn Bootloader>> {
    #[cfg(feature = "grub")]
    return Ok(Box::new(GrubBootloader::new()?));

    #[cfg(feature = "uboot")]
    return Ok(Box::new(UBootBootloader::new()?));
}

/// The result of a bootloader availability check.
pub enum BootloaderEnv {
    /// Bootloader environment opened successfully.
    Available(Box<dyn Bootloader>),
    /// Bootloader environment could not be opened.
    Degraded(BootloaderError),
}

impl BootloaderEnv {
    /// Returns `true` if the bootloader environment is unavailable.
    pub fn is_degraded(&self) -> bool {
        matches!(self, Self::Degraded(_))
    }

    /// Returns a mutable reference to the bootloader if available.
    pub fn available_mut(&mut self) -> Option<&mut dyn Bootloader> {
        match self {
            Self::Available(b) => Some(b.as_mut()),
            Self::Degraded(_) => None,
        }
    }

    /// Returns a shared reference to the bootloader if available.
    pub fn available(&self) -> Option<&dyn Bootloader> {
        match self {
            Self::Available(b) => Some(b.as_ref()),
            Self::Degraded(_) => None,
        }
    }
}

/// The outcome of `classify_bootloader`.
pub enum BootloaderDecision {
    /// Continue init with this bootloader env. The bool is `true` iff degraded.
    Continue(BootloaderEnv, bool),
    /// Abort init with this error — caller passes it to `handle_fatal_error`.
    Abort(crate::error::InitramfsError),
}

/// Decide how to proceed based on the bootloader open result and the image type.
///
/// - `Ok(bl)` → `Continue(Available(bl), false)` — normal boot, both image types.
/// - `Err(e)` + release-image → `Continue(Degraded(e), true)` — degraded boot continues.
/// - `Err(e)` + debug-image → `Abort(DegradedBoot(e))` — enter debug shell immediately.
pub fn classify_bootloader(
    open_result: std::result::Result<Box<dyn Bootloader>, BootloaderError>,
    is_release_image: bool,
) -> BootloaderDecision {
    match open_result {
        Ok(bl) => BootloaderDecision::Continue(BootloaderEnv::Available(bl), false),
        Err(e) if is_release_image => {
            BootloaderDecision::Continue(BootloaderEnv::Degraded(e), true)
        }
        Err(e) => BootloaderDecision::Abort(crate::error::InitramfsError::DegradedBoot(e)),
    }
}

/// Create a mock bootloader for testing
#[cfg(test)]
pub fn create_mock_bootloader() -> MockBootloader {
    MockBootloader::new()
}

/// Mock bootloader for testing
#[cfg(test)]
#[derive(Default)]
pub struct MockBootloader {
    env: std::collections::HashMap<String, String>,
    /// fsck results stored as typed records — no subprocess encoding needed in tests.
    fsck: std::collections::HashMap<PartitionName, FsckRecord>,
    /// Keys passed to set_env, in call order. Used by tests to verify set_env was/wasn't called.
    pub set_env_calls: Vec<BootloaderEnvKey>,
}

#[cfg(test)]
impl MockBootloader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_env(mut self, key: BootloaderEnvKey, value: &str) -> Self {
        self.env.insert(key.as_str().to_string(), value.to_string());
        self
    }
}

#[cfg(test)]
impl Bootloader for MockBootloader {
    fn get_env(&self, key: BootloaderEnvKey) -> Result<Option<String>> {
        Ok(self.env.get(key.as_str().as_ref()).cloned())
    }

    fn set_env(&mut self, key: BootloaderEnvKey, value: Option<&str>) -> Result<()> {
        self.set_env_calls.push(key);
        match value {
            Some(v) => {
                self.env.insert(key.as_str().to_string(), v.to_string());
            }
            None => {
                self.env.remove(key.as_str().as_ref());
            }
        }
        Ok(())
    }

    fn save_fsck_status(
        &mut self,
        partition: PartitionName,
        code: FsckExitCode,
        output: &str,
    ) -> Result<()> {
        self.fsck.insert(
            partition,
            FsckRecord {
                exit_code: code,
                output: output.to_string(),
            },
        );
        Ok(())
    }

    fn get_fsck_status(&self, partition: PartitionName) -> Result<Option<FsckRecord>> {
        Ok(self.fsck.get(&partition).cloned())
    }

    fn clear_fsck_status(&mut self, partition: PartitionName) -> Result<()> {
        self.fsck.remove(&partition);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod classify_tests {
        use super::*;
        use crate::error::{BootloaderError, InitramfsError};

        fn ok_bootloader() -> std::result::Result<Box<dyn Bootloader>, BootloaderError> {
            Ok(Box::new(MockBootloader::new()))
        }

        fn err_bootloader() -> std::result::Result<Box<dyn Bootloader>, BootloaderError> {
            Err(BootloaderError::CommandFailed {
                command: "grub-editenv".into(),
                reason: "not found".into(),
            })
        }

        #[test]
        fn ok_release_image_returns_available_not_degraded() {
            let decision = classify_bootloader(ok_bootloader(), true);
            assert!(matches!(
                decision,
                BootloaderDecision::Continue(BootloaderEnv::Available(_), false)
            ));
        }

        #[test]
        fn ok_debug_image_returns_available_not_degraded() {
            let decision = classify_bootloader(ok_bootloader(), false);
            assert!(matches!(
                decision,
                BootloaderDecision::Continue(BootloaderEnv::Available(_), false)
            ));
        }

        #[test]
        fn err_release_image_returns_degraded_continue() {
            let decision = classify_bootloader(err_bootloader(), true);
            assert!(matches!(
                decision,
                BootloaderDecision::Continue(BootloaderEnv::Degraded(_), true)
            ));
        }

        #[test]
        fn err_debug_image_returns_abort_with_degraded_boot_error() {
            let decision = classify_bootloader(err_bootloader(), false);
            assert!(matches!(
                decision,
                BootloaderDecision::Abort(InitramfsError::DegradedBoot(_))
            ));
        }
    }

    #[test]
    fn test_mock_bootloader_get_set() {
        let mut bl = MockBootloader::new();

        bl.set_env(BootloaderEnvKey::ValidateUpdate, Some("1"))
            .unwrap();
        assert_eq!(
            bl.get_env(BootloaderEnvKey::ValidateUpdate).unwrap(),
            Some("1".to_string())
        );

        bl.set_env(BootloaderEnvKey::ValidateUpdate, None).unwrap();
        assert_eq!(bl.get_env(BootloaderEnvKey::ValidateUpdate).unwrap(), None);
    }

    #[test]
    fn test_mock_bootloader_with_env() {
        let bl = MockBootloader::new()
            .with_env(BootloaderEnvKey::ValidateUpdate, "1")
            .with_env(BootloaderEnvKey::BootloaderUpdated, "0");

        assert_eq!(
            bl.get_env(BootloaderEnvKey::ValidateUpdate).unwrap(),
            Some("1".to_string())
        );
        assert_eq!(
            bl.get_env(BootloaderEnvKey::BootloaderUpdated).unwrap(),
            Some("0".to_string())
        );
        assert_eq!(
            bl.get_env(BootloaderEnvKey::FsckStatus(PartitionName::Boot))
                .unwrap(),
            None
        );
    }

    #[test]
    fn test_mock_bootloader_fsck_status() {
        use crate::partition::PartitionName;
        let mut bl = MockBootloader::new();

        bl.save_fsck_status(
            PartitionName::Boot,
            FsckExitCode::CORRECTED,
            "errors corrected on pass 1",
        )
        .unwrap();

        let retrieved = bl.get_fsck_status(PartitionName::Boot).unwrap();
        assert_eq!(
            retrieved,
            Some(FsckRecord {
                exit_code: FsckExitCode::CORRECTED,
                output: "errors corrected on pass 1".to_string()
            })
        );

        bl.clear_fsck_status(PartitionName::Boot).unwrap();
        assert_eq!(bl.get_fsck_status(PartitionName::Boot).unwrap(), None);
    }
}
