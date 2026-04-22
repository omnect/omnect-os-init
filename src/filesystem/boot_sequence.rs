//! Boot-sequence mount and fsck orchestration
//!
//! These functions coordinate partition mounting and fsck result persistence
//! during initramfs startup. Kept in the library crate so they can be unit-tested
//! with mock bootloaders and temporary directories.

use std::{fs, path::Path};

use nix::mount::MsFlags;

use crate::bootloader::Bootloader;
use crate::error::{FilesystemError, InitramfsError, PartitionError};
use crate::filesystem::{
    FsType, FsckExitCode, MountOptions, MountPoint, check_filesystem_lenient, is_path_mounted,
    mount, mount_points, mount_readwrite, mount_tmpfs,
};
use crate::partition::{PartitionLayout, PartitionName};
use crate::runtime::OdsStatus;

/// Path within the mounted data partition where fsck logs are written.
const FSCK_LOG_DIR: &str = "mnt/data/var/log/fsck";

/// Run fsck on a partition and record the result (including output) in `ods_status`.
///
/// Lenient by design: partitions that fsck reports as failed (exit ≥ 4) are
/// still recorded and the caller proceeds to mount them. A degraded boot with
/// a corrupted partition is preferable to an unrecoverable brick on an
/// embedded device without physical access. The full fsck result is persisted
/// via `OdsStatus` (→ bootloader env + `/data/var/log/fsck/<partition>.log`)
/// so ODS can act on the degraded state at runtime.
///
/// Intercepts `FsckRequiresReboot` to save the output before propagating, ensuring
/// it is available for persistence even when mounting is aborted early.
pub fn fsck_and_record(
    dev: &Path,
    name: PartitionName,
    ods_status: &mut OdsStatus,
    fstype: FsType,
) -> std::result::Result<(), FilesystemError> {
    match check_filesystem_lenient(dev, fstype) {
        Ok(r) => {
            ods_status.add_fsck_result(name, r.exit_code.bits(), r.output);
            Ok(())
        }
        Err(FilesystemError::FsckRequiresReboot {
            device,
            code,
            ref output,
        }) => {
            ods_status.add_fsck_result(name, code, output.clone());
            Err(FilesystemError::FsckRequiresReboot {
                device,
                code,
                output: output.clone(),
            })
        }
        Err(e) => Err(e),
    }
}

/// Mount all required partitions in the correct order.
pub fn mount_partitions(
    layout: &PartitionLayout,
    rootfs: &Path,
    ods_status: &mut OdsStatus,
) -> crate::error::Result<()> {
    // Mount rootfs read-only — rootCurrent is mandatory; abort if missing.
    let root_dev = layout
        .partitions
        .get(&PartitionName::RootCurrent)
        .ok_or_else(|| {
            InitramfsError::Partition(PartitionError::DeviceDetection(
                "rootCurrent not found in partition map; cannot mount rootfs".to_string(),
            ))
        })?;

    // The mount target must exist before mount(2) is called. The directory is
    // not baked into the initramfs image — create it here on every boot.
    fs::create_dir_all(rootfs).map_err(|e| {
        InitramfsError::Io(std::io::Error::other(format!(
            "Failed to create rootfs mount point {}: {}",
            rootfs.display(),
            e
        )))
    })?;

    // rootCurrent is mounted directly — no fsck. Legacy bash never runs check_fs on
    // rootCurrent either: the kernel's own ext4 journal replay is the correct recovery
    // mechanism. Running fsck -y before mount can interfere with journal replay and
    // cause EUCLEAN on a filesystem that the kernel could have mounted cleanly.
    mount(MountPoint::new(
        root_dev,
        rootfs,
        MountOptions::ext4_readonly().noatime().nodiratime(),
    ))?;
    log::info!("Mounted rootfs at {}", rootfs.display());

    // Mount boot partition.
    // vfat is mounted read-write without noatime/nodiratime: GRUB needs to write
    // grubenv on the boot partition; atime writes are acceptable on vfat.
    if let Some(boot_dev) = layout.partitions.get(&PartitionName::Boot) {
        let boot_mount = rootfs.join(mount_points::BOOT);
        if is_path_mounted(&boot_mount)? {
            // Boot already mounted at this stage is a logic error: mount_partitions
            // is called exactly once, after rootfs is freshly mounted. If boot is
            // already present something has gone wrong in the boot sequence.
            return Err(InitramfsError::Filesystem(FilesystemError::MountFailed {
                src_path: boot_dev.clone(),
                target: boot_mount,
                reason: "boot partition already mounted at start of mount_partitions".to_string(),
            }));
        }
        fsck_and_record(boot_dev, PartitionName::Boot, ods_status, FsType::Vfat)?;
        mount_readwrite(boot_dev, &boot_mount, FsType::Vfat)?;
    }

    // Mount factory partition read-only
    if let Some(factory_dev) = layout.partitions.get(&PartitionName::Factory) {
        let factory_mount = rootfs.join(mount_points::FACTORY_PARTITION);
        fsck_and_record(
            factory_dev,
            PartitionName::Factory,
            ods_status,
            FsType::Ext4,
        )?;
        mount(MountPoint::new(
            factory_dev,
            &factory_mount,
            MountOptions::ext4_readonly().noatime().nodiratime(),
        ))?;
    }

    // Mount cert partition read-write — initramfs creates ca/ and priv/ subdirs on first boot
    if let Some(cert_dev) = layout.partitions.get(&PartitionName::Cert) {
        let cert_mount = rootfs.join(mount_points::CERT_PARTITION);
        fsck_and_record(cert_dev, PartitionName::Cert, ods_status, FsType::Ext4)?;
        mount(MountPoint::new(
            cert_dev,
            &cert_mount,
            MountOptions::ext4_readwrite().noatime().nodiratime(),
        ))?;
    }

    // Mount etc partition (for overlay upper)
    if let Some(etc_dev) = layout.partitions.get(&PartitionName::Etc) {
        let etc_mount = rootfs.join(mount_points::ETC_PARTITION);
        fsck_and_record(etc_dev, PartitionName::Etc, ods_status, FsType::Ext4)?;
        mount(MountPoint::new(
            etc_dev,
            &etc_mount,
            MountOptions::ext4_readwrite().noatime().nodiratime(),
        ))?;
    }

    // Mount data partition
    if let Some(data_dev) = layout.partitions.get(&PartitionName::Data) {
        let data_mount = rootfs.join(mount_points::DATA_PARTITION);
        fsck_and_record(data_dev, PartitionName::Data, ods_status, FsType::Ext4)?;
        mount(MountPoint::new(
            data_dev,
            &data_mount,
            MountOptions::ext4_readwrite().noatime().nodiratime(),
        ))?;
    }

    // /var/volatile provides a writable mount for volatile data under the read-only rootfs
    let var_volatile = rootfs.join(mount_points::VAR_VOLATILE);
    mount_tmpfs(&var_volatile, MsFlags::empty(), None)?;

    // /run is NOT mounted here: the initramfs /run tmpfs (mounted by
    // mount_essential_filesystems) is moved into the new root by switch_root
    // using MS_MOVE. Mounting a second tmpfs at /rootfs/run would cause EBUSY
    // and lose any files written there (e.g. ODS runtime state).

    Ok(())
}

/// Persist fsck results after all partitions are mounted.
///
/// For each partition with a non-zero fsck exit code:
/// - Stores the gzip+base64 encoded exit code and full output in the bootloader
///   environment (grubenv / uboot-env) for inspection after the next boot.
/// - Writes the full output to `/data/var/log/fsck/<partition>.log` on the data
///   partition so ODS and operators can inspect it after boot.
pub fn persist_fsck_results(
    ods_status: &OdsStatus,
    bootloader: &mut dyn Bootloader,
    rootfs_dir: &Path,
) {
    // Bootloader save (grubenv or env file) is the primary persistence mechanism
    // and works as long as the boot partition is mounted — which is true even on
    // the FsckRequiresReboot path (boot is mounted before fsck runs).
    //
    // The data partition log is best-effort: it is only mounted when
    // mount_partitions() succeeds fully, so it may not be available here.
    let log_dir = rootfs_dir.join(FSCK_LOG_DIR);
    let data_mounted =
        is_path_mounted(&rootfs_dir.join(mount_points::DATA_PARTITION)).unwrap_or(false);

    for (partition, fsck) in &ods_status.fsck {
        if FsckExitCode::from(fsck.code).is_clean() {
            continue;
        }

        if let Err(e) = bootloader.save_fsck_status(*partition, fsck.code, &fsck.output) {
            log::warn!(
                "Failed to save fsck status for {} to bootloader env: {}",
                partition,
                e
            );
        }

        if !fsck.output.is_empty() {
            if !data_mounted {
                log::warn!(
                    "Data partition not mounted; skipping fsck log write for {}",
                    partition
                );
                continue;
            }
            if let Err(e) = fs::create_dir_all(&log_dir) {
                log::warn!("Failed to create fsck log dir {}: {}", log_dir.display(), e);
            } else {
                let log_path = log_dir.join(format!("{}.log", partition));
                if let Err(e) = fs::write(&log_path, &fsck.output) {
                    log::warn!("Failed to write fsck log {}: {}", log_path.display(), e);
                } else {
                    log::info!("Wrote fsck log: {}", log_path.display());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootloader::Bootloader;
    use crate::error::BootloaderError;
    use crate::partition::PartitionName;
    use crate::runtime::OdsStatus;
    use tempfile::TempDir;

    // ---- helpers -------------------------------------------------------

    fn make_ods_with(partition: PartitionName, code: i32, output: &str) -> OdsStatus {
        let mut s = OdsStatus::new();
        s.add_fsck_result(partition, code, output.to_string());
        s
    }
    struct TrackingBootloader {
        saved: Vec<(PartitionName, i32, String)>,
    }

    impl TrackingBootloader {
        fn new() -> Self {
            Self { saved: Vec::new() }
        }
    }

    impl Bootloader for TrackingBootloader {
        fn get_env(&self, _key: &str) -> crate::bootloader::Result<Option<String>> {
            Ok(None)
        }
        fn set_env(&mut self, _key: &str, _value: Option<&str>) -> crate::bootloader::Result<()> {
            Ok(())
        }
        fn save_fsck_status(
            &mut self,
            partition: PartitionName,
            code: i32,
            output: &str,
        ) -> crate::bootloader::Result<()> {
            self.saved.push((partition, code, output.to_string()));
            Ok(())
        }
        fn get_fsck_status(
            &self,
            _partition: PartitionName,
        ) -> crate::bootloader::Result<Option<(i32, String)>> {
            Ok(None)
        }
        fn clear_fsck_status(
            &mut self,
            _partition: PartitionName,
        ) -> crate::bootloader::Result<()> {
            Ok(())
        }
    }

    /// Mock that always fails on save_fsck_status.
    struct FailingBootloader;

    impl Bootloader for FailingBootloader {
        fn get_env(&self, _key: &str) -> crate::bootloader::Result<Option<String>> {
            Ok(None)
        }
        fn set_env(&mut self, _key: &str, _value: Option<&str>) -> crate::bootloader::Result<()> {
            Ok(())
        }
        fn save_fsck_status(
            &mut self,
            _partition: PartitionName,
            _code: i32,
            _output: &str,
        ) -> crate::bootloader::Result<()> {
            Err(BootloaderError::CommandFailed {
                command: "mock".into(),
                reason: "injected failure".into(),
            })
        }
        fn get_fsck_status(
            &self,
            _partition: PartitionName,
        ) -> crate::bootloader::Result<Option<(i32, String)>> {
            Ok(None)
        }
        fn clear_fsck_status(
            &mut self,
            _partition: PartitionName,
        ) -> crate::bootloader::Result<()> {
            Ok(())
        }
    }

    // ---- tests ---------------------------------------------------------

    #[test]
    fn test_persist_zero_code_not_saved() {
        // Exit code 0 (clean) must not trigger any bootloader write.
        let ods = make_ods_with(PartitionName::Boot, 0, "clean");
        let temp = TempDir::new().unwrap();
        let mut bl = TrackingBootloader::new();

        persist_fsck_results(&ods, &mut bl, temp.path());

        assert!(bl.saved.is_empty(), "zero exit code must not be persisted");
    }

    #[test]
    fn test_persist_nonzero_calls_save_fsck_status() {
        // Non-zero exit code must call save_fsck_status with correct args.
        let ods = make_ods_with(PartitionName::Boot, 1, "errors corrected");
        let temp = TempDir::new().unwrap();
        let mut bl = TrackingBootloader::new();

        persist_fsck_results(&ods, &mut bl, temp.path());

        assert_eq!(bl.saved.len(), 1);
        assert_eq!(bl.saved[0].0, PartitionName::Boot);
        assert_eq!(bl.saved[0].1, 1);
        assert_eq!(bl.saved[0].2, "errors corrected");
    }

    #[test]
    fn test_persist_empty_output_still_calls_bootloader_but_no_log_dir() {
        // Empty output: bootloader is still called (code != 0), but no log dir is created.
        let ods = make_ods_with(PartitionName::Data, 4, "");
        let temp = TempDir::new().unwrap();
        let mut bl = TrackingBootloader::new();

        persist_fsck_results(&ods, &mut bl, temp.path());

        assert_eq!(bl.saved.len(), 1);
        // No log dir should be created for empty output.
        assert!(!temp.path().join("mnt/data/var/log/fsck").exists());
    }

    #[test]
    fn test_persist_multiple_partitions_only_nonzero_saved() {
        // Mix of zero and non-zero codes — only non-zero ones reach save_fsck_status.
        let mut ods = OdsStatus::new();
        ods.add_fsck_result(PartitionName::Boot, 0, "clean".to_string());
        ods.add_fsck_result(PartitionName::Data, 1, "errors corrected".to_string());
        ods.add_fsck_result(PartitionName::Etc, 0, "clean".to_string());
        ods.add_fsck_result(PartitionName::Cert, 4, "uncorrected errors".to_string());

        let temp = TempDir::new().unwrap();
        let mut bl = TrackingBootloader::new();

        persist_fsck_results(&ods, &mut bl, temp.path());

        assert_eq!(bl.saved.len(), 2);
        let saved_partitions: std::collections::HashSet<PartitionName> =
            bl.saved.iter().map(|(p, _, _)| *p).collect();
        assert!(saved_partitions.contains(&PartitionName::Data));
        assert!(saved_partitions.contains(&PartitionName::Cert));
        assert!(!saved_partitions.contains(&PartitionName::Boot));
        assert!(!saved_partitions.contains(&PartitionName::Etc));
    }

    #[test]
    fn test_persist_bootloader_save_failure_does_not_abort() {
        // A failing bootloader must not panic or propagate — it is non-fatal.
        let ods = make_ods_with(PartitionName::Boot, 2, "reboot required");
        let temp = TempDir::new().unwrap();
        let mut bl = FailingBootloader;

        // Must not panic.
        persist_fsck_results(&ods, &mut bl, temp.path());
    }

    #[test]
    fn test_persist_data_not_mounted_no_log_dir_created() {
        // When data partition is not mounted (normal in tests), no log dir is created.
        let ods = make_ods_with(PartitionName::Boot, 1, "some output");
        let temp = TempDir::new().unwrap();
        let mut bl = TrackingBootloader::new();

        persist_fsck_results(&ods, &mut bl, temp.path());

        // Bootloader was still called.
        assert_eq!(bl.saved.len(), 1);
        // But log dir must not be created (data not mounted).
        assert!(!temp.path().join("mnt/data/var/log/fsck").exists());
    }
}
