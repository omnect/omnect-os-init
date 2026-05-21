//! omnect-os-init library
//!
//! This library provides the core functionality for the omnect-os init process.
//! It replaces the bash-based initramfs scripts with a type-safe Rust implementation.

use std::path::Path;

use log::{info, warn};

use crate::{
    config::Config,
    filesystem::{mount_core_partitions, persist_fsck_results},
    mode::{BootContext, BootMode},
    partition::{PartitionLayout, create_omnect_symlinks, detect_root_device},
};

pub mod bootloader;
pub mod config;
pub mod early_init;
pub mod error;
pub mod filesystem;
pub mod logging;
pub mod mode;
pub mod partition;
pub mod preflight;
pub mod runtime;

// Re-export main types for convenience
#[cfg(any(test, feature = "test-utils"))]
pub use crate::bootloader::MockBootloader;
pub use crate::bootloader::{
    Bootloader, BootloaderDecision, BootloaderEnv, classify_bootloader, open_bootloader_env,
};
pub use crate::early_init::mount_essential_filesystems;
pub use crate::error::{InitramfsError, Result};
pub use crate::logging::KmsgLogger;
pub use crate::runtime::OdsStatus;

/// Mount point for the real rootfs inside the initramfs.
const ROOTFS_DIR: &str = "/rootfs";

/// Apply a bootloader decision, enforcing the FsckRequiresReboot-wins invariant.
///
/// `core_result` (e.g. `FsckRequiresReboot`) is always propagated before either
/// `DegradedBoot` or `set_degraded_boot()`, so a reboot request is never silently
/// swallowed by a concurrent bootloader failure.
fn apply_bootloader_decision(
    decision: BootloaderDecision,
    core_result: Result<()>,
    ods_status: &mut OdsStatus,
) -> Result<BootloaderEnv> {
    match decision {
        BootloaderDecision::Continue(env, _) => {
            core_result?;
            if let BootloaderEnv::Degraded(ref e) = env {
                warn!("Bootloader environment unavailable: {e}; booting in degraded mode");
                ods_status.set_degraded_boot();
            }
            Ok(env)
        }
        BootloaderDecision::Abort(err) => {
            core_result?;
            Err(err)
        }
    }
}

pub fn run_init() -> Result<()> {
    info!("omnect-os-initramfs starting");

    let config = Config::load()?;
    let rootfs = Path::new(ROOTFS_DIR);

    info!("Detecting root device...");
    let root_device = detect_root_device(&config.cmdline)?;
    info!(
        "Root device: {} (partition {})",
        root_device.base.display(),
        root_device.root_partition.display()
    );

    let layout = PartitionLayout::new(root_device)?;
    create_omnect_symlinks(&layout)?;

    let mut ods_status = OdsStatus::new();

    // Mount core partitions (rootfs + boot). Capture the result rather than
    // propagating immediately: if fsck on the boot partition requires a reboot,
    // ods_status already holds the diagnostic (fsck_and_record stores it before
    // returning the error). We open the bootloader next and persist fsck data
    // when available; apply_bootloader_decision then enforces error precedence.
    let core_result = mount_core_partitions(&layout, rootfs, &mut ods_status);

    // Best-effort: open the bootloader environment. The image type determines how
    // to proceed when it is unavailable — see classify_bootloader.
    //
    // Note: if mount_core_partitions returned FsckRequiresReboot, the boot partition
    // may not be mounted (GRUB), causing open_bootloader_env() to fail.
    // apply_bootloader_decision always propagates core_result before DegradedBoot.
    let is_release = cfg!(feature = "release-image");
    let decision = classify_bootloader(open_bootloader_env(), is_release);

    let mut bootloader_env = apply_bootloader_decision(decision, core_result, &mut ods_status)?;

    // Persist fsck results now that we have a mutable BootloaderEnv.
    // On-disk log write is skipped here (data not yet mounted) but runs again
    // in normal.rs after mount_remaining_partitions.
    persist_fsck_results(&ods_status, bootloader_env.available_mut(), rootfs);
    if bootloader_env.available_mut().is_some() {
        // Records moved to bootloader env; clear to avoid double-serialization
        // into the ODS runtime JSON. Not done in degraded mode — fsck results
        // remain in the JSON so ODS and operators can still read them.
        ods_status.fsck.clear();
    }

    {
        let ctx = preflight::PreflightCtx {
            layout: &layout,
            bootloader: &mut bootloader_env,
        };
        preflight::run(ctx)?;
    }

    let ctx = BootContext::new(&config, &layout, rootfs, bootloader_env, ods_status);

    match BootMode::detect(ctx.bootloader.available())? {
        BootMode::Normal => mode::normal::run(ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootloader::MockBootloader;
    use crate::error::{BootloaderError, FilesystemError, InitramfsError};
    use crate::filesystem::FsckExitCode;
    use std::path::PathBuf;

    fn make_available() -> BootloaderDecision {
        BootloaderDecision::Continue(
            BootloaderEnv::Available(Box::new(MockBootloader::new())),
            false,
        )
    }

    fn make_degraded() -> BootloaderDecision {
        BootloaderDecision::Continue(
            BootloaderEnv::Degraded(BootloaderError::CommandFailed {
                command: "grub-editenv".into(),
                reason: "test".into(),
            }),
            true,
        )
    }

    fn make_abort() -> BootloaderDecision {
        BootloaderDecision::Abort(InitramfsError::DegradedBoot(
            BootloaderError::CommandFailed {
                command: "grub-editenv".into(),
                reason: "test".into(),
            },
        ))
    }

    fn fsck_reboot_err() -> Result<()> {
        Err(InitramfsError::Filesystem(
            FilesystemError::FsckRequiresReboot {
                device: PathBuf::from("/dev/sda1"),
                code: FsckExitCode::REBOOT_REQUIRED,
                output: String::new(),
            },
        ))
    }

    #[test]
    fn available_ok_core_returns_available_env() {
        let mut ods = OdsStatus::new();
        let result = apply_bootloader_decision(make_available(), Ok(()), &mut ods);
        assert!(matches!(result, Ok(BootloaderEnv::Available(_))));
        assert!(!ods.degraded_boot);
    }

    #[test]
    fn degraded_ok_core_sets_degraded_flag() {
        let mut ods = OdsStatus::new();
        let result = apply_bootloader_decision(make_degraded(), Ok(()), &mut ods);
        assert!(matches!(result, Ok(BootloaderEnv::Degraded(_))));
        assert!(ods.degraded_boot);
    }

    #[test]
    fn fsck_reboot_wins_over_degraded_continue() {
        // FsckRequiresReboot must propagate even when bootloader is also unavailable.
        let mut ods = OdsStatus::new();
        let result = apply_bootloader_decision(make_degraded(), fsck_reboot_err(), &mut ods);
        assert!(
            matches!(
                result,
                Err(InitramfsError::Filesystem(
                    FilesystemError::FsckRequiresReboot { .. }
                ))
            ),
            "expected FsckRequiresReboot, not DegradedBoot"
        );
        assert!(!ods.degraded_boot, "degraded flag must not be set on reboot path");
    }

    #[test]
    fn fsck_reboot_wins_over_abort() {
        // FsckRequiresReboot must propagate even when decision is Abort(DegradedBoot).
        let mut ods = OdsStatus::new();
        let result = apply_bootloader_decision(make_abort(), fsck_reboot_err(), &mut ods);
        assert!(
            matches!(
                result,
                Err(InitramfsError::Filesystem(
                    FilesystemError::FsckRequiresReboot { .. }
                ))
            ),
            "expected FsckRequiresReboot, not DegradedBoot"
        );
    }
}
