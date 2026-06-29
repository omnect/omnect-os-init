pub mod backup_restore;
pub mod config;
pub mod reformat;

use std::path::PathBuf;

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

/// Entry point for factory-reset mode.
///
/// Clears the trigger env var, runs the reset sequence, writes status to
/// `ods_status`, and always delegates to Normal boot — never blocks the device.
pub fn run(mut ctx: BootContext<'_>, config: FactoryResetConfig) -> Result<()> {
    if let Some(bl) = ctx.boot_env.available_mut() {
        if let Err(e) = bl.set_env(BootEnvKey::FactoryReset, None) {
            warn!("Failed to clear factory-reset bootloader var: {e}; proceeding anyway");
        }
    }

    let status = match run_reset(ctx.layout, ctx.rootfs, &config) {
        Ok(status) => status,
        Err(e) => {
            warn!("Factory reset failed: {e}; continuing with Normal boot");
            let code = match &e {
                InitramfsError::FactoryReset(FactoryResetError::InvalidConfig(_)) => {
                    FactoryResetStatusCode::Invalid
                }
                InitramfsError::FactoryReset(FactoryResetError::BackupFailed { .. })
                | InitramfsError::FactoryReset(FactoryResetError::RestoreFailed { .. }) => {
                    FactoryResetStatusCode::Error
                }
                _ => FactoryResetStatusCode::ConfigError,
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
    rootfs: &std::path::Path,
    config: &FactoryResetConfig,
) -> Result<FactoryResetStatus> {
    if config.mode != 1 {
        let msg = format!("factory reset mode {} is not supported", config.mode);
        warn!("{msg}");
        return Err(FactoryResetError::InvalidConfig(msg).into());
    }

    let mut mounts: Vec<PathBuf> = Vec::new();
    factory_reset_mount(layout, rootfs, &mut mounts)?;

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

    factory_reset_mount(layout, rootfs, &mut mounts)?;

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

/// Mount factory (ro, if present), etc (rw), data (rw) and set up overlays.
///
/// Tracks each mount in `mounts` so `factory_reset_umount` can reverse them.
fn factory_reset_mount(
    layout: &PartitionLayout,
    rootfs: &std::path::Path,
    mounts: &mut Vec<PathBuf>,
) -> Result<()> {
    if let Some(factory_dev) = layout.partitions.get(&PartitionName::Factory) {
        let factory_mount = rootfs.join(mount_points::FACTORY_PARTITION);
        std::fs::create_dir_all(&factory_mount)?;
        mount(MountPoint::new(
            factory_dev,
            &factory_mount,
            MountOptions::ext4_readonly(),
        ))
        .map_err(|e| FactoryResetError::MountError(format!("factory: {e}")))?;
        mounts.push(factory_mount);
    }

    if let Some(etc_dev) = layout.partitions.get(&PartitionName::Etc) {
        let etc_mount = rootfs.join(mount_points::ETC_PARTITION);
        std::fs::create_dir_all(&etc_mount)?;
        mount(MountPoint::new(
            etc_dev,
            &etc_mount,
            MountOptions::ext4_readwrite(),
        ))
        .map_err(|e| FactoryResetError::MountError(format!("etc: {e}")))?;
        mounts.push(etc_mount);
    }

    if let Some(data_dev) = layout.partitions.get(&PartitionName::Data) {
        let data_mount = rootfs.join(mount_points::DATA_PARTITION);
        std::fs::create_dir_all(&data_mount)?;
        mount(MountPoint::new(
            data_dev,
            &data_mount,
            MountOptions::ext4_readwrite(),
        ))
        .map_err(|e| FactoryResetError::MountError(format!("data: {e}")))?;
        mounts.push(data_mount);
    }

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

/// Unmount all factory-reset mounts in reverse order.
///
/// Continues on individual failures and returns the last error, if any, so
/// the caller always attempts to unmount everything regardless of partial failures.
pub(crate) fn factory_reset_umount(mounts: &mut Vec<PathBuf>) -> Result<()> {
    let mut last_err: Option<InitramfsError> = None;
    for path in mounts.drain(..).rev() {
        if is_path_mounted(&path).unwrap_or(false) {
            if let Err(e) = umount(&path) {
                warn!("Failed to unmount {}: {e}", path.display());
                last_err = Some(e.into());
            }
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
