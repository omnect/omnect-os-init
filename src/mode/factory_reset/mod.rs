pub mod backup_restore;
pub mod config;
pub mod reformat;

use std::path::{Path, PathBuf};

use log::warn;

use crate::{
    bootloader::BootEnvKey,
    error::{FactoryResetError, FilesystemError, InitramfsError, Result},
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

/// Outcome of the reformat-retry loop, returned so the caller can set the
/// success `context` note and, on exhaustion, the bootloader-env signal.
// No non-test caller until Task 5 wires this into run_destructive_phase; the
// allow is removed there.
#[allow(dead_code)]
struct RetryReport {
    /// Partitions that needed one reformat-and-retry (empty on a clean mount).
    retried: Vec<PartitionName>,
    /// Set when the retry was exhausted: the partition and reason to persist.
    exhausted: Option<(PartitionName, String)>,
}

/// Injectable seam over the destructive-phase mount side effects, so the
/// bounded-retry control flow is unit-testable without real block devices.
/// Narrowly scoped to this loop — not a general refactor of the module.
// No non-test caller until Task 5 wires this into run_destructive_phase; the
// allow is removed there.
#[allow(dead_code)]
trait ReformatRetryOps {
    fn reformat(&mut self, device: &Path, label: &str) -> Result<()>;
    fn mount_all(&mut self) -> Result<()>;
}

/// Resolve a mount/overlay failure back to the reformatted partition it
/// concerns. `MountFailed` carries the source device (match against the target
/// device paths); `OverlayFailed` carries the mount point (match against the
/// etc/data mount points). Any other error, or a path matching neither
/// reformatted partition (e.g. the read-only `factory` partition), yields None
/// so the caller propagates the error without retrying.
///
/// COUPLING: `src_path` equals `targets.data_dev`/`etc_dev` only because both
/// come from the same `layout.partitions` lookup. If a future change resolves
/// one side to a different path form (e.g. `/dev/omnect/data`), this match
/// silently stops firing — the mocked tests cannot catch that.
// No non-test caller until Task 5 wires this into run_destructive_phase; the
// allow is removed there.
#[allow(dead_code)]
fn resolve_failed_partition(
    err: &InitramfsError,
    rootfs: &Path,
    targets: &ReformatTargets,
) -> Option<PartitionName> {
    match err {
        InitramfsError::Filesystem(FilesystemError::MountFailed { src_path, .. }) => {
            if src_path == targets.data_dev {
                Some(PartitionName::Data)
            } else if src_path == targets.etc_dev {
                Some(PartitionName::Etc)
            } else {
                None
            }
        }
        InitramfsError::Filesystem(FilesystemError::OverlayFailed { target, .. }) => {
            if *target == rootfs.join(mount_points::DATA_PARTITION) {
                Some(PartitionName::Data)
            } else if *target == rootfs.join(mount_points::ETC_PARTITION) {
                Some(PartitionName::Etc)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Mount the reformatted partitions, self-healing a single bad reformat per
/// partition before giving up.
///
/// At most one reformat-and-retry per `data`/`etc`: on a mount/overlay failure
/// resolving to a reformatted partition, that partition is re-`mkfs`'d once and
/// the mount retried. A second failure on the same partition abandons the retry
/// and is returned in `RetryReport.exhausted` — NOT propagated as `Err`, so the
/// typed `(partition, reason)` survives out to `run()` (see spec §3.2/§3.5). A
/// failure resolving to neither reformatted partition propagates immediately.
// No non-test caller until Task 5 wires this into run_destructive_phase; the
// allow is removed there.
#[allow(dead_code)]
fn mount_reformatted_with_retry(
    rootfs: &Path,
    targets: &ReformatTargets,
    ops: &mut dyn ReformatRetryOps,
) -> Result<RetryReport> {
    let mut retried: Vec<PartitionName> = Vec::new();
    loop {
        match ops.mount_all() {
            Ok(()) => {
                return Ok(RetryReport {
                    retried,
                    exhausted: None,
                });
            }
            Err(e) => {
                let Some(part) = resolve_failed_partition(&e, rootfs, targets) else {
                    return Err(e);
                };
                if retried.contains(&part) {
                    return Ok(RetryReport {
                        retried,
                        exhausted: Some((part, e.to_string())),
                    });
                }
                let (device, label) = match part {
                    PartitionName::Data => (targets.data_dev, DATA_PARTITION_LABEL),
                    PartitionName::Etc => (targets.etc_dev, ETC_PARTITION_LABEL),
                    _ => return Err(e),
                };
                retried.push(part);
                ops.reformat(device, label)?;
            }
        }
    }
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

    #[cfg(feature = "factory-reset")]
    mod retry_tests {
        use super::*;
        use crate::error::FilesystemError;

        // Programmable ops: each mount_all() call pops the next scripted result.
        struct ScriptedOps {
            mount_results: std::collections::VecDeque<Result<()>>,
            reformatted: Vec<PathBuf>,
        }
        impl ScriptedOps {
            fn new(results: Vec<Result<()>>) -> Self {
                Self {
                    mount_results: results.into_iter().collect(),
                    reformatted: vec![],
                }
            }
        }
        impl ReformatRetryOps for ScriptedOps {
            fn reformat(&mut self, device: &Path, _label: &str) -> Result<()> {
                self.reformatted.push(device.to_path_buf());
                Ok(())
            }
            fn mount_all(&mut self) -> Result<()> {
                self.mount_results
                    .pop_front()
                    .expect("mount_all called more times than scripted")
            }
        }

        fn targets<'a>(
            data: &'a Path,
            etc: &'a Path,
            preserve: &'a [String],
        ) -> ReformatTargets<'a> {
            ReformatTargets {
                data_dev: data,
                etc_dev: etc,
                preserve_list: preserve,
                backup_dir: Path::new("/tmp/does-not-matter"),
            }
        }

        fn mount_failed(src: &Path) -> InitramfsError {
            FilesystemError::MountFailed {
                src_path: src.to_path_buf(),
                target: PathBuf::from("/rootfs/mnt/etc"),
                reason: "bad superblock".into(),
            }
            .into()
        }

        fn overlay_failed(target: &Path) -> InitramfsError {
            FilesystemError::OverlayFailed {
                target: target.to_path_buf(),
                reason: "cannot create upperdir".into(),
            }
            .into()
        }

        #[test]
        fn clean_mount_reports_no_retry() {
            let (data, etc) = (Path::new("/dev/sda7"), Path::new("/dev/sda6"));
            let mut ops = ScriptedOps::new(vec![Ok(())]);
            let report = mount_reformatted_with_retry(
                Path::new("/rootfs"),
                &targets(data, etc, &[]),
                &mut ops,
            )
            .unwrap();
            assert!(report.retried.is_empty());
            assert!(report.exhausted.is_none());
            assert!(ops.reformatted.is_empty());
        }

        #[test]
        fn etc_recovers_after_one_reformat() {
            let (data, etc) = (Path::new("/dev/sda7"), Path::new("/dev/sda6"));
            // First mount fails on etc, second succeeds.
            let mut ops = ScriptedOps::new(vec![Err(mount_failed(etc)), Ok(())]);
            let report = mount_reformatted_with_retry(
                Path::new("/rootfs"),
                &targets(data, etc, &[]),
                &mut ops,
            )
            .unwrap();
            assert_eq!(report.retried, vec![PartitionName::Etc]);
            assert!(report.exhausted.is_none());
            assert_eq!(ops.reformatted, vec![etc.to_path_buf()]);
        }

        #[test]
        fn data_recovers_after_one_reformat() {
            let (data, etc) = (Path::new("/dev/sda7"), Path::new("/dev/sda6"));
            // First mount fails on data, second succeeds.
            let mut ops = ScriptedOps::new(vec![Err(mount_failed(data)), Ok(())]);
            let report = mount_reformatted_with_retry(
                Path::new("/rootfs"),
                &targets(data, etc, &[]),
                &mut ops,
            )
            .unwrap();
            assert_eq!(report.retried, vec![PartitionName::Data]);
            assert!(report.exhausted.is_none());
            assert_eq!(ops.reformatted, vec![data.to_path_buf()]);
        }

        #[test]
        fn both_partitions_each_reformat_once() {
            let (data, etc) = (Path::new("/dev/sda7"), Path::new("/dev/sda6"));
            let mut ops = ScriptedOps::new(vec![
                Err(mount_failed(data)),
                Err(mount_failed(etc)),
                Ok(()),
            ]);
            let report = mount_reformatted_with_retry(
                Path::new("/rootfs"),
                &targets(data, etc, &[]),
                &mut ops,
            )
            .unwrap();
            assert!(report.exhausted.is_none());
            assert_eq!(
                report.retried,
                vec![PartitionName::Data, PartitionName::Etc]
            );
            assert_eq!(ops.reformatted, vec![data.to_path_buf(), etc.to_path_buf()]);
        }

        #[test]
        fn etc_exhausts_after_second_failure() {
            let (data, etc) = (Path::new("/dev/sda7"), Path::new("/dev/sda6"));
            let mut ops = ScriptedOps::new(vec![Err(mount_failed(etc)), Err(mount_failed(etc))]);
            let report = mount_reformatted_with_retry(
                Path::new("/rootfs"),
                &targets(data, etc, &[]),
                &mut ops,
            )
            .unwrap();
            assert_eq!(report.retried, vec![PartitionName::Etc]);
            let (part, _reason) = report.exhausted.expect("must record exhausted");
            assert_eq!(part, PartitionName::Etc);
        }

        #[test]
        fn overlay_failure_on_etc_triggers_reformat() {
            let (data, etc) = (Path::new("/dev/sda7"), Path::new("/dev/sda6"));
            let etc_mount = Path::new("/rootfs").join(mount_points::ETC_PARTITION);
            let mut ops = ScriptedOps::new(vec![Err(overlay_failed(&etc_mount)), Ok(())]);
            let report = mount_reformatted_with_retry(
                Path::new("/rootfs"),
                &targets(data, etc, &[]),
                &mut ops,
            )
            .unwrap();
            assert_eq!(report.retried, vec![PartitionName::Etc]);
            assert!(report.exhausted.is_none());
            assert_eq!(ops.reformatted, vec![etc.to_path_buf()]);
        }

        #[test]
        fn unresolvable_failure_propagates_err() {
            let (data, etc) = (Path::new("/dev/sda7"), Path::new("/dev/sda6"));
            // A failure on the factory partition device — matches neither data nor etc.
            let mut ops = ScriptedOps::new(vec![Err(mount_failed(Path::new("/dev/sda4")))]);
            let result = mount_reformatted_with_retry(
                Path::new("/rootfs"),
                &targets(data, etc, &[]),
                &mut ops,
            );
            assert!(result.is_err());
            assert!(ops.reformatted.is_empty());
        }
    }
}
