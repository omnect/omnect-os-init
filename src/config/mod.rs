//! Configuration module for omnect-os-init
//!
//! Loads only what cannot be determined at build time:
//! - `rootfs_dir`: path to the rootfs mount point (default `/rootfs`)
//! - `cmdline_params`: kernel command line parameters (e.g. `quiet`)
//!
//! Build-time constants generated from Yocto environment variables are
//! available via the `build` submodule.

/// Build-time constants generated from Yocto environment variables by build.rs.
pub mod build {
    include!(concat!(env!("OUT_DIR"), "/build_config.rs"));
}

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::error::{ConfigError, Result};

/// Runtime configuration for the initramfs
#[derive(Debug, Clone)]
pub struct Config {
    /// Path to the rootfs mount point
    pub rootfs_dir: PathBuf,

    /// Kernel command line parameters
    pub cmdline_params: HashMap<String, String>,
}

impl Config {
    /// Load configuration from all sources
    pub fn load() -> Result<Self> {
        let cmdline_params = Self::parse_cmdline()?;

        let rootfs_dir = std::env::var("ROOTFS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/rootfs"));

        Ok(Self {
            rootfs_dir,
            cmdline_params,
        })
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

    /// Check if kernel quiet mode is enabled
    pub fn is_quiet(&self) -> bool {
        self.cmdline_params.contains_key("quiet")
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            rootfs_dir: PathBuf::from("/rootfs"),
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
        assert!(config.cmdline_params.is_empty());
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
