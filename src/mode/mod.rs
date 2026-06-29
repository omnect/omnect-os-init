use std::path::Path;

use crate::{
    BootEnv, BootEnvState, Result, config::Config, partition::PartitionLayout, runtime::OdsStatus,
};

#[cfg(feature = "factory-reset")]
use crate::bootloader::BootEnvKey;

pub mod normal;

#[cfg(feature = "factory-reset")]
pub mod factory_reset;

/// Runtime context passed to the active boot-mode handler.
pub struct BootContext<'a> {
    pub(crate) config: &'a Config,
    pub(crate) layout: &'a PartitionLayout,
    pub(crate) rootfs: &'a Path,
    pub(crate) boot_env: BootEnvState,
    pub(crate) ods_status: OdsStatus,
}

impl<'a> BootContext<'a> {
    pub(crate) fn new(
        config: &'a Config,
        layout: &'a PartitionLayout,
        rootfs: &'a Path,
        boot_env: BootEnvState,
        ods_status: OdsStatus,
    ) -> Self {
        Self {
            config,
            layout,
            rootfs,
            boot_env,
            ods_status,
        }
    }
}

/// The detected boot mode to execute.
pub enum BootMode {
    Normal,
    #[cfg(feature = "factory-reset")]
    FactoryReset(factory_reset::config::FactoryResetConfig),
}

impl BootMode {
    /// Detect the boot mode from the boot environment.
    ///
    /// When the `factory-reset` bootloader env key is set and contains valid
    /// JSON, returns `FactoryReset`. Falls back to `Normal` on any env-read
    /// or JSON parse error — never blocks boot.
    pub fn detect(_bl: Option<&dyn BootEnv>) -> Result<Self> {
        #[cfg(feature = "factory-reset")]
        if let Some(bl) = _bl {
            match bl.get_env(BootEnvKey::FactoryReset) {
                Ok(Some(json)) => {
                    match factory_reset::config::FactoryResetConfig::parse(&json) {
                        Ok(config) => return Ok(Self::FactoryReset(config)),
                        Err(e) => {
                            log::warn!(
                                "factory-reset: invalid config JSON, booting normally: {e}"
                            );
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    log::warn!("factory-reset: failed to read env, booting normally: {e}");
                }
            }
        }

        Ok(Self::Normal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootloader::create_mock_bootloader;

    #[test]
    fn detect_normal_with_live_bootloader() {
        let mock = create_mock_bootloader();
        let mode = BootMode::detect(Some(&mock)).unwrap();
        assert!(matches!(mode, BootMode::Normal));
    }

    #[test]
    fn detect_normal_degraded_boot_no_bootloader() {
        let mode = BootMode::detect(None).unwrap();
        assert!(matches!(mode, BootMode::Normal));
    }

    #[cfg(feature = "factory-reset")]
    mod factory_reset_detect_tests {
        use super::*;
        use crate::bootloader::BootEnvKey;

        #[test]
        fn detect_normal_when_factory_reset_key_absent() {
            let mock = create_mock_bootloader();
            let mode = BootMode::detect(Some(&mock)).unwrap();
            assert!(matches!(mode, BootMode::Normal));
        }

        #[test]
        fn detect_factory_reset_when_key_present_valid_json() {
            let mock = create_mock_bootloader()
                .with_env(BootEnvKey::FactoryReset, r#"{"mode":1,"preserve":[]}"#);
            let mode = BootMode::detect(Some(&mock)).unwrap();
            assert!(matches!(mode, BootMode::FactoryReset(_)));
            if let BootMode::FactoryReset(config) = mode {
                assert_eq!(config.mode, 1);
                assert!(config.preserve.is_empty());
            }
        }

        #[test]
        fn detect_normal_when_key_present_invalid_json() {
            let mock =
                create_mock_bootloader().with_env(BootEnvKey::FactoryReset, "not-json");
            let mode = BootMode::detect(Some(&mock)).unwrap();
            assert!(matches!(mode, BootMode::Normal));
        }

        #[test]
        fn detect_normal_when_bootloader_unavailable() {
            let mode = BootMode::detect(None).unwrap();
            assert!(matches!(mode, BootMode::Normal));
        }

        #[test]
        fn detect_normal_when_get_env_fails() {
            let mock = create_mock_bootloader().with_get_env_error();
            let mode = BootMode::detect(Some(&mock)).unwrap();
            assert!(matches!(mode, BootMode::Normal));
        }
    }
}
