use std::path::Path;

use crate::{
    BootEnv, BootEnvState, Result, config::Config, partition::PartitionLayout, runtime::OdsStatus,
};

pub mod normal;

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
}

impl BootMode {
    /// Detect the boot mode from the boot environment.
    ///
    /// Returns `Normal` for both available and degraded (absent) boot env states.
    pub fn detect(_bl: Option<&dyn BootEnv>) -> Result<Self> {
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
}
