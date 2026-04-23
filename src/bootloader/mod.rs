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
}

impl BootloaderEnvKey {
    /// Returns the env-var name as it is stored in the bootloader environment.
    pub fn as_str(&self) -> Cow<'static, str> {
        match self {
            Self::ValidateUpdate => Cow::Borrowed("omnect_validate_update"),
            Self::BootloaderUpdated => Cow::Borrowed("omnect_bootloader_updated"),
            Self::FsckStatus(p) => Cow::Owned(format!("omnect_fsck_{p}")),
        }
    }
}

/// Trait for bootloader environment access
///
/// This trait abstracts the differences between GRUB and U-Boot bootloader
/// environment access, allowing the rest of the codebase to work with
/// bootloader variables in a unified way.
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
    fn save_fsck_status(&mut self, partition: PartitionName, code: FsckExitCode, output: &str)
    -> Result<()>;

    /// Get fsck status from bootloader environment.
    ///
    /// Returns the decoded `FsckRecord` if a value is present,
    /// or `None` if no status was stored for this partition.
    fn get_fsck_status(&self, partition: PartitionName) -> Result<Option<FsckRecord>>;

    /// Clear fsck status from bootloader environment
    fn clear_fsck_status(&mut self, partition: PartitionName) -> Result<()>;
}

/// Creates the appropriate bootloader implementation based on the build-time feature flag.
///
/// The bootloader type is a build-time property of the target platform:
/// - `grub` feature: x86-64 EFI targets using GRUB (`grub-editenv`)
/// - `uboot` feature: ARM targets using U-Boot (`fw_printenv`/`fw_setenv`)
///
/// Exactly one of `grub` or `uboot` must be enabled; build.rs enforces this.
pub fn create_bootloader() -> Result<Box<dyn Bootloader>> {
    #[cfg(feature = "grub")]
    return Ok(Box::new(GrubBootloader::new()?));

    #[cfg(feature = "uboot")]
    return Ok(Box::new(UBootBootloader::new()?));
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
        self.fsck.insert(partition, FsckRecord { exit_code: code, output: output.to_string() });
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

    #[test]
    fn test_mock_bootloader_get_set() {
        let mut bl = MockBootloader::new();

        bl.set_env(BootloaderEnvKey::ValidateUpdate, Some("1")).unwrap();
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

        bl.save_fsck_status(PartitionName::Boot, FsckExitCode::CORRECTED, "errors corrected on pass 1")
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
