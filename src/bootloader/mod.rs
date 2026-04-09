//! Bootloader abstraction module
//!
//! This module provides a trait-based abstraction over different bootloaders
//! (GRUB and U-Boot) to allow unified access to bootloader environment variables.

#[cfg(feature = "grub")]
mod grub;
mod types;
#[cfg(feature = "uboot")]
mod uboot;

use std::path::Path;

use crate::error::BootloaderError;

#[cfg(feature = "grub")]
pub use self::grub::GrubBootloader;
#[cfg(feature = "uboot")]
pub use self::uboot::UBootBootloader;

pub type Result<T> = std::result::Result<T, BootloaderError>;

/// Bootloader environment variable names
pub mod vars {
    pub const OMNECT_VALIDATE_UPDATE: &str = "omnect_validate_update";
    pub const OMNECT_BOOTLOADER_UPDATED: &str = "omnect_bootloader_updated";
}

/// Prefix for fsck status variables in bootloader environment
pub const FSCK_VAR_PREFIX: &str = "omnect_fsck_";

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
    fn get_env(&self, key: &str) -> Result<Option<String>>;

    /// Set or delete a bootloader environment variable
    ///
    /// Pass `Some(value)` to set the variable, or `None` to delete it.
    fn set_env(&mut self, key: &str, value: Option<&str>) -> Result<()>;

    /// Save fsck result to bootloader environment.
    ///
    /// Stores exit code and full fsck output as gzip+base64 encoded string so the
    /// diagnostic text survives the reboot required after fsck corrects errors.
    fn save_fsck_status(&mut self, partition: &str, code: i32, output: &str) -> Result<()>;

    /// Get fsck status from bootloader environment.
    ///
    /// Returns the decoded `(exit_code, output)` pair if a value is present,
    /// or `None` if no status was stored for this partition.
    fn get_fsck_status(&self, partition: &str) -> Result<Option<(i32, String)>>;

    /// Clear fsck status from bootloader environment
    fn clear_fsck_status(&mut self, partition: &str) -> Result<()>;
}

/// Creates the appropriate bootloader implementation based on the build-time feature flag.
///
/// The bootloader type is a build-time property of the target platform:
/// - `grub` feature: x86-64 EFI targets using GRUB (`grub-editenv`)
/// - `uboot` feature: ARM targets using U-Boot (`fw_printenv`/`fw_setenv`)
///
/// Exactly one of `grub` or `uboot` must be enabled; build.rs enforces this.
pub fn create_bootloader(_rootfs_dir: &Path) -> Result<Box<dyn Bootloader>> {
    #[cfg(feature = "grub")]
    return Ok(Box::new(GrubBootloader::new(_rootfs_dir)?));

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
}

#[cfg(test)]
impl MockBootloader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.insert(key.to_string(), value.to_string());
        self
    }
}

#[cfg(test)]
impl Bootloader for MockBootloader {
    fn get_env(&self, key: &str) -> Result<Option<String>> {
        Ok(self.env.get(key).cloned())
    }

    fn set_env(&mut self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(v) => {
                self.env.insert(key.to_string(), v.to_string());
            }
            None => {
                self.env.remove(key);
            }
        }
        Ok(())
    }

    fn save_fsck_status(&mut self, partition: &str, code: i32, output: &str) -> Result<()> {
        use crate::bootloader::types::encode_fsck_output;
        let key = format!("{}{}", FSCK_VAR_PREFIX, partition);
        self.env.insert(key, encode_fsck_output(code, output));
        Ok(())
    }

    fn get_fsck_status(&self, partition: &str) -> Result<Option<(i32, String)>> {
        use crate::bootloader::types::decode_fsck_output;
        let key = format!("{}{}", FSCK_VAR_PREFIX, partition);
        Ok(self.env.get(&key).and_then(|v| decode_fsck_output(v)))
    }

    fn clear_fsck_status(&mut self, partition: &str) -> Result<()> {
        let key = format!("{}{}", FSCK_VAR_PREFIX, partition);
        self.env.remove(&key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_bootloader_get_set() {
        let mut bl = MockBootloader::new();

        // Test set and get
        bl.set_env("test-key", Some("test-value")).unwrap();
        assert_eq!(
            bl.get_env("test-key").unwrap(),
            Some("test-value".to_string())
        );

        // Test delete
        bl.set_env("test-key", None).unwrap();
        assert_eq!(bl.get_env("test-key").unwrap(), None);
    }

    #[test]
    fn test_mock_bootloader_with_env() {
        let bl = MockBootloader::new()
            .with_env("factory-reset", r#"{"mode":1}"#)
            .with_env("flash-mode", "1");

        assert_eq!(
            bl.get_env("factory-reset").unwrap(),
            Some(r#"{"mode":1}"#.to_string())
        );
        assert_eq!(bl.get_env("flash-mode").unwrap(), Some("1".to_string()));
        assert_eq!(bl.get_env("nonexistent").unwrap(), None);
    }

    #[test]
    fn test_mock_bootloader_fsck_status() {
        let mut bl = MockBootloader::new();

        bl.save_fsck_status("boot", 1, "errors corrected on pass 1")
            .unwrap();

        let retrieved = bl.get_fsck_status("boot").unwrap();
        assert_eq!(
            retrieved,
            Some((1, "errors corrected on pass 1".to_string()))
        );

        bl.clear_fsck_status("boot").unwrap();
        assert_eq!(bl.get_fsck_status("boot").unwrap(), None);
    }
}
