//! Configuration module for omnect-os-init
//!
//! Provides a unified `Config` struct loaded once at startup and passed
//! explicitly through the init pipeline. Build-time constants generated from
//! Yocto environment variables are available via the `build` submodule.

use std::collections::HashMap;
use std::fs;

use crate::error::ConfigError;

/// Build-time constants generated from Yocto environment variables by build.rs.
pub mod build {
    include!(concat!(env!("OUT_DIR"), "/build_config.rs"));
}

/// Parsed kernel command line parameters.
///
/// Handles both `key=value` pairs and bare flags (e.g. `quiet`, `ro`).
/// Bare flags are stored with an empty string value so `get("quiet")` returns
/// `Some("")` when the flag is present.
#[derive(Debug, Clone, Default)]
pub struct CmdlineConfig {
    params: HashMap<String, String>,
}

impl CmdlineConfig {
    /// Load from `/proc/cmdline`.
    pub fn load() -> crate::Result<Self> {
        let raw = fs::read_to_string("/proc/cmdline").map_err(ConfigError::CmdlineReadFailed)?;
        Ok(Self::parse(&raw))
    }

    /// Parses a raw cmdline string; also usable directly in tests.
    ///
    /// Handles `key=value` pairs and bare flags (e.g. `quiet`, `ro`). Values
    /// containing spaces are not supported — the kernel splits the cmdline on
    /// whitespace, so quoted values with spaces arrive as separate tokens.
    /// Bare flags are stored with an empty string value.
    pub fn parse(cmdline: &str) -> Self {
        let mut params = HashMap::new();
        for token in cmdline.split_whitespace() {
            if let Some((key, value)) = token.split_once('=') {
                // The omnect kernel cmdline convention never uses single-quoted values.
                // This strip is purely defensive against the double-quoted root="..."
                // style that some bootloaders emit.
                params.insert(key.to_string(), value.trim_matches('"').to_string());
            } else {
                params.insert(token.to_string(), String::new());
            }
        }
        Self { params }
    }

    /// Get a parameter value by key. Returns `None` if the key is absent.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.params.get(key).map(String::as_str)
    }
}

/// Unified runtime configuration, loaded once during early init and passed
/// explicitly to all init sub-systems.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Parsed kernel command line.
    pub cmdline: CmdlineConfig,
}

impl Config {
    /// Load configuration from the running kernel environment.
    ///
    /// Reads `/proc/cmdline` and evaluates compile-time feature flags.
    pub fn load() -> crate::Result<Self> {
        let cmdline = CmdlineConfig::load()?;
        Ok(Self { cmdline })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cmdline_parse_key_value() {
        let cfg = CmdlineConfig::parse("rootpart=2 bootpart_fsuuid=1234-ABCD ro quiet");
        assert_eq!(cfg.get("rootpart"), Some("2"));
        assert_eq!(cfg.get("bootpart_fsuuid"), Some("1234-ABCD"));
    }

    #[test]
    fn test_cmdline_parse_bare_flags() {
        let cfg = CmdlineConfig::parse("rootpart=2 ro quiet");
        assert_eq!(cfg.get("ro"), Some(""));
    }

    #[test]
    fn test_cmdline_parse_missing_key() {
        let cfg = CmdlineConfig::parse("ro quiet");
        assert_eq!(cfg.get("rootpart"), None);
        assert_eq!(cfg.get("bootpart_fsuuid"), None);
    }

    #[test]
    fn test_cmdline_parse_quoted_value() {
        let cfg = CmdlineConfig::parse(r#"root="/dev/mmcblk1p2" ro"#);
        assert_eq!(cfg.get("root"), Some("/dev/mmcblk1p2"));
    }

    #[test]
    fn test_cmdline_default_is_empty() {
        let cfg = CmdlineConfig::default();
        assert_eq!(cfg.get("rootpart"), None);
    }

    #[test]
    fn test_cmdline_duplicate_key_last_wins() {
        // HashMap::insert overwrites; the last occurrence of a key wins.
        // This test pins that contract so a refactor to first-wins is caught.
        let cfg = CmdlineConfig::parse("rootpart=2 rootpart=3");
        assert_eq!(cfg.get("rootpart"), Some("3"));
    }
}
