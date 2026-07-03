pub mod backup_restore;
pub mod config;
pub mod reformat;

use std::path::{Path, PathBuf};

use log::warn;

use crate::{
    bootloader::BootEnvKey,
    error::{FactoryResetError, InitramfsError, Result},
    filesystem::{
        MountOptions, MountPoint, is_path_mounted, mount, mount_points, setup_data_overlay,
        setup_etc_overlay, umount,
    },
    mode::{BootContext, factory_reset::backup_restore::RestoreResult},
    partition::{PartitionLayout, PartitionName},
    runtime::{FactoryResetStatus, FactoryResetStatusCode},
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

    let status = match run_reset(ctx.layout, ctx.rootfs, &config) {
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
            }
        }
    };
    ctx.ods_status.set_factory_reset(status);

    crate::mode::normal::run(ctx)
}

/// Inner reset sequence — returns the final status on success/partial-restore,
/// or `Err` on hard failures that occur before the destructive phase begins
/// (mount, config, backup) — nothing has been touched yet, so the caller's
/// generic error-to-status mapping is accurate for these.
///
/// Failures at or after the first `reformat_ext4` call are resolved to a
/// status internally (see `run_destructive_phase`) rather than propagated,
/// so they are never confused with a safe pre-reformat abort.
fn run_reset(
    layout: &PartitionLayout,
    rootfs: &Path,
    config: &FactoryResetConfig,
) -> Result<FactoryResetStatus> {
    if config.mode != SUPPORTED_RESET_MODE {
        let msg = format!("factory reset mode {} is not supported", config.mode);
        warn!("{msg}");
        return Err(FactoryResetError::InvalidConfig(msg).into());
    }

    let mut mounts: Vec<PathBuf> = Vec::new();
    factory_reset_mount(layout, rootfs, &mut mounts).inspect_err(|_| {
        let _ = factory_reset_umount(&mut mounts);
    })?;

    let preserve_list = build_preserve_list(config, rootfs).inspect_err(|_| {
        let _ = factory_reset_umount(&mut mounts);
    })?;

    let backup_dir = PathBuf::from(FACTORY_RESET_BACKUP_DIR);
    backup_all(rootfs, &preserve_list, &backup_dir).inspect_err(|_| {
        let _ = factory_reset_umount(&mut mounts);
    })?;

    factory_reset_umount(&mut mounts)?;

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
        data_dev,
        etc_dev,
        &preserve_list,
        &backup_dir,
    ) {
        Ok(status) => Ok(status),
        Err(e) => Ok(destructive_phase_failure_status(e, preserve_list)),
    }
}

/// Reformat + restore — everything from here on is destructive: `data` and/or
/// `etc` may already be wiped and the tmpfs backup discarded. Callers must
/// treat any `Err` from this function as data-loss, not a safe no-op abort.
fn run_destructive_phase(
    layout: &PartitionLayout,
    rootfs: &Path,
    mounts: &mut Vec<PathBuf>,
    data_dev: &Path,
    etc_dev: &Path,
    preserve_list: &[String],
    backup_dir: &Path,
) -> Result<FactoryResetStatus> {
    reformat_ext4(data_dev, DATA_PARTITION_LABEL)?;
    reformat_ext4(etc_dev, ETC_PARTITION_LABEL)?;

    // Re-mount for restore, including factory: the reformat above wiped etc's
    // overlay upper dir, so setup_etc_overlay (inside factory_reset_mount) finds
    // it empty and needs factory mounted to reseed it with factory /etc defaults
    // before restore_all overlays the preserved paths on top. Skipping factory
    // here leaves the upper dir permanently unseeded once any preserved path
    // populates it, since the empty-upper-dir seed check never fires again.
    factory_reset_mount(layout, rootfs, mounts).inspect_err(|_| {
        let _ = factory_reset_umount(mounts);
    })?;

    let restore_result = restore_all(rootfs, preserve_list, backup_dir).inspect_err(|_| {
        let _ = factory_reset_umount(mounts);
    })?;

    factory_reset_umount(mounts)?;

    log::info!("factory-reset complete");

    let status = match restore_result {
        RestoreResult::Success => FactoryResetStatus {
            status: FactoryResetStatusCode::Success,
            error: None,
            context: None,
            paths: preserve_list.to_vec(),
            data_wiped: true,
        },
        RestoreResult::PartialFailure { context, error } => FactoryResetStatus {
            status: FactoryResetStatusCode::Error,
            error: Some(error),
            context: Some(context),
            paths: preserve_list.to_vec(),
            data_wiped: true,
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
    }
}

/// Mount factory (ro, if present), etc (rw), data (rw) and set up overlays.
///
/// Tracks each mount in `mounts` so `factory_reset_umount` can reverse them.
/// Used for both the pre-backup and post-reformat mounts — factory must be
/// present both times so `setup_etc_overlay` can always reseed an empty etc
/// upper dir from factory defaults.
fn factory_reset_mount(
    layout: &PartitionLayout,
    rootfs: &Path,
    mounts: &mut Vec<PathBuf>,
) -> Result<()> {
    mount_partition(
        layout,
        PartitionName::Factory,
        mount_points::FACTORY_PARTITION,
        MountOptions::ext4_readonly(),
        "factory",
        rootfs,
        mounts,
    )?;

    mount_partition(
        layout,
        PartitionName::Etc,
        mount_points::ETC_PARTITION,
        MountOptions::ext4_readwrite(),
        "etc",
        rootfs,
        mounts,
    )?;

    mount_partition(
        layout,
        PartitionName::Data,
        mount_points::DATA_PARTITION,
        MountOptions::ext4_readwrite(),
        "data",
        rootfs,
        mounts,
    )?;

    setup_etc_overlay(rootfs)
        .map_err(|e| FactoryResetError::MountError(format!("etc overlay: {e}")))?;
    mounts.push(rootfs.join("etc"));

    setup_data_overlay(rootfs)
        .map_err(|e| FactoryResetError::MountError(format!("data overlay: {e}")))?;
    mounts.push(rootfs.join("home"));
    mounts.push(rootfs.join("var/lib"));
    mounts.push(rootfs.join("usr/local"));
    #[cfg(feature = "persistent-var-log")]
    mounts.push(rootfs.join("var/log"));

    Ok(())
}

/// Mount `partition` at `rootfs/mount_point`, if present in `layout`, and
/// track it in `mounts` for later cleanup via `factory_reset_umount`.
///
/// A no-op when the partition is absent from the layout.
fn mount_partition(
    layout: &PartitionLayout,
    partition: PartitionName,
    mount_point: &str,
    options: MountOptions,
    label: &str,
    rootfs: &Path,
    mounts: &mut Vec<PathBuf>,
) -> Result<()> {
    let Some(dev) = layout.partitions.get(&partition) else {
        return Ok(());
    };
    let mount_path = rootfs.join(mount_point);
    std::fs::create_dir_all(&mount_path)?;
    mount(MountPoint::new(dev, &mount_path, options))
        .map_err(|e| FactoryResetError::MountError(format!("{label}: {e}")))?;
    mounts.push(mount_path);
    Ok(())
}

/// Unmount all factory-reset mounts in reverse order.
///
/// Continues on individual failures and returns the last error, if any, so
/// the caller always attempts to unmount everything regardless of partial failures.
pub(crate) fn factory_reset_umount(mounts: &mut Vec<PathBuf>) -> Result<()> {
    let mut last_err: Option<InitramfsError> = None;
    for path in mounts.drain(..).rev() {
        // is_path_mounted re-reads /proc/mounts; treat a read failure as
        // "assume still mounted" rather than "not mounted" — a spurious skip
        // here would leave a partition mounted under the imminent reformat.
        let mounted = is_path_mounted(&path).unwrap_or_else(|e| {
            warn!(
                "Failed to check mount status for {}: {e}; attempting unmount anyway",
                path.display()
            );
            true
        });
        if mounted && let Err(e) = umount(&path) {
            warn!("Failed to unmount {}: {e}", path.display());
            last_err = Some(e.into());
        }
    }
    if let Some(e) = last_err {
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::partition::RootDevice;
    use std::collections::HashMap;

    #[test]
    fn factory_reset_umount_succeeds_for_empty_mount_list() {
        let mut mounts: Vec<PathBuf> = Vec::new();
        assert!(factory_reset_umount(&mut mounts).is_ok());
        assert!(mounts.is_empty());
    }

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
        // (no real partitions) proves this never reaches mount_partition.
        let layout = empty_layout();
        let config = FactoryResetConfig {
            mode: 2,
            preserve: vec![],
        };
        let err = run_reset(&layout, Path::new("/nonexistent"), &config).unwrap_err();
        assert!(matches!(
            err,
            InitramfsError::FactoryReset(FactoryResetError::InvalidConfig(_))
        ));
    }

    #[test]
    fn mount_partition_is_noop_when_absent_from_layout() {
        let layout = empty_layout();
        let mut mounts: Vec<PathBuf> = Vec::new();
        let result = mount_partition(
            &layout,
            PartitionName::Factory,
            mount_points::FACTORY_PARTITION,
            MountOptions::ext4_readonly(),
            "factory",
            Path::new("/nonexistent"),
            &mut mounts,
        );
        assert!(result.is_ok());
        assert!(mounts.is_empty());
    }
}
