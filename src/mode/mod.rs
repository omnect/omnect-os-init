use std::path::Path;

use crate::{Bootloader, Result, config::Config, partition::PartitionLayout, runtime::OdsStatus};

/// The root filesystem mount point inside the initramfs.
///
/// Defined here (not in `main.rs`) so `run_init()` and all mode handlers
/// share a single source of truth.
pub const ROOTFS_DIR: &str = "/rootfs";

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
    pub config: &'a Config,
    pub layout: &'a PartitionLayout,
    pub rootfs: &'a Path,
    pub bootloader: Option<Box<dyn Bootloader>>,
    pub ods_status: OdsStatus,
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
    use crate::bootloader::MockBootloader;

    #[test]
    fn detect_normal_with_live_bootloader() {
        let mock = MockBootloader::new();
        let mode = BootMode::detect(Some(&mock)).unwrap();
        assert!(matches!(mode, BootMode::Normal));
    }

    #[test]
    fn detect_normal_degraded_boot_no_bootloader() {
        let mode = BootMode::detect(None).unwrap();
        assert!(matches!(mode, BootMode::Normal));
    }
}
