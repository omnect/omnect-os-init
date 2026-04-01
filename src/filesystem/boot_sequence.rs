//! Boot-sequence mount and fsck orchestration
//!
//! These functions coordinate partition mounting and fsck result persistence
//! during initramfs startup. Kept in the library crate so they can be unit-tested
//! with mock bootloaders and temporary directories.

use std::{fs, path::Path};

use nix::mount::MsFlags;

use crate::bootloader::Bootloader;
use crate::config::Config;
use crate::error::{FilesystemError, InitramfsError, PartitionError};
use crate::filesystem::{
    MountManager, MountOptions, MountPoint, check_filesystem_lenient, is_path_mounted,
};
use crate::partition::{PartitionLayout, partition_names};
use crate::runtime::OdsStatus;

/// Run fsck on a partition and record the result (including output) in `ods_status`.
///
/// Intercepts `FsckRequiresReboot` to save the output before propagating, ensuring
/// it is available for persistence even when mounting is aborted early.
pub fn fsck_and_record(
    dev: &Path,
    name: &str,
    ods_status: &mut OdsStatus,
    fstype: &str,
) -> std::result::Result<(), FilesystemError> {
    match check_filesystem_lenient(dev, fstype) {
        Ok(r) => {
            ods_status.add_fsck_result(name, r.exit_code, r.output);
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
    mm: &mut MountManager,
    layout: &PartitionLayout,
    config: &Config,
    ods_status: &mut OdsStatus,
) -> crate::error::Result<()> {
    let rootfs = &config.rootfs_dir;

    // Mount rootfs read-only — rootCurrent is mandatory; abort if missing.
    let root_dev = layout
        .partitions
        .get(partition_names::ROOT_CURRENT)
        .ok_or_else(|| {
            InitramfsError::Partition(PartitionError::DeviceDetection(
                "rootCurrent not found in partition map; cannot mount rootfs".to_string(),
            ))
        })?;
    // rootCurrent is mounted directly — no fsck. Legacy bash never runs check_fs on
    // rootCurrent either: the kernel's own ext4 journal replay is the correct recovery
    // mechanism. Running fsck -y before mount can interfere with journal replay and
    // cause EUCLEAN on a filesystem that the kernel could have mounted cleanly.
    mm.mount(MountPoint::new(
        root_dev,
        rootfs,
        MountOptions::ext4_readonly().noatime().nodiratime(),
    ))?;
    log::info!("Mounted rootfs at {}", rootfs.display());

    // Mount boot partition — legacy uses bare mount with no explicit options
    if let Some(boot_dev) = layout.partitions.get(partition_names::BOOT) {
        let boot_mount = rootfs.join("boot");
        fsck_and_record(boot_dev, partition_names::BOOT, ods_status, "vfat")?;
        mm.mount_readwrite(boot_dev, &boot_mount, "vfat")?;
    }

    // Mount factory partition read-only
    if let Some(factory_dev) = layout.partitions.get(partition_names::FACTORY) {
        let factory_mount = rootfs.join("mnt/factory");
        fsck_and_record(factory_dev, partition_names::FACTORY, ods_status, "ext4")?;
        mm.mount(MountPoint::new(
            factory_dev,
            &factory_mount,
            MountOptions::ext4_readonly().noatime().nodiratime(),
        ))?;
    }

    // Mount cert partition read-write — initramfs creates ca/ and priv/ subdirs on first boot
    if let Some(cert_dev) = layout.partitions.get(partition_names::CERT) {
        let cert_mount = rootfs.join("mnt/cert");
        fsck_and_record(cert_dev, partition_names::CERT, ods_status, "ext4")?;
        mm.mount(MountPoint::new(
            cert_dev,
            &cert_mount,
            MountOptions::ext4_readwrite().noatime().nodiratime(),
        ))?;
    }

    // Mount etc partition (for overlay upper)
    if let Some(etc_dev) = layout.partitions.get(partition_names::ETC) {
        let etc_mount = rootfs.join("mnt/etc");
        fsck_and_record(etc_dev, partition_names::ETC, ods_status, "ext4")?;
        mm.mount(MountPoint::new(
            etc_dev,
            &etc_mount,
            MountOptions::ext4_readwrite().noatime().nodiratime(),
        ))?;
    }

    // Mount data partition
    if let Some(data_dev) = layout.partitions.get(partition_names::DATA) {
        let data_mount = rootfs.join("mnt/data");
        fsck_and_record(data_dev, partition_names::DATA, ods_status, "ext4")?;
        mm.mount(MountPoint::new(
            data_dev,
            &data_mount,
            MountOptions::ext4_readwrite().noatime().nodiratime(),
        ))?;
    }

    // /var/volatile provides a writable mount for volatile data under the read-only rootfs
    let var_volatile = rootfs.join("var/volatile");
    mm.mount_tmpfs(&var_volatile, MsFlags::empty(), None)?;

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
    let log_dir = rootfs_dir.join("mnt/data/var/log/fsck");
    let data_mounted = is_path_mounted(&rootfs_dir.join("mnt/data")).unwrap_or(false);

    for (partition, fsck) in &ods_status.fsck {
        if fsck.code == 0 {
            continue;
        }

        if let Err(e) = bootloader.save_fsck_status(partition, fsck.code, &fsck.output) {
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
