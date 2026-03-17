//! Bootloader abstraction module
//!
//! This module provides a trait-based abstraction over different bootloaders
//! (GRUB and U-Boot) to allow unified access to bootloader environment variables.

mod grub;
mod types;
mod uboot;

use std::path::Path;

use crate::error::BootloaderError;

pub use self::grub::GrubBootloader;
pub use self::types::BootloaderType;
pub use self::uboot::UBootBootloader;

pub type Result<T> = std::result::Result<T, BootloaderError>;

/// Bootloader environment variable names
pub mod vars {
    pub const FACTORY_RESET: &str = "factory-reset";
    pub const FLASH_MODE: &str = "flash-mode";
    pub const FLASH_MODE_DEVPATH: &str = "flash-mode-devpath";
    pub const FLASH_MODE_URL: &str = "flash-mode-url";
    pub const RESIZED_DATA: &str = "resized-data";
    pub const OMNECT_VALIDATE_UPDATE: &str = "omnect_validate_update";
    pub const DATA_MOUNT_OPTIONS: &str = "data-mount-options";
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

    /// Save fsck exit code to bootloader environment.
    ///
    /// Only the integer exit code is stored (single digit) to keep the
    /// environment block small. Full fsck output is written to
    /// `/data/var/log/fsck/<partition>.log` by the caller.
    fn save_fsck_status(&mut self, partition: &str, code: i32) -> Result<()>;

    /// Get fsck status from bootloader environment.
    ///
    /// Returns the raw integer exit code string if it exists (e.g. `"0"`, `"1"`).
    fn get_fsck_status(&self, partition: &str) -> Result<Option<String>>;

    /// Clear fsck status from bootloader environment
    fn clear_fsck_status(&mut self, partition: &str) -> Result<()>;

    /// Get the bootloader type
    fn bootloader_type(&self) -> BootloaderType;
}

/// Creates the appropriate bootloader implementation based on available tools.
///
/// Detection logic:
/// - If `grub-editenv` is present in the initramfs (`/usr/bin/grub-editenv`), use GRUB.
///   Must be called after the boot partition is mounted (grubenv lives there).
/// - Otherwise, use U-Boot (assumes fw_printenv/fw_setenv available in initramfs).
pub fn create_bootloader(rootfs_dir: &Path) -> Result<Box<dyn Bootloader>> {
    // grub-editenv is an initramfs tool, not installed in the rootfs.
    const GRUB_EDITENV_INITRAMFS_PATH: &str = "/usr/bin/grub-editenv";

    if std::path::Path::new(GRUB_EDITENV_INITRAMFS_PATH).exists() {
        Ok(Box::new(GrubBootloader::new(rootfs_dir)?))
    } else {
        Ok(Box::new(UBootBootloader::new()?))
    }
}

/// Create a mock bootloader for testing
#[cfg(test)]
pub fn create_mock_bootloader() -> MockBootloader {
    MockBootloader::new()
}

/// Mock bootloader for testing
#[cfg(test)]
pub struct MockBootloader {
    env: std::collections::HashMap<String, String>,
}

#[cfg(test)]
impl MockBootloader {
    pub fn new() -> Self {
        Self {
            env: std::collections::HashMap::new(),
        }
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

    fn save_fsck_status(&mut self, partition: &str, code: i32) -> Result<()> {
        let key = format!("omnect_fsck_{}", partition);
        self.env.insert(key, code.to_string());
        Ok(())
    }

    fn get_fsck_status(&self, partition: &str) -> Result<Option<String>> {
        let key = format!("omnect_fsck_{}", partition);
        Ok(self.env.get(&key).cloned())
    }

    fn clear_fsck_status(&mut self, partition: &str) -> Result<()> {
        let key = format!("omnect_fsck_{}", partition);
        self.env.remove(&key);
        Ok(())
    }

    fn bootloader_type(&self) -> BootloaderType {
        BootloaderType::Mock
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

        bl.save_fsck_status("boot", 1).unwrap();

        let retrieved = bl.get_fsck_status("boot").unwrap();
        assert_eq!(retrieved, Some("1".to_string()));

        bl.clear_fsck_status("boot").unwrap();
        assert_eq!(bl.get_fsck_status("boot").unwrap(), None);
    }

    #[test]
    fn test_bootloader_type() {
        let bl = MockBootloader::new();
        assert_eq!(bl.bootloader_type(), BootloaderType::Mock);
    }
}
