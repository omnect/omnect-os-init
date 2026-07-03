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
                // BackupFailed, ReformatFailed, MountError, Io → operational error.
                // Note: RestoreFailed cannot arrive here — restore_all() accumulates
                // per-path failures as RestoreResult::PartialFailure and always returns Ok.
                _ => FactoryResetStatusCode::Error,
            };
            FactoryResetStatus {
                status: code,
                error: Some(e.to_string()),
                context: None,
                paths: vec![],
            }
        }
    };
    ctx.ods_status.set_factory_reset(status);

    crate::mode::normal::run(ctx)
}

/// Inner reset sequence — returns the final status on success/partial-restore,
/// or `Err` on hard failures (mount, backup, reformat).
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
    factory_reset_mount(layout, rootfs, &mut mounts, IncludeFactory::Yes).inspect_err(|_| {
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

    reformat_ext4(data_dev, "data")?;
    reformat_ext4(etc_dev, "etc")?;

    // Re-mount for restore: etc(rw) + data(rw) + overlays only. Factory is
    // deliberately excluded (design spec §3.4 step 8) — its defaults were
    // already applied to the etc upper layer during the first mount, and
    // mode::normal::run mounts factory again afterward for the next boot.
    factory_reset_mount(layout, rootfs, &mut mounts, IncludeFactory::No).inspect_err(|_| {
        let _ = factory_reset_umount(&mut mounts);
    })?;

    let restore_result = restore_all(rootfs, &preserve_list, &backup_dir).inspect_err(|_| {
        let _ = factory_reset_umount(&mut mounts);
    })?;

    factory_reset_umount(&mut mounts)?;

    log::info!("factory-reset complete");

    let status = match restore_result {
        RestoreResult::Success => FactoryResetStatus {
            status: FactoryResetStatusCode::Success,
            error: None,
            context: None,
            paths: preserve_list,
        },
        RestoreResult::PartialFailure { context, error } => FactoryResetStatus {
            status: FactoryResetStatusCode::Error,
            error: Some(error),
            context: Some(context),
            paths: preserve_list,
        },
    };

    Ok(status)
}

/// Whether `factory_reset_mount` should also mount the (optional) factory
/// partition. `No` is used for the restore-phase remount — see the call site
/// in `run_reset`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IncludeFactory {
    Yes,
    No,
}

/// Mount factory (ro, if present and requested), etc (rw), data (rw) and set
/// up overlays.
///
/// Tracks each mount in `mounts` so `factory_reset_umount` can reverse them.
fn factory_reset_mount(
    layout: &PartitionLayout,
    rootfs: &Path,
    mounts: &mut Vec<PathBuf>,
    include_factory: IncludeFactory,
) -> Result<()> {
    if include_factory == IncludeFactory::Yes {
        mount_partition(
            layout,
            PartitionName::Factory,
            mount_points::FACTORY_PARTITION,
            MountOptions::ext4_readonly(),
            "factory",
            rootfs,
            mounts,
        )?;
    }

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
/// A no-op when the partition is absent from the layout — matches the
/// existing behavior of the per-partition blocks this replaces.
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

    #[test]
    fn factory_reset_umount_succeeds_for_empty_mount_list() {
        let mut mounts: Vec<PathBuf> = Vec::new();
        assert!(factory_reset_umount(&mut mounts).is_ok());
        assert!(mounts.is_empty());
    }
}
