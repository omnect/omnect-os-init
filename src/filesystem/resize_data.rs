//! Data partition auto-resize
//!
//! Expands the data partition and its ext4 filesystem to fill available disk
//! space on first boot. Called from `mode::first_boot` on every first boot
//! (the caller guarantees the resize guard is absent before invoking this).

use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use crate::bootloader::{Bootloader, BootloaderEnvKey};
use crate::error::{ResizeDataError, Result};
use crate::filesystem::{FsType, check_filesystem};
use crate::partition::PartitionName;

#[cfg(feature = "gpt")]
const SGDISK_CMD: &str = "/sbin/sgdisk";
const PARTED_CMD: &str = "/sbin/parted";
const RESIZE2FS_CMD: &str = "/sbin/resize2fs";
const SYNC_CMD: &str = "/bin/sync";

#[cfg(feature = "gpt")]
const SGDISK_MOVE_BACKUP_HEADER: &str = "-e";
const PARTED_RESIZEPART: &str = "resizepart";
const PARTED_RESIZE_FULL: &str = "100%";
#[cfg(feature = "dos")]
const PARTED_PRINT: &str = "print";
const RESIZE2FS_FORCE: &str = "-f";

const MTAB_PATH: &str = "/etc/mtab";
const PROC_MOUNTS_PATH: &str = "/proc/self/mounts";

type ResizeResult<T> = std::result::Result<T, ResizeDataError>;

pub fn resize_if_needed(
    layout: &crate::partition::PartitionLayout,
    bootloader: &mut Box<dyn Bootloader>,
) -> Result<()> {
    let data_dev = match layout.partitions.get(&PartitionName::Data) {
        Some(d) => d.clone(),
        None => {
            log::warn!("Data partition not found in layout; skipping resize");
            return Ok(());
        }
    };
    let rootblk = &layout.device.base;

    let part_nr = partition_number(&data_dev)
        .ok_or_else(|| ResizeDataError::InvalidDevicePath(data_dev.clone()))?;

    log::info!(
        "Resizing data partition: {} partition {}",
        rootblk.display(),
        part_nr
    );

    let rootblk_str = rootblk
        .to_str()
        .ok_or_else(|| ResizeDataError::NonUtf8Path(rootblk.to_path_buf()))?;

    #[cfg(feature = "gpt")]
    {
        // Move backup GPT header to end of disk before resizing — required when
        // the disk was grown after the image was written (e.g. flash → larger medium).
        run_cmd(SGDISK_CMD, &[rootblk_str, SGDISK_MOVE_BACKUP_HEADER])?;
    }

    #[cfg(feature = "dos")]
    {
        // The logical data partition lives inside an extended container; resize
        // the container first so there is free space to expand the logical partition.
        let ext_nr = find_extended_partition(rootblk)?;
        run_cmd(
            PARTED_CMD,
            &[
                rootblk_str,
                PARTED_RESIZEPART,
                &ext_nr.to_string(),
                PARTED_RESIZE_FULL,
            ],
        )?;
    }

    run_cmd(
        PARTED_CMD,
        &[
            rootblk_str,
            PARTED_RESIZEPART,
            &part_nr.to_string(),
            PARTED_RESIZE_FULL,
        ],
    )?;

    // resize2fs requires a valid mtab entry; create a symlink to /proc/self/mounts
    // if one does not already exist.
    ensure_mtab()?;
    check_filesystem(&data_dev, FsType::Ext4)?;

    let data_dev_str = data_dev
        .to_str()
        .ok_or_else(|| ResizeDataError::NonUtf8Path(data_dev.clone()))?;
    run_cmd(RESIZE2FS_CMD, &[RESIZE2FS_FORCE, data_dev_str])?;

    run_cmd(SYNC_CMD, &[])?;

    bootloader.set_env(BootloaderEnvKey::ResizedData, Some("1"))?;

    log::info!("Data partition resize complete");
    Ok(())
}

/// Extract the trailing partition number from a block device path.
///
/// Works for all common Linux naming conventions:
/// - SATA/SCSI: `/dev/sda8` → 8
/// - NVMe: `/dev/nvme0n1p7` → 7
/// - eMMC: `/dev/mmcblk0p8` → 8
fn partition_number(dev: &Path) -> Option<u32> {
    let name = dev.file_name()?.to_str()?;
    let digits: String = name
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    let number: String = digits.chars().rev().collect();
    number.parse().ok()
}

/// Find the extended partition number on a DOS/MBR disk.
///
/// Invokes `parted print` and scans its output for a line whose type column
/// contains "extended". Only relevant for `dos` partition layouts.
#[cfg(feature = "dos")]
fn find_extended_partition(rootblk: &Path) -> ResizeResult<u32> {
    let out = Command::new(PARTED_CMD)
        .args([
            rootblk
                .to_str()
                .ok_or_else(|| ResizeDataError::NonUtf8Path(rootblk.to_path_buf()))?,
            PARTED_PRINT,
        ])
        .output()
        .map_err(ResizeDataError::Io)?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    parse_extended_partition_nr(&stdout)
        .ok_or_else(|| ResizeDataError::ExtendedPartitionNotFound(rootblk.to_path_buf()))
}

#[cfg(feature = "dos")]
fn parse_extended_partition_nr(parted_output: &str) -> Option<u32> {
    for line in parted_output.lines() {
        let lower = line.to_lowercase();
        if lower.contains("extended")
            && let Some(nr_str) = line.split_whitespace().next()
            && let Ok(nr) = nr_str.parse::<u32>()
        {
            return Some(nr);
        }
    }
    None
}

/// Ensure `/etc/mtab` is present and readable by resize2fs.
///
/// If no mtab exists (or a stale symlink is in its place), creates a symlink
/// to `/proc/self/mounts`. A real mtab file is left untouched.
fn ensure_mtab() -> ResizeResult<()> {
    ensure_mtab_at(Path::new(MTAB_PATH), Path::new(PROC_MOUNTS_PATH))
}

fn ensure_mtab_at(mtab: &Path, target: &Path) -> ResizeResult<()> {
    if mtab.exists() && !mtab.is_symlink() {
        return Ok(());
    }

    if mtab.is_symlink() {
        std::fs::remove_file(mtab)?;
    }

    symlink(target, mtab)?;
    Ok(())
}

/// Run an external command and return an error if it exits non-zero.
fn run_cmd(program: &str, args: &[&str]) -> ResizeResult<()> {
    if args.is_empty() {
        log::info!("Running: {}", program);
    } else {
        log::info!("Running: {} {}", program, args.join(" "));
    }

    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(ResizeDataError::Io)?;

    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        let output = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        return Err(ResizeDataError::CommandFailed {
            command: if args.is_empty() {
                program.to_string()
            } else {
                format!("{} {}", program, args.join(" "))
            },
            code,
            output,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_partition_number_sata() {
        assert_eq!(partition_number(Path::new("/dev/sda8")), Some(8));
    }

    #[test]
    fn test_partition_number_nvme() {
        assert_eq!(partition_number(Path::new("/dev/nvme0n1p7")), Some(7));
    }

    #[test]
    fn test_partition_number_mmc() {
        assert_eq!(partition_number(Path::new("/dev/mmcblk0p8")), Some(8));
    }

    #[test]
    fn test_partition_number_multi_digit() {
        assert_eq!(partition_number(Path::new("/dev/sda10")), Some(10));
    }

    #[test]
    fn test_partition_number_no_digits() {
        assert_eq!(partition_number(Path::new("/dev/sda")), None);
    }

    #[cfg(feature = "dos")]
    #[test]
    fn test_find_extended_partition_parses_output() {
        let output = "\
Model: ATA VBOX HARDDISK (scsi)
Disk /dev/sda: 8590MB
Sector size (logical/physical): 512B/512B
Partition Table: msdos
Disk Flags:

Number  Start   End     Size    Type      File system  Flags
 1      1049kB  500MB   499MB   primary   fat32        boot, esp
 2      500MB   1500MB  1000MB  primary   ext4
 3      1500MB  8589MB  7089MB  extended
 8      1501MB  8589MB  7088MB  logical   ext4";

        assert_eq!(parse_extended_partition_nr(output), Some(3));
    }

    #[cfg(feature = "dos")]
    #[test]
    fn test_find_extended_partition_not_found() {
        let output = "\
Number  Start   End     Size   File system  Name  Flags
 1      1049kB  500MB   499MB  fat32              boot, esp
 7      500MB   8589MB  8089MB ext4";

        assert_eq!(parse_extended_partition_nr(output), None);
    }

    #[test]
    fn test_resize_skips_when_data_partition_absent() {
        use crate::bootloader::MockBootloader;
        use crate::partition::{PartitionLayout, RootDevice};
        use std::collections::HashMap;

        let layout = PartitionLayout {
            partitions: HashMap::new(),
            device: RootDevice {
                base: std::path::PathBuf::from("/dev/sda"),
                partition_sep: "",
                root_partition: std::path::PathBuf::from("/dev/sda2"),
            },
        };
        let mut bl: Box<dyn crate::bootloader::Bootloader> = Box::new(MockBootloader::new());

        assert!(resize_if_needed(&layout, &mut bl).is_ok());
        assert!(bl.get_env(BootloaderEnvKey::ResizedData).unwrap().is_none());
    }

    #[test]
    fn test_ensure_mtab_creates_symlink_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let mtab = dir.path().join("mtab");
        let target = dir.path().join("mounts");
        std::fs::write(&target, "").unwrap();

        assert!(ensure_mtab_at(&mtab, &target).is_ok());
        assert!(mtab.is_symlink());
        assert!(mtab.exists());
    }

    #[test]
    fn test_ensure_mtab_leaves_real_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let mtab = dir.path().join("mtab");
        let target = dir.path().join("mounts");
        std::fs::write(&mtab, "original").unwrap();

        assert!(ensure_mtab_at(&mtab, &target).is_ok());
        assert!(!mtab.is_symlink());
        assert_eq!(std::fs::read_to_string(&mtab).unwrap(), "original");
    }

    #[test]
    fn test_ensure_mtab_replaces_stale_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let mtab = dir.path().join("mtab");
        let stale = dir.path().join("nonexistent");
        let target = dir.path().join("mounts");
        std::fs::write(&target, "").unwrap();

        std::os::unix::fs::symlink(&stale, &mtab).unwrap();
        assert!(!mtab.exists()); // dangling

        assert!(ensure_mtab_at(&mtab, &target).is_ok());
        assert!(mtab.is_symlink());
        assert!(mtab.exists());
    }
}
