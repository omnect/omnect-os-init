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
pub use crate::bootloader::MockBootEnv;
pub use crate::bootloader::{
    BootEnv, BootEnvDecision, BootEnvState, classify_boot_env, open_boot_env,
};
pub use crate::early_init::mount_essential_filesystems;
pub use crate::error::{InitramfsError, Result};
pub use crate::logging::KmsgLogger;
pub use crate::runtime::OdsStatus;

/// Mount point for the real rootfs inside the initramfs.
const ROOTFS_DIR: &str = "/rootfs";

/// Apply a bootloader decision, enforcing the FsckRequiresReboot-wins invariant.
///
/// Persists fsck results to the bootloader environment *before* propagating
/// `core_result`, satisfying the contract documented in `mount_core_partitions`:
/// the diagnostic must be written before the error propagates, or it is lost
/// across the reboot. This is a no-op for degraded boot (`env.available_mut()`
/// returns `None`) and when `ods_status.fsck` is empty.
///
/// After a successful persist the fsck records are cleared from `ods_status` to
/// prevent double-serialization into the ODS runtime JSON. In degraded mode the
/// records are intentionally kept so ODS consumers can still read them.
fn apply_boot_env_decision(
    decision: BootEnvDecision,
    core_result: Result<()>,
    ods_status: &mut OdsStatus,
    rootfs: &Path,
) -> Result<BootEnvState> {
    match decision {
        BootEnvDecision::Continue(mut env) => {
            // Persist before propagating core_result (uboot is always Available,
            // so without this the diagnostic would be lost on FsckRequiresReboot).
            persist_fsck_results(ods_status, env.available_mut(), rootfs);
            if env.available_mut().is_some() {
                // Records moved to bootloader env; clear to avoid double-serialization
                // into the ODS runtime JSON. Not done in degraded mode — fsck results
                // remain in the JSON so ODS and operators can still read them.
                ods_status.fsck.clear();
            }
            core_result?;
            if let BootEnvState::Degraded(ref e) = env {
                warn!("Boot environment unavailable: {e}; booting in degraded mode");
                ods_status.set_degraded_boot();
            }
            Ok(env)
        }
        BootEnvDecision::Abort(err) => {
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
    // ods_status already holds the diagnostic. apply_boot_env_decision persists
    // it to the bootloader env before propagating (satisfying the contract in
    // mount_core_partitions), then enforces FsckRequiresReboot-wins precedence.
    let core_result = mount_core_partitions(&layout, rootfs, &mut ods_status);

    // Best-effort: open the bootloader environment. The image type determines how
    // to proceed when it is unavailable — see classify_boot_env.
    //
    // Note: if mount_core_partitions returned FsckRequiresReboot, the boot partition
    // may not be mounted (GRUB), causing open_boot_env() to fail.
    // apply_boot_env_decision always propagates core_result before DegradedBoot.
    let is_release = cfg!(feature = "release-image");
    let decision = classify_boot_env(open_boot_env(), is_release);

    let mut bootloader_env =
        apply_boot_env_decision(decision, core_result, &mut ods_status, rootfs)?;

    {
        let ctx = preflight::PreflightCtx {
            layout: &layout,
            boot_env: &mut bootloader_env,
        };
        preflight::run(ctx)?;
    }

    let ctx = BootContext::new(&config, &layout, rootfs, bootloader_env, ods_status);

    match BootMode::detect(ctx.boot_env.available())? {
        BootMode::Normal => mode::normal::run(ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootloader::MockBootEnv;
    use crate::error::{BootEnvError, FilesystemError, InitramfsError};
    use crate::filesystem::FsckExitCode;
    use std::path::PathBuf;

    fn make_available() -> BootEnvDecision {
        BootEnvDecision::Continue(BootEnvState::Available(Box::new(MockBootEnv::new())))
    }

    fn make_degraded() -> BootEnvDecision {
        BootEnvDecision::Continue(BootEnvState::Degraded(BootEnvError::CommandFailed {
            command: "grub-editenv".into(),
            reason: "test".into(),
        }))
    }

    fn make_abort() -> BootEnvDecision {
        BootEnvDecision::Abort(InitramfsError::DegradedBoot(BootEnvError::CommandFailed {
            command: "grub-editenv".into(),
            reason: "test".into(),
        }))
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
        let result =
            apply_boot_env_decision(make_available(), Ok(()), &mut ods, Path::new("/tmp"));
        assert!(matches!(result, Ok(BootEnvState::Available(_))));
        assert!(!ods.degraded_boot);
    }

    #[test]
    fn degraded_ok_core_sets_degraded_flag() {
        let mut ods = OdsStatus::new();
        let result =
            apply_boot_env_decision(make_degraded(), Ok(()), &mut ods, Path::new("/tmp"));
        assert!(matches!(result, Ok(BootEnvState::Degraded(_))));
        assert!(ods.degraded_boot);
    }

    #[test]
    fn fsck_reboot_wins_over_degraded_continue() {
        // FsckRequiresReboot must propagate even when bootloader is also unavailable.
        let mut ods = OdsStatus::new();
        let result = apply_boot_env_decision(
            make_degraded(),
            fsck_reboot_err(),
            &mut ods,
            Path::new("/tmp"),
        );
        assert!(
            matches!(
                result,
                Err(InitramfsError::Filesystem(
                    FilesystemError::FsckRequiresReboot { .. }
                ))
            ),
            "expected FsckRequiresReboot, not DegradedBoot"
        );
        assert!(
            !ods.degraded_boot,
            "degraded flag must not be set on reboot path"
        );
    }

    #[test]
    fn fsck_reboot_wins_over_abort() {
        // FsckRequiresReboot must propagate even when decision is Abort(DegradedBoot).
        let mut ods = OdsStatus::new();
        let result =
            apply_boot_env_decision(make_abort(), fsck_reboot_err(), &mut ods, Path::new("/tmp"));
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

    #[test]
    fn persist_runs_before_fsck_reboot_propagates() {
        // Regression test for uboot: on uboot open_boot_env() is infallible,
        // so env is Available. The fsck diagnostic in ods_status.fsck must be
        // persisted to the bootloader env *before* FsckRequiresReboot propagates,
        // or it is lost across the reboot (boot_sequence.rs:68-76 contract).
        let mut ods = OdsStatus::new();
        ods.add_fsck_result(
            crate::partition::PartitionName::Boot,
            1,
            "errors corrected on pass 1".into(),
        );

        let decision =
            BootEnvDecision::Continue(BootEnvState::Available(Box::new(MockBootEnv::new())));
        let result =
            apply_boot_env_decision(decision, fsck_reboot_err(), &mut ods, Path::new("/tmp"));

        assert!(
            matches!(
                result,
                Err(InitramfsError::Filesystem(
                    FilesystemError::FsckRequiresReboot { .. }
                ))
            ),
            "FsckRequiresReboot must still propagate"
        );
        // fsck.clear() runs after persist; ods.fsck must be empty — records were
        // moved to the bootloader env before the error propagated.
        assert!(
            ods.fsck.is_empty(),
            "persist_fsck_results must run before propagating FsckRequiresReboot \
             (boot_sequence.rs:68-76 contract)"
        );
    }
}
