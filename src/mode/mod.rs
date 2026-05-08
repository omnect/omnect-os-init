use std::path::Path;

use crate::{Bootloader, Result, config::Config, partition::PartitionLayout, runtime::OdsStatus};

pub mod normal;
#[cfg(feature = "resize-data")]
pub mod resize_data;

/// Runtime context passed to the active boot-mode handler.
///
/// A handler implements one boot path (normal, factory-reset, resize, …).
pub struct BootContext<'a> {
    pub(crate) config: &'a Config,
    pub(crate) layout: &'a PartitionLayout,
    pub(crate) rootfs: &'a Path,
    pub(crate) bootloader: Option<Box<dyn Bootloader>>,
    pub(crate) ods_status: OdsStatus,
}

impl<'a> BootContext<'a> {
    pub(crate) fn new(
        config: &'a Config,
        layout: &'a PartitionLayout,
        rootfs: &'a Path,
        bootloader: Option<Box<dyn Bootloader>>,
        ods_status: OdsStatus,
    ) -> Self {
        Self {
            config,
            layout,
            rootfs,
            bootloader,
            ods_status,
        }
    }
}

/// The detected boot mode to execute.
pub enum BootMode {
    Normal,
}

impl BootMode {
    /// Detect the boot mode from bootloader environment variables.
    ///
    /// Returns `Normal` when the bootloader is absent (degraded boot).
    /// `_bl` becomes active once a non-Normal variant is added.
    pub fn detect(_bl: Option<&dyn Bootloader>) -> Result<Self> {
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
