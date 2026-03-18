//! Configuration module for omnect-os-init
//!
//! This module handles loading configuration from various sources:
//! - Kernel command line (/proc/cmdline)
//! - Environment variables
//! - /etc/os-release
//!
//! Build-time constants generated from Yocto environment variables are
//! available via the `build` submodule.

/// Build-time constants generated from Yocto environment variables by build.rs.
pub mod build {
    include!(concat!(env!("OUT_DIR"), "/build_config.rs"));
}

use crate::error::{ConfigError, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Runtime configuration for the initramfs
#[derive(Debug, Clone)]
pub struct Config {
    /// Path to the rootfs mount point
    pub rootfs_dir: PathBuf,

    /// Whether this is a release image
    pub is_release_image: bool,

    /// Machine features from os-release
    pub machine_features: Vec<String>,

    /// Distro features from os-release
    pub distro_features: Vec<String>,

    /// Kernel command line parameters
    pub cmdline_params: HashMap<String, String>,
}

impl Config {
    /// Load configuration from all sources
    pub fn load() -> Result<Self> {
        let cmdline_params = Self::parse_cmdline()?;

        // Get rootfs_dir from environment or use default
        let rootfs_dir = std::env::var("ROOTFS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/rootfs"));

        // os-release is not read here: rootfs is not mounted yet at this point.
        // Call load_os_release() after mount_partitions() succeeds.

        Ok(Self {
            rootfs_dir,
            is_release_image: false,
            machine_features: vec![],
            distro_features: vec![],
            cmdline_params,
        })
    }

    /// Load os-release fields from the mounted rootfs.
    ///
    /// Must be called after `mount_partitions` so that `rootfs_dir/etc/os-release`
    /// exists and reflects the real OS image.
    pub fn load_os_release(&mut self) -> Result<()> {
        let (is_release, machine_features, distro_features) =
            Self::parse_os_release(&self.rootfs_dir)?;
        self.is_release_image = is_release;
        self.machine_features = machine_features;
        self.distro_features = distro_features;
        Ok(())
    }

    /// Parse kernel command line parameters
    fn parse_cmdline() -> Result<HashMap<String, String>> {
        let cmdline = fs::read_to_string("/proc/cmdline").map_err(|e| ConfigError::ReadFailed {
            path: "/proc/cmdline".to_string(),
            reason: e.to_string(),
        })?;
        let mut params: HashMap<String, String> = HashMap::new();

        for part in cmdline.split_whitespace() {
            if let Some((key, value)) = part.split_once('=') {
                params.insert(key.to_string(), value.to_string());
            } else {
                // Boolean parameter (just the key)
                params.insert(part.to_string(), String::new());
            }
        }

        Ok(params)
    }

    /// Parse os-release file for configuration
    fn parse_os_release(rootfs_dir: &Path) -> Result<(bool, Vec<String>, Vec<String>)> {
        let os_release_path = rootfs_dir.join("etc/os-release");
        let content = match fs::read_to_string(&os_release_path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(ConfigError::ReadFailed {
                    path: os_release_path.display().to_string(),
                    reason: e.to_string(),
                }
                .into());
            }
        };

        let mut is_release = false;
        let mut machine_features = vec![];
        let mut distro_features = vec![];

        for line in content.lines() {
            if let Some((key, value)) = line.split_once('=') {
                let value = value.trim_matches('"');

                match key {
                    "OMNECT_RELEASE_IMAGE" => {
                        is_release = value == "1";
                    }
                    "MACHINE_FEATURES" => {
                        machine_features =
                            value.split_whitespace().map(|s| s.to_string()).collect();
                    }
                    "DISTRO_FEATURES" => {
                        distro_features = value.split_whitespace().map(|s| s.to_string()).collect();
                    }
                    _ => {}
                }
            }
        }

        Ok((is_release, machine_features, distro_features))
    }

    /// Check if a distro feature is enabled
    pub fn has_distro_feature(&self, feature: &str) -> bool {
        self.distro_features.iter().any(|f| f == feature)
    }

    /// Check if a machine feature is enabled
    pub fn has_machine_feature(&self, feature: &str) -> bool {
        self.machine_features.iter().any(|f| f == feature)
    }

    /// Check if flash-mode-2 is enabled
    pub fn has_flash_mode_2(&self) -> bool {
        self.has_distro_feature("flash-mode-2")
    }

    /// Check if flash-mode-3 is enabled
    pub fn has_flash_mode_3(&self) -> bool {
        self.has_distro_feature("flash-mode-3")
    }

    /// Check if resize-data is enabled
    pub fn has_resize_data(&self) -> bool {
        self.has_distro_feature("resize-data")
    }

    /// Check if persistent-var-log is enabled
    pub fn has_persistent_var_log(&self) -> bool {
        self.has_distro_feature("persistent-var-log")
    }

    /// Check if EFI is supported
    pub fn has_efi(&self) -> bool {
        self.has_machine_feature("efi")
    }

    /// Check if kernel quiet mode is enabled
    pub fn is_quiet(&self) -> bool {
        self.cmdline_params.contains_key("quiet")
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            rootfs_dir: PathBuf::from("/rootfs"),
            is_release_image: false,
            machine_features: vec![],
            distro_features: vec![],
            cmdline_params: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.rootfs_dir, PathBuf::from("/rootfs"));
        assert!(!config.is_release_image);
        assert!(config.machine_features.is_empty());
    }

    #[test]
    fn test_has_distro_feature() {
        let mut config = Config::default();
        config.distro_features = vec!["flash-mode-2".to_string(), "resize-data".to_string()];

        assert!(config.has_distro_feature("flash-mode-2"));
        assert!(config.has_distro_feature("resize-data"));
        assert!(!config.has_distro_feature("flash-mode-3"));
    }

    #[test]
    fn test_has_machine_feature() {
        let mut config = Config::default();
        config.machine_features = vec!["efi".to_string(), "tpm2".to_string()];

        assert!(config.has_efi());
        assert!(config.has_machine_feature("tpm2"));
        assert!(!config.has_machine_feature("nonexistent"));
    }

    #[test]
    fn test_convenience_methods() {
        let mut config = Config::default();
        config.distro_features = vec![
            "flash-mode-2".to_string(),
            "resize-data".to_string(),
            "persistent-var-log".to_string(),
        ];

        assert!(config.has_flash_mode_2());
        assert!(!config.has_flash_mode_3());
        assert!(config.has_resize_data());
        assert!(config.has_persistent_var_log());
    }

    #[test]
    fn test_quiet_mode() {
        let mut config = Config::default();
        assert!(!config.is_quiet());

        config
            .cmdline_params
            .insert("quiet".to_string(), String::new());
        assert!(config.is_quiet());
    }
}
