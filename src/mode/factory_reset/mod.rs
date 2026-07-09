pub mod backup_restore;
pub mod config;
pub mod reformat;

use std::path::{Path, PathBuf};

use log::warn;

use crate::{
    bootloader::BootEnvKey,
    error::{FactoryResetError, InitramfsError, Result},
    filesystem::{
        FsType, MountOptions, PartitionMountSpec, mount_points, mount_tracked_partition,
        setup_data_overlay_tracked, setup_etc_overlay_tracked, unmount_tracked,
    },
    mode::{BootContext, factory_reset::backup_restore::RestoreResult},
    partition::{PartitionLayout, PartitionName},
    runtime::{FactoryResetStatus, FactoryResetStatusCode, OdsStatus},
};

use crate::mode::factory_reset::{
    backup_restore::{backup_all, restore_all},
    config::{FactoryResetConfig, build_preserve_list},
    reformat::reformat_ext4,
};

const FACTORY_RESET_BACKUP_DIR: &str = "/tmp/factory_reset/backup";
const SUPPORTED_RESET_MODE: u32 = 1;

/// ext4 volume labels applied by `reformat_ext4` — distinct from the
/// `mount_points` path constants, which name mount *locations* not labels.
const DATA_PARTITION_LABEL: &str = "data";
const ETC_PARTITION_LABEL: &str = "etc";

/// Entry point for factory-reset mode.
///
/// Clears the trigger env var, runs the reset sequence, writes status to
/// `ods_status`, and always delegates to Normal boot — never blocks the device.
pub fn run(mut ctx: BootContext<'_>, config: FactoryResetConfig) -> Result<()> {
    // Spec §5: clear failure is non-fatal — log and continue with the reset.
    // If set_env consistently fails the trigger persists and the reset will
    // repeat on every boot until set_env succeeds. This is the accepted
    // trade-off per the error-handling table in the design spec.
    if let Some(bl) = ctx.boot_env.available_mut()
        && let Err(e) = bl.set_env(BootEnvKey::FactoryReset, None)
    {
        warn!("Failed to clear factory-reset bootloader var: {e}; proceeding anyway");
    }

    let status = match run_reset(ctx.layout, ctx.rootfs, &config, &mut ctx.ods_status) {
        Ok(status) => status,
        Err(e) => {
            warn!("Factory reset failed: {e}; continuing with Normal boot");
            let code = match &e {
                InitramfsError::FactoryReset(FactoryResetError::InvalidConfig(_)) => {
                    FactoryResetStatusCode::Invalid
                }
                InitramfsError::FactoryReset(FactoryResetError::MissingField(_)) => {
                    FactoryResetStatusCode::ConfigError
                }
                // Note: RestoreFailed cannot arrive here — restore_all() accumulates
                // per-path failures as RestoreResult::PartialFailure and always returns Ok.
                _ => FactoryResetStatusCode::Error,
            };
            FactoryResetStatus {
                status: code,
                error: Some(e.to_string()),
                context: None,
                paths: vec![],
                data_wiped: false,
                exhausted_signal: None,
            }
        }
    };
    ctx.ods_status.set_factory_reset(status);

    crate::mode::normal::run(ctx)
}

/// Inner reset sequence. Returns `Err` only for failures before the
/// destructive phase begins (mount, config, backup); failures at or after
/// the first `reformat_ext4` call are resolved to a status internally
/// instead, so they're never mistaken for a safe pre-reformat abort.
fn run_reset(
    layout: &PartitionLayout,
    rootfs: &Path,
    config: &FactoryResetConfig,
    ods_status: &mut OdsStatus,
) -> Result<FactoryResetStatus> {
    if config.mode != SUPPORTED_RESET_MODE {
        let msg = format!("factory reset mode {} is not supported", config.mode);
        warn!("{msg}");
        return Err(FactoryResetError::InvalidConfig(msg).into());
    }

    let mut mounts: Vec<PathBuf> = Vec::new();
    factory_reset_mount(layout, rootfs, ods_status, &mut mounts).inspect_err(|_| {
        let _ = unmount_tracked(&mut mounts);
    })?;

    let preserve_list = build_preserve_list(config, rootfs).inspect_err(|_| {
        let _ = unmount_tracked(&mut mounts);
    })?;

    let backup_dir = PathBuf::from(FACTORY_RESET_BACKUP_DIR);
    backup_all(rootfs, &preserve_list, &backup_dir).inspect_err(|_| {
        let _ = unmount_tracked(&mut mounts);
    })?;

    unmount_tracked(&mut mounts)?;

    let data_dev = layout.partitions.get(&PartitionName::Data).ok_or_else(|| {
        FactoryResetError::MountError("data partition not found in layout".to_string())
    })?;
    let etc_dev = layout.partitions.get(&PartitionName::Etc).ok_or_else(|| {
        FactoryResetError::MountError("etc partition not found in layout".to_string())
    })?;

    match run_destructive_phase(
        layout,
        rootfs,
        &mut mounts,
        ods_status,
        ReformatTargets {
            data_dev,
            etc_dev,
            preserve_list: &preserve_list,
            backup_dir: &backup_dir,
        },
    ) {
        Ok(status) => Ok(status),
        Err(e) => Ok(destructive_phase_failure_status(e, preserve_list)),
    }
}

/// Devices and paths needed by `run_destructive_phase`, grouped to keep the
/// function's argument count manageable.
struct ReformatTargets<'a> {
    data_dev: &'a Path,
    etc_dev: &'a Path,
    preserve_list: &'a [String],
    backup_dir: &'a Path,
}

/// Reformat + restore — everything from here on is destructive: `data` and/or
/// `etc` may already be wiped and the tmpfs backup discarded. Callers must
/// treat any `Err` from this function as data-loss, not a safe no-op abort.
fn run_destructive_phase(
    layout: &PartitionLayout,
    rootfs: &Path,
    mounts: &mut Vec<PathBuf>,
    ods_status: &mut OdsStatus,
    targets: ReformatTargets,
) -> Result<FactoryResetStatus> {
    reformat_ext4(targets.data_dev, DATA_PARTITION_LABEL)?;
    reformat_ext4(targets.etc_dev, ETC_PARTITION_LABEL)?;

    // Re-mount including factory so setup_etc_overlay reseeds etc's now-empty
    // upper dir with factory defaults before restore_all overlays preserved
    // paths on top — the seed check only fires while the upper dir is empty.
    factory_reset_mount(layout, rootfs, ods_status, mounts).inspect_err(|_| {
        let _ = unmount_tracked(mounts);
    })?;

    let restore_result = restore_all(rootfs, targets.preserve_list, targets.backup_dir)
        .inspect_err(|_| {
            let _ = unmount_tracked(mounts);
        })?;

    unmount_tracked(mounts)?;

    log::info!("factory-reset complete");

    let status = match restore_result {
        RestoreResult::Success => FactoryResetStatus {
            status: FactoryResetStatusCode::Success,
            error: None,
            context: None,
            paths: targets.preserve_list.to_vec(),
            data_wiped: true,
            exhausted_signal: None,
        },
        RestoreResult::PartialFailure { context, error } => FactoryResetStatus {
            status: FactoryResetStatusCode::Error,
            error: Some(error),
            context: Some(context),
            paths: targets.preserve_list.to_vec(),
            data_wiped: true,
            exhausted_signal: None,
        },
    };

    Ok(status)
}

/// Build the status for a failure that occurred during or after the
/// destructive phase — `data_wiped: true` and the preserve list are always
/// populated so ODS/cloud can tell this apart from a safe pre-reformat abort
/// and see what was lost.
fn destructive_phase_failure_status(e: InitramfsError, paths: Vec<String>) -> FactoryResetStatus {
    warn!(
        "factory reset failed after the destructive phase began; preserved data may be permanently lost: {e}"
    );
    FactoryResetStatus {
        status: FactoryResetStatusCode::Error,
        error: Some(e.to_string()),
        context: None,
        paths,
        data_wiped: true,
        exhausted_signal: None,
    }
}

/// Mount factory (ro, if present), etc (rw), data (rw) and set up overlays.
///
/// Tracks each mount in `mounts` so `unmount_tracked` can reverse them.
/// Used for both the pre-backup and post-reformat mounts — factory must be
/// present both times so `setup_etc_overlay_tracked` can always reseed an
/// empty etc upper dir from factory defaults.
fn factory_reset_mount(
    layout: &PartitionLayout,
    rootfs: &Path,
    ods_status: &mut OdsStatus,
    mounts: &mut Vec<PathBuf>,
) -> Result<()> {
    mount_tracked_partition(
        layout,
        PartitionMountSpec {
            partition: PartitionName::Factory,
            mount_point: mount_points::FACTORY_PARTITION,
            options: MountOptions::ext4_readonly(),
            fstype: FsType::Ext4,
        },
        rootfs,
        ods_status,
        mounts,
    )?;

    mount_tracked_partition(
        layout,
        PartitionMountSpec {
            partition: PartitionName::Etc,
            mount_point: mount_points::ETC_PARTITION,
            options: MountOptions::ext4_readwrite(),
            fstype: FsType::Ext4,
        },
        rootfs,
        ods_status,
        mounts,
    )?;

    mount_tracked_partition(
        layout,
        PartitionMountSpec {
            partition: PartitionName::Data,
            mount_point: mount_points::DATA_PARTITION,
            options: MountOptions::ext4_readwrite(),
            fstype: FsType::Ext4,
        },
        rootfs,
        ods_status,
        mounts,
    )?;

    setup_etc_overlay_tracked(rootfs, mounts)?;
    setup_data_overlay_tracked(rootfs, mounts)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::partition::RootDevice;
    use std::collections::HashMap;

    fn empty_layout() -> PartitionLayout {
        PartitionLayout {
            partitions: HashMap::new(),
            device: RootDevice {
                base: PathBuf::from("/dev/sda"),
                partition_sep: "",
                root_partition: PathBuf::from("/dev/sda2"),
            },
        }
    }

    #[test]
    fn run_reset_rejects_unsupported_mode_before_touching_layout() {
        // Pure gate checked before any mount is attempted — an empty layout
        // (no real partitions) proves this never reaches factory_reset_mount.
        let layout = empty_layout();
        let config = FactoryResetConfig {
            mode: 2,
            preserve: vec![],
        };
        let mut ods_status = OdsStatus::new();
        let err =
            run_reset(&layout, Path::new("/nonexistent"), &config, &mut ods_status).unwrap_err();
        assert!(matches!(
            err,
            InitramfsError::FactoryReset(FactoryResetError::InvalidConfig(_))
        ));
    }
}
