//! Root device detection from kernel command line parameters.
//!
//! Parses `/proc/cmdline` for `root=/dev/<device>` to determine the root block device.

use std::fs;
use std::path::{Path, PathBuf};

use crate::partition::{PartitionError, Result};

/// Represents the detected root block device and its properties.
#[derive(Debug, Clone)]
pub struct RootDevice {
    /// Base block device path (e.g., `/dev/sda`, `/dev/nvme0n1`, `/dev/mmcblk0`)
    pub base: PathBuf,
    /// Partition separator ("" for sda, "p" for nvme0n1p, mmcblk0p)
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
///
/// # Expected format
/// `root=/dev/<device>` - Direct device path (e.g., `/dev/mmcblk0p2`, `/dev/sda2`)
///
/// # Errors
/// Returns error if:
/// - Cannot read `/proc/cmdline`
/// - `root=` parameter is missing or malformed
/// - Device path doesn't exist or cannot be resolved
pub fn detect_root_device() -> Result<RootDevice> {
    detect_root_device_from_cmdline("/proc/cmdline")
}

/// Internal implementation with configurable path for testing.
pub(crate) fn detect_root_device_from_cmdline(cmdline_path: &str) -> Result<RootDevice> {
    let cmdline = fs::read_to_string(cmdline_path).map_err(|e| {
        PartitionError::DeviceDetection(format!("failed to read {}: {}", cmdline_path, e))
    })?;

    // Parse root= parameter from cmdline
    let root_param = parse_cmdline_param(&cmdline, "root")?
        .ok_or_else(|| PartitionError::DeviceDetection("missing root= parameter".into()))?;

    // Validate format: must start with /dev/
    if !root_param.starts_with("/dev/") {
        return Err(PartitionError::DeviceDetection(format!(
            "root= must be a device path starting with /dev/, got: {}",
            root_param
        )));
    }

    let partition_path = PathBuf::from(&root_param);
    if !partition_path.exists() {
        return Err(PartitionError::DeviceDetection(format!(
            "root device {} does not exist",
            root_param
        )));
    }

    // Canonicalize to resolve any symlinks
    let partition_path = fs::canonicalize(&partition_path).map_err(|e| {
        PartitionError::DeviceDetection(format!("failed to canonicalize {}: {}", root_param, e))
    })?;

    // Derive base block device from partition path
    let (base, partition_sep) = derive_base_device(&partition_path)?;

    // Optionally validate against omnect_rootblk hint
    if let Some(hint) = parse_cmdline_param(&cmdline, "omnect_rootblk")? {
        let hint_path = PathBuf::from(&hint);
        if hint_path != base {
            log::warn!(
                "omnect_rootblk hint {} differs from detected base device {}",
                hint,
                base.display()
            );
        }
    }

    Ok(RootDevice {
        base,
        partition_sep,
        root_partition: partition_path,
    })
}

/// Parses a parameter value from kernel command line.
///
/// Handles both `key=value` and `key="value with spaces"` formats.
fn parse_cmdline_param(cmdline: &str, key: &str) -> Result<Option<String>> {
    let prefix = format!("{}=", key);

    for token in cmdline.split_whitespace() {
        if let Some(value) = token.strip_prefix(&prefix) {
            // Handle quoted values
            let value = value.trim_matches('"');
            return Ok(Some(value.to_string()));
        }
    }

    Ok(None)
}

/// Derives the base block device from a partition device path.
///
/// Examples:
/// - `/dev/sda2` → (`/dev/sda`, "")
/// - `/dev/nvme0n1p2` → (`/dev/nvme0n1`, "p")
/// - `/dev/mmcblk0p2` → (`/dev/mmcblk0`, "p")
fn derive_base_device(partition_path: &Path) -> Result<(PathBuf, String)> {
    let partition_name = partition_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            PartitionError::DeviceDetection(format!(
                "invalid partition path: {}",
                partition_path.display()
            ))
        })?;

    let parent = partition_path
        .parent()
        .unwrap_or_else(|| Path::new("/dev"));

    // Try different partition naming schemes
    // NVMe/MMC: nvme0n1p2, mmcblk0p2 - partition number after 'p'
    // SATA/virtio: sda2, vda2 - partition number directly appended

    // Check for 'p' separator (NVMe, MMC)
    if let Some(pos) = partition_name.rfind('p') {
        let suffix = &partition_name[pos + 1..];
        if suffix.chars().all(|c| c.is_ascii_digit()) && !suffix.is_empty() {
            let base_name = &partition_name[..pos];
            // Verify this is actually a block device by checking sysfs
            let sysfs_path = format!("/sys/block/{}", base_name);
            if Path::new(&sysfs_path).exists() {
                let base_path = parent.join(base_name);
                return Ok((base_path, "p".to_string()));
            }
        }
    }

    // Try direct numeric suffix (SATA, virtio)
    let mut base_end = partition_name.len();
    while base_end > 0 && partition_name[..base_end].ends_with(|c: char| c.is_ascii_digit()) {
        base_end -= 1;
    }

    if base_end < partition_name.len() && base_end > 0 {
        let base_name = &partition_name[..base_end];
        let sysfs_path = format!("/sys/block/{}", base_name);
        if Path::new(&sysfs_path).exists() {
            let base_path = parent.join(base_name);
            return Ok((base_path, String::new()));
        }
    }

    Err(PartitionError::DeviceDetection(format!(
        "could not derive base device from {}",
        partition_path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cmdline_param_direct_device() {
        let cmdline = "root=/dev/mmcblk0p2 ro quiet";
        assert_eq!(
            parse_cmdline_param(cmdline, "root").unwrap(),
            Some("/dev/mmcblk0p2".to_string())
        );
    }

    #[test]
    fn test_parse_cmdline_param_missing() {
        let cmdline = "ro quiet";
        assert_eq!(parse_cmdline_param(cmdline, "root").unwrap(), None);
    }

    #[test]
    fn test_parse_cmdline_param_complex() {
        // Real-world example from Raspberry Pi
        let cmdline = "root=/dev/mmcblk0p2 coherent_pool=1M 8250.nr_uarts=1 \
                       console=tty0 console=ttyS0,115200 rdinit=/bin/bash";
        assert_eq!(
            parse_cmdline_param(cmdline, "root").unwrap(),
            Some("/dev/mmcblk0p2".to_string())
        );
        assert_eq!(
            parse_cmdline_param(cmdline, "coherent_pool").unwrap(),
            Some("1M".to_string())
        );
        assert_eq!(
            parse_cmdline_param(cmdline, "rdinit").unwrap(),
            Some("/bin/bash".to_string())
        );
    }

    #[test]
    fn test_parse_cmdline_omnect_rootblk() {
        let cmdline = "root=/dev/sda2 omnect_rootblk=/dev/sda ro";
        assert_eq!(
            parse_cmdline_param(cmdline, "omnect_rootblk").unwrap(),
            Some("/dev/sda".to_string())
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
}
