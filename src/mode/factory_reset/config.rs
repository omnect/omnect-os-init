use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::error::{FactoryResetError, Result};

pub(crate) const FACTORY_RESET_CONFIG_FILE: &str = "etc/omnect/factory-reset.json";
const FACTORY_RESET_CONFIG_DIR: &str = "etc/omnect/factory-reset.d";
const PRESERVE_LIST_MANDATORY: &str = "/etc/omnect/factory-reset.d/";
const KEY_APPLICATIONS: &str = "applications";
const KEY_PATHS: &str = "paths";

#[derive(Debug, Deserialize)]
pub struct FactoryResetConfig {
    pub mode: u32,
    #[serde(default)]
    pub preserve: Vec<String>,
}

impl FactoryResetConfig {
    pub fn parse(json: &str) -> Result<Self> {
        serde_json::from_str(json).map_err(|e| {
            FactoryResetError::InvalidConfig(format!("Failed to parse factory-reset JSON: {e}"))
                .into()
        })
    }
}

pub fn build_preserve_list(config: &FactoryResetConfig, rootfs: &Path) -> Result<Vec<String>> {
    let mut list = vec![PRESERVE_LIST_MANDATORY.to_string()];

    let has_non_app_keys = config.preserve.iter().any(|k| k != KEY_APPLICATIONS);

    let config_file = rootfs.join(FACTORY_RESET_CONFIG_FILE);
    let key_config: Option<Value> = if has_non_app_keys {
        let content = std::fs::read_to_string(&config_file).map_err(|e| {
            FactoryResetError::InvalidConfig(format!(
                "Failed to read {}: {e}",
                config_file.display()
            ))
        })?;
        let value: Value = serde_json::from_str(&content).map_err(|e| {
            FactoryResetError::InvalidConfig(format!(
                "Failed to parse {}: {e}",
                config_file.display()
            ))
        })?;
        Some(value)
    } else {
        None
    };

    for key in &config.preserve {
        if key == KEY_APPLICATIONS {
            collect_application_paths(rootfs, &mut list)?;
        } else {
            let value = key_config.as_ref().unwrap_or_else(|| {
                unreachable!("key_config must be Some for non-application keys")
            });
            let paths = value.get(key.as_str()).ok_or_else(|| {
                FactoryResetError::MissingField(format!(
                    "{}: no '{key}' key",
                    config_file.display()
                ))
            })?;
            let arr = paths.as_array().ok_or_else(|| {
                FactoryResetError::InvalidConfig(format!(
                    "{}: value for key '{key}' must be an array",
                    config_file.display()
                ))
            })?;
            for p in arr {
                if let Some(s) = p.as_str() {
                    list.push(s.to_string());
                }
            }
        }
    }

    Ok(list)
}

fn collect_application_paths(rootfs: &Path, list: &mut Vec<String>) -> Result<()> {
    let dir = rootfs.join(FACTORY_RESET_CONFIG_DIR);

    if !dir.exists() {
        return Ok(());
    }

    let entries = std::fs::read_dir(&dir).map_err(|e| {
        FactoryResetError::InvalidConfig(format!("Failed to read {}: {e}", dir.display()))
    })?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let content = std::fs::read_to_string(&path).map_err(|e| {
            FactoryResetError::InvalidConfig(format!("Failed to read {}: {e}", path.display()))
        })?;

        let value: Value = serde_json::from_str(&content).map_err(|e| {
            FactoryResetError::InvalidConfig(format!("{}: invalid JSON ({e})", path.display()))
        })?;

        if let Some(arr) = value.get(KEY_PATHS).and_then(|v| v.as_array()) {
            for p in arr {
                if let Some(s) = p.as_str() {
                    list.push(s.to_string());
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn parse_mode_and_preserve() {
        let cfg = FactoryResetConfig::parse(r#"{"mode":1,"preserve":[]}"#).unwrap();
        assert_eq!(cfg.mode, 1);
        assert!(cfg.preserve.is_empty());
    }

    #[test]
    fn parse_with_preserve_keys() {
        let cfg = FactoryResetConfig::parse(r#"{"mode":2,"preserve":["applications","network"]}"#)
            .unwrap();
        assert_eq!(cfg.mode, 2);
        assert_eq!(cfg.preserve, vec!["applications", "network"]);
    }

    #[test]
    fn parse_invalid_json_returns_error() {
        assert!(FactoryResetConfig::parse("not json").is_err());
    }

    #[test]
    fn parse_missing_mode_returns_error() {
        assert!(FactoryResetConfig::parse(r#"{"preserve":[]}"#).is_err());
    }

    #[test]
    fn build_preserve_list_empty_preserve() {
        let temp = TempDir::new().unwrap();
        let cfg = FactoryResetConfig {
            mode: 1,
            preserve: vec![],
        };
        let list = build_preserve_list(&cfg, temp.path()).unwrap();
        assert_eq!(list, vec![PRESERVE_LIST_MANDATORY]);
    }

    #[test]
    fn build_preserve_list_applications_key() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("etc/omnect/factory-reset.d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("app.json"),
            r#"{"paths":["/home/user/.config","var/app"]}"#,
        )
        .unwrap();

        let cfg = FactoryResetConfig {
            mode: 1,
            preserve: vec!["applications".into()],
        };
        let list = build_preserve_list(&cfg, temp.path()).unwrap();
        assert_eq!(list[0], PRESERVE_LIST_MANDATORY);
        assert!(list.contains(&"/home/user/.config".to_string()));
        assert!(list.contains(&"var/app".to_string()));
    }

    #[test]
    fn build_preserve_list_custom_key() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("etc/omnect");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("factory-reset.json"),
            r#"{"network":["/etc/network/interfaces","/etc/wpa_supplicant.conf"]}"#,
        )
        .unwrap();

        let cfg = FactoryResetConfig {
            mode: 1,
            preserve: vec!["network".into()],
        };
        let list = build_preserve_list(&cfg, temp.path()).unwrap();
        assert!(list.contains(&"/etc/network/interfaces".to_string()));
        assert!(list.contains(&"/etc/wpa_supplicant.conf".to_string()));
    }

    #[test]
    fn build_preserve_list_custom_key_non_array_returns_error() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("etc/omnect");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("factory-reset.json"),
            r#"{"network":"/etc/network/interfaces"}"#,
        )
        .unwrap();

        let cfg = FactoryResetConfig {
            mode: 1,
            preserve: vec!["network".into()],
        };
        assert!(build_preserve_list(&cfg, temp.path()).is_err());
    }

    #[test]
    fn build_preserve_list_applications_invalid_json_returns_invalid_config() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("etc/omnect/factory-reset.d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("app.json"), "not json").unwrap();

        let cfg = FactoryResetConfig {
            mode: 1,
            preserve: vec!["applications".into()],
        };
        let error = build_preserve_list(&cfg, temp.path()).unwrap_err();
        assert!(matches!(
            error,
            crate::error::InitramfsError::FactoryReset(FactoryResetError::InvalidConfig(_))
        ));
    }
}
