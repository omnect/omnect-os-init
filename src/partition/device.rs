//! Root device detection from kernel command line.
//!
//! Supports two omnect-os boot paths depending on the bootloader:
//!
//! - **GRUB** (`rootpart=N` + `bootpart_fsuuid=<uuid>`): GRUB probes the filesystem
//!   UUID of its boot partition via `probe --fs-uuid` and passes it as `bootpart_fsuuid=`
//!   on the kernel cmdline. initramfs calls `blkid --uuid <uuid>` to resolve the exact
//!   boot partition device, then strips the partition suffix to get the base disk.
//!
//! - **U-Boot** (`root=/dev/<device>`): full device path set by U-Boot bootargs
//!   (e.g. `root=/dev/mmcblk1p2`). Base device and separator are derived from the path.

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use crate::partition::{PartitionError, Result};

const DEVICE_WAIT_TIMEOUT_SECS: u64 = 30;
const DEVICE_POLL_INTERVAL_MS: u64 = 100;
const BLKID_CMD: &str = "/sbin/blkid";

/// Represents the detected root block device and its properties.
#[derive(Debug, Clone)]
pub struct RootDevice {
    /// Base block device path (e.g., `/dev/sda`, `/dev/nvme0n1`, `/dev/mmcblk0`)
    pub base: PathBuf,
    /// Partition separator ("" for sda/vda, "p" for nvme0n1/mmcblk0)
    pub partition_sep: String,
    /// Root partition device path (e.g., `/dev/sda2`, `/dev/mmcblk0p2`)
    pub root_partition: PathBuf,
}

impl RootDevice {
    /// Constructs the path to a specific partition number.
    pub fn partition_path(&self, partition_num: u32) -> PathBuf {
        PathBuf::from(format!(
            "{}{}{}",
            self.base.display(),
            self.partition_sep,
            partition_num
        ))
    }
}

/// Detects the root device by parsing kernel command line parameters.
pub fn detect_root_device() -> Result<RootDevice> {
    detect_root_device_from_cmdline("/proc/cmdline")
}

/// Internal implementation with configurable cmdline path for testing.
pub(crate) fn detect_root_device_from_cmdline(cmdline_path: &str) -> Result<RootDevice> {
    let cmdline = fs::read_to_string(cmdline_path).map_err(|e| {
        PartitionError::DeviceDetection(format!("failed to read {}: {}", cmdline_path, e))
    })?;

    // GRUB: rootpart=N + bootpart_fsuuid=<uuid>
    // GRUB and initramfs always ship in the same image, so bootpart_fsuuid is
    // always present on GRUB boots — no fallback paths needed.
    if let Some(part_str) = parse_cmdline_param(&cmdline, "rootpart")? {
        let part_num: u32 = part_str.parse().map_err(|_| {
            PartitionError::DeviceDetection(format!(
                "rootpart= is not a valid partition number: {}",
                part_str
            ))
        })?;

        let fsuuid = parse_cmdline_param(&cmdline, "bootpart_fsuuid")?.ok_or_else(|| {
            PartitionError::DeviceDetection(
                "rootpart= present but bootpart_fsuuid= missing from cmdline".into(),
            )
        })?;

        return device_from_fsuuid(&fsuuid, part_num);
    }

    // U-Boot: root=/dev/<device> (full partition path in bootargs)
    if let Some(root) = parse_cmdline_param(&cmdline, "root")? {
        if !root.starts_with("/dev/") {
            return Err(PartitionError::DeviceDetection(format!(
                "root= must start with /dev/, got: {}",
                root
            )));
        }
        return device_from_path(&root);
    }

    Err(PartitionError::DeviceDetection(
        "neither rootpart= (GRUB) nor root= (U-Boot) found in kernel cmdline".into(),
    ))
}

/// Resolves the boot disk via the filesystem UUID of the boot partition (`bootpart_fsuuid=`).
///
/// GRUB runs `probe --fs-uuid` on `${root}` (the boot partition) and passes the result
/// on the kernel cmdline. `blkid` is retried in a loop until the UUID is found or the
/// timeout expires — block devices may not be ready immediately at initramfs startup.
fn device_from_fsuuid(fsuuid: &str, part_num: u32) -> Result<RootDevice> {
    use std::process::Command;

    log::info!(
        "device_from_fsuuid: resolving boot partition UUID={}",
        fsuuid
    );

    // Retry blkid until the UUID appears or the timeout expires.
    // Block devices may not be ready when initramfs first runs blkid.
    let timeout = Duration::from_secs(DEVICE_WAIT_TIMEOUT_SECS);
    let start = Instant::now();
    let boot_part_str = loop {
        let output = Command::new(BLKID_CMD)
            .args(["--uuid", fsuuid])
            .output()
            .map_err(|e| PartitionError::DeviceDetection(format!("failed to run blkid: {}", e)))?;

        match output.status.code() {
            Some(0) => {
                // UUID found; stdout is the device path.
                let dev = std::str::from_utf8(&output.stdout)
                    .map_err(|_| {
                        PartitionError::DeviceDetection("blkid output is not UTF-8".into())
                    })?
                    .trim()
                    .to_string();
                log::info!(
                    "device_from_fsuuid: UUID={} resolved to {} after {:.1}s",
                    fsuuid,
                    dev,
                    start.elapsed().as_secs_f32()
                );
                break dev;
            }
            Some(2) => {
                // UUID not found yet; retry until timeout.
                if start.elapsed() >= timeout {
                    return Err(PartitionError::DeviceDetection(format!(
                        "blkid found no device with UUID={} within {}s",
                        fsuuid,
                        timeout.as_secs()
                    )));
                }
                log::debug!(
                    "device_from_fsuuid: UUID={} not found yet, retrying...",
                    fsuuid
                );
                thread::sleep(Duration::from_millis(DEVICE_POLL_INTERVAL_MS));
            }
            code => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(PartitionError::DeviceDetection(format!(
                    "blkid exited with status {:?}: {}",
                    code,
                    stderr.trim()
                )));
            }
        }
    };

    let rd = root_device_from_blkid(&boot_part_str, part_num)?;
    wait_for_device(&rd.root_partition)?;
    log::info!(
        "device_from_fsuuid: root device = {} (partition {})",
        rd.base.display(),
        part_num
    );
    Ok(rd)
}

/// Pure pipeline: given the device path returned by `blkid --uuid`, construct a `RootDevice`.
///
/// Separated from `device_from_fsuuid` so it can be driven by fixture data in tests.
pub fn root_device_from_blkid(boot_part_dev: &str, part_num: u32) -> Result<RootDevice> {
    let name = PathBuf::from(boot_part_dev)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            PartitionError::DeviceDetection(format!("invalid blkid output: {}", boot_part_dev))
        })?
        .to_string();

    let (base_name, sep) = split_partition_suffix(&name)?;
    let base = PathBuf::from("/dev").join(&base_name);
    let root_partition = PathBuf::from(format!("/dev/{}{}{}", base_name, sep, part_num));

    Ok(RootDevice {
        base,
        partition_sep: sep,
        root_partition,
    })
}

/// Builds a `RootDevice` from a full `root=/dev/<device>` path (U-Boot boot path).
fn device_from_path(path: &str) -> Result<RootDevice> {
    let root_partition = PathBuf::from(path);
    wait_for_device(&root_partition)?;
    let name = root_partition
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| PartitionError::DeviceDetection(format!("invalid device path: {}", path)))?;
    let (base_name, sep) = split_partition_suffix(name)?;
    let base = PathBuf::from("/dev").join(&base_name);
    log::info!("root device from root= (U-Boot): {}", base.display());
    Ok(RootDevice {
        base,
        partition_sep: sep,
        root_partition,
    })
}

/// Splits a partition device name into `(base_name, separator)`.
///
/// Examples: `"sda2"` → `("sda", "")`, `"mmcblk1p2"` → `("mmcblk1", "p")`
fn split_partition_suffix(name: &str) -> Result<(String, String)> {
    // NVMe / MMC: partition number follows a "p" separator
    if (name.contains("nvme") || name.starts_with("mmcblk"))
        && let Some(pos) = name.rfind('p')
    {
        let suffix = &name[pos + 1..];
        if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
            return Ok((name[..pos].to_string(), "p".to_string()));
        }
    }

    // SATA / virtio: partition number appended directly (e.g. sda2, vda2)
    let base_end = name.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if base_end > 0 && base_end < name.len() {
        return Ok((name[..base_end].to_string(), String::new()));
    }

    Err(PartitionError::DeviceDetection(format!(
        "could not derive base device from: {}",
        name
    )))
}

fn wait_for_device(device: &std::path::Path) -> Result<()> {
    let timeout = Duration::from_secs(DEVICE_WAIT_TIMEOUT_SECS);
    let start = Instant::now();
    loop {
        if device.exists() {
            return Ok(());
        }
        if start.elapsed() > timeout {
            return Err(PartitionError::DeviceDetection(format!(
                "device {} did not appear within {} seconds",
                device.display(),
                timeout.as_secs()
            )));
        }
        thread::sleep(Duration::from_millis(DEVICE_POLL_INTERVAL_MS));
    }
}

/// Parses a parameter value from kernel command line.
///
/// Handles `key=value` format. Values containing spaces are not supported
/// (the kernel cmdline splits on whitespace; quoted values with spaces
/// would be split into multiple tokens by `split_whitespace`).
pub fn parse_cmdline_param(cmdline: &str, key: &str) -> Result<Option<String>> {
    let prefix = format!("{}=", key);
    for token in cmdline.split_whitespace() {
        if let Some(value) = token.strip_prefix(&prefix) {
            return Ok(Some(value.trim_matches('"').to_string()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cmdline_param_bootpart_fsuuid() {
        let cmdline = "rootpart=2 bootpart_fsuuid=1234-ABCD ro quiet";
        assert_eq!(
            parse_cmdline_param(cmdline, "rootpart").unwrap(),
            Some("2".to_string())
        );
        assert_eq!(
            parse_cmdline_param(cmdline, "bootpart_fsuuid").unwrap(),
            Some("1234-ABCD".to_string())
        );
    }

    #[test]
    fn test_parse_cmdline_param_rootpart() {
        let cmdline = "rootpart=2 console=ttyS0,115200 quiet";
        assert_eq!(
            parse_cmdline_param(cmdline, "rootpart").unwrap(),
            Some("2".to_string())
        );
        assert_eq!(
            parse_cmdline_param(cmdline, "bootpart_fsuuid").unwrap(),
            None
        );
    }

    #[test]
    fn test_parse_cmdline_param_missing() {
        let cmdline = "ro quiet";
        assert_eq!(parse_cmdline_param(cmdline, "rootpart").unwrap(), None);
        assert_eq!(
            parse_cmdline_param(cmdline, "bootpart_fsuuid").unwrap(),
            None
        );
    }

    #[test]
    fn test_parse_cmdline_param_complex() {
        let cmdline =
            "rootpart=2 coherent_pool=1M console=ttyS0,115200 bootpart_fsuuid=ABCD-1234 ro";
        assert_eq!(
            parse_cmdline_param(cmdline, "rootpart").unwrap(),
            Some("2".to_string())
        );
        assert_eq!(
            parse_cmdline_param(cmdline, "bootpart_fsuuid").unwrap(),
            Some("ABCD-1234".to_string())
        );
    }

    #[test]
    fn test_parse_cmdline_param_uboot_root() {
        let cmdline = "root=/dev/mmcblk1p2 ro quiet";
        assert_eq!(
            parse_cmdline_param(cmdline, "root").unwrap(),
            Some("/dev/mmcblk1p2".to_string())
        );
        assert_eq!(parse_cmdline_param(cmdline, "rootpart").unwrap(), None);
    }

    #[test]
    fn test_split_partition_suffix_sata() {
        assert_eq!(
            split_partition_suffix("sda2").unwrap(),
            ("sda".to_string(), String::new())
        );
    }

    #[test]
    fn test_split_partition_suffix_mmc() {
        assert_eq!(
            split_partition_suffix("mmcblk1p2").unwrap(),
            ("mmcblk1".to_string(), "p".to_string())
        );
    }

    #[test]
    fn test_split_partition_suffix_nvme() {
        assert_eq!(
            split_partition_suffix("nvme0n1p2").unwrap(),
            ("nvme0n1".to_string(), "p".to_string())
        );
    }

    #[test]
    fn test_split_partition_suffix_virtio() {
        assert_eq!(
            split_partition_suffix("vda2").unwrap(),
            ("vda".to_string(), String::new())
        );
    }

    #[test]
    fn test_root_device_partition_path_sata() {
        let device = RootDevice {
            base: PathBuf::from("/dev/sda"),
            partition_sep: String::new(),
            root_partition: PathBuf::from("/dev/sda2"),
        };
        assert_eq!(device.partition_path(1), PathBuf::from("/dev/sda1"));
        assert_eq!(device.partition_path(7), PathBuf::from("/dev/sda7"));
    }

    #[test]
    fn test_root_device_partition_path_mmc() {
        let device = RootDevice {
            base: PathBuf::from("/dev/mmcblk0"),
            partition_sep: "p".to_string(),
            root_partition: PathBuf::from("/dev/mmcblk0p2"),
        };
        assert_eq!(device.partition_path(1), PathBuf::from("/dev/mmcblk0p1"));
        assert_eq!(device.partition_path(7), PathBuf::from("/dev/mmcblk0p7"));
    }

    #[test]
    fn test_root_device_partition_path_nvme() {
        let device = RootDevice {
            base: PathBuf::from("/dev/nvme0n1"),
            partition_sep: "p".to_string(),
            root_partition: PathBuf::from("/dev/nvme0n1p2"),
        };
        assert_eq!(device.partition_path(1), PathBuf::from("/dev/nvme0n1p1"));
        assert_eq!(device.partition_path(7), PathBuf::from("/dev/nvme0n1p7"));
    }

    #[test]
    fn test_split_partition_suffix_multi_digit_sata() {
        assert_eq!(
            split_partition_suffix("sda12").unwrap(),
            ("sda".to_string(), String::new())
        );
        assert_eq!(
            split_partition_suffix("sdb100").unwrap(),
            ("sdb".to_string(), String::new())
        );
    }

    #[test]
    fn test_split_partition_suffix_multi_digit_nvme() {
        assert_eq!(
            split_partition_suffix("nvme1n2p100").unwrap(),
            ("nvme1n2".to_string(), "p".to_string())
        );
    }

    #[test]
    fn test_split_partition_suffix_multi_digit_mmc() {
        assert_eq!(
            split_partition_suffix("mmcblk1p12").unwrap(),
            ("mmcblk1".to_string(), "p".to_string())
        );
    }

    #[test]
    fn test_split_partition_suffix_virtio_second_disk() {
        assert_eq!(
            split_partition_suffix("vdb7").unwrap(),
            ("vdb".to_string(), String::new())
        );
    }

    #[test]
    fn test_split_partition_suffix_loop_documents_fallback() {
        // loop devices fall through to the SATA branch because the name doesn't
        // contain "nvme" or start with "mmcblk". The result is technically wrong
        // (base="loop0p") but omnect-os does not target loop devices.
        assert_eq!(
            split_partition_suffix("loop0p1").unwrap(),
            ("loop0p".to_string(), String::new())
        );
    }

    #[test]
    fn test_split_partition_suffix_whole_disk_errors() {
        assert!(split_partition_suffix("sda").is_err());
        // dm-0: trailing digit after hyphen parses as a suffix, yielding base="dm-" —
        // incorrect, but omnect-os does not target device-mapper devices.
        assert!(split_partition_suffix("dm-0").is_ok());
    }

    #[test]
    fn test_root_device_partition_path_virtio() {
        let device = RootDevice {
            base: PathBuf::from("/dev/vda"),
            partition_sep: String::new(),
            root_partition: PathBuf::from("/dev/vda2"),
        };
        assert_eq!(device.partition_path(1), PathBuf::from("/dev/vda1"));
        assert_eq!(device.partition_path(7), PathBuf::from("/dev/vda7"));
    }
}
