use std::path::Path;

use crate::{Bootloader, Result, config::Config, partition::PartitionLayout, runtime::OdsStatus};

pub mod normal;

/// Context passed to every mode handler.
///
/// Mode handlers are invoked with **all partitions mounted**: rootfs read-only
/// at `/rootfs`, boot at `/rootfs/boot`, factory/data/cert/etc at their
/// standard mount points. `persist_fsck_results` has already run. Handlers
/// own the lifecycle of any overlay or bind mounts and must not assume
/// additional preflight will occur. Future modes (factory-reset, flash-mode)
/// that need to unmount partitions before acting do so internally.
pub struct BootContext<'a> {
    pub(crate) config: &'a Config,
    /// Reserved for FactoryReset/Resize handlers — unused in normal boot.
    #[allow(dead_code)]
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
///
/// Only `Normal` ships in this PR. Future variants (`FactoryReset`, `Resize`,
/// `FlashMode`) are added in their respective implementation PRs alongside
/// their detection logic, typed payloads, and `BootloaderEnvKey` additions.
pub enum BootMode {
    Normal,
    // FactoryReset(FactoryResetConfig) — added in the factory-reset PR
    // Resize                           — added in the resize PR
    // FlashMode(FlashKind)             — added in the flash-mode PR
}

impl BootMode {
    /// Detect the boot mode from bootloader environment variables.
    ///
    /// Accepts `Option<&dyn Bootloader>`. Returns `Normal` when the bootloader
    /// is absent (degraded boot: no env vars readable → no special mode).
    ///
    /// The `_bl` parameter is intentionally unused until the first additional
    /// mode variant lands. Rename to `bl` and add detection logic in the
    /// respective implementation PR.
    pub fn detect(_bl: Option<&dyn Bootloader>) -> Result<Self> {
        Ok(Self::Normal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootloader::create_mock_bootloader;

    // TODO: replace with a full env-var × variant matrix when the first non-Normal
    // BootMode variant lands. Each future variant must add tests covering:
    //   - env-var present + live bootloader  → correct variant returned
    //   - env-var present + no bootloader    → degraded-boot fallback (Normal)
    //   - env-var absent                     → Normal
    // Until then these two tests only verify the degraded-boot path is reachable.

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
