//! Data partition auto-resize
//!
//! Expands the data partition and its ext4 filesystem to fill available disk
//! space on first boot. Guarded by the `resized-data` bootloader variable so
//! it runs exactly once.

use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use crate::bootloader::{Bootloader, BootloaderEnvKey};
use crate::error::{ResizeDataError, Result};
use crate::partition::PartitionName;

#[cfg(feature = "gpt")]
const SGDISK_CMD: &str = "/sbin/sgdisk";
const PARTED_CMD: &str = "/sbin/parted";
const E2FSCK_CMD: &str = "/sbin/e2fsck";
const RESIZE2FS_CMD: &str = "/sbin/resize2fs";
const SYNC_CMD: &str = "/bin/sync";

const MTAB_PATH: &str = "/etc/mtab";
const PROC_MOUNTS_PATH: &str = "/proc/self/mounts";

type ResizeResult<T> = std::result::Result<T, ResizeDataError>;

pub fn resize_if_needed(
    layout: &crate::partition::PartitionLayout,
    bootloader: Option<&mut dyn Bootloader>,
    _rootfs: &Path, // reserved: may be needed for chroot-relative paths in future callers
) -> Result<()> {
    let Some(bootloader) = bootloader else {
        log::warn!("Bootloader unavailable; skipping data partition resize");
        return Ok(());
    };

    if bootloader.get_env(BootloaderEnvKey::ResizedData)?.is_some() {
        log::info!("Data partition already resized, skipping");
        return Ok(());
    }

    let data_dev = match layout.partitions.get(&PartitionName::Data) {
        Some(d) => d.clone(),
        None => {
            log::warn!("Data partition not found in layout; skipping resize");
            return Ok(());
        }
    };
    let rootblk = &layout.device.base;

    let part_nr = partition_number(&data_dev).ok_or_else(|| {
        crate::error::InitramfsError::ResizeData(ResizeDataError::InvalidDevicePath(
            data_dev.clone(),
        ))
    })?;

    log::info!(
        "Resizing data partition: {} partition {}",
        rootblk.display(),
        part_nr
    );

    let rootblk_str = rootblk.to_str().ok_or_else(|| {
        crate::error::InitramfsError::ResizeData(ResizeDataError::NonUtf8Path(
            rootblk.to_path_buf(),
        ))
    })?;

    #[cfg(feature = "gpt")]
    {
        // Move backup GPT header to end of disk before resizing — required when
        // the disk was grown after the image was written (e.g. flash → larger medium).
        run_cmd(SGDISK_CMD, &[rootblk_str, "-e"])
            .map_err(crate::error::InitramfsError::ResizeData)?;
    }

    #[cfg(feature = "dos")]
    {
        // The logical data partition lives inside an extended container; resize
        // the container first so there is free space to expand the logical partition.
        let ext_nr =
            find_extended_partition(rootblk).map_err(crate::error::InitramfsError::ResizeData)?;
        run_cmd(
            PARTED_CMD,
            &[rootblk_str, "resizepart", &ext_nr.to_string(), "100%"],
        )
        .map_err(crate::error::InitramfsError::ResizeData)?;
    }

    run_cmd(
        PARTED_CMD,
        &[rootblk_str, "resizepart", &part_nr.to_string(), "100%"],
    )
    .map_err(crate::error::InitramfsError::ResizeData)?;

    // resize2fs requires a valid mtab entry; create a symlink to /proc/self/mounts
    // if one does not already exist.
    ensure_mtab().map_err(crate::error::InitramfsError::ResizeData)?;
    run_e2fsck(&data_dev).map_err(crate::error::InitramfsError::ResizeData)?;

    let data_dev_str = data_dev.to_str().ok_or_else(|| {
        crate::error::InitramfsError::ResizeData(ResizeDataError::NonUtf8Path(data_dev.clone()))
    })?;
    run_cmd(RESIZE2FS_CMD, &["-f", data_dev_str])
        .map_err(crate::error::InitramfsError::ResizeData)?;

    run_cmd(SYNC_CMD, &[]).map_err(crate::error::InitramfsError::ResizeData)?;

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
            "print",
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
    let mtab = Path::new(MTAB_PATH);
    let target = Path::new(PROC_MOUNTS_PATH);

    if mtab.exists() && !mtab.is_symlink() {
        return Ok(());
    }

    if mtab.is_symlink() {
        std::fs::remove_file(mtab)?;
    }

    symlink(target, mtab)?;
    Ok(())
}

/// Run `e2fsck -y` on a device, tolerating exit codes 0 and 1.
///
/// e2fsck exit code 1 means "errors were corrected" — that is a success
/// outcome for our purposes; we just need the filesystem to be consistent
/// before resize2fs runs.
fn run_e2fsck(dev: &Path) -> ResizeResult<()> {
    let dev_str = dev
        .to_str()
        .ok_or_else(|| ResizeDataError::NonUtf8Path(dev.to_path_buf()))?;
    log::info!("Running: {} -y {}", E2FSCK_CMD, dev_str);

    let out = Command::new(E2FSCK_CMD)
        .args(["-y", dev_str])
        .output()
        .map_err(ResizeDataError::Io)?;

    let code = out.status.code().unwrap_or(-1);
    // 0 = clean, 1 = errors corrected; both are acceptable
    if code > 1 {
        let output = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        return Err(ResizeDataError::CommandFailed {
            command: format!("{} -y {}", E2FSCK_CMD, dev_str),
            code,
            output,
        });
    }

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
            command: format!("{} {}", program, args.join(" ")),
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
}
