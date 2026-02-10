//! Root device detection
//!
//! Detects the root block device using stat(2) + sysfs lookup.
//! Works across all device types (SATA, NVMe, MMC, virtio) without hardcoded names.

use std::path::{Path, PathBuf};

use crate::partition::Result;
use crate::error::PartitionError;

/// Sysfs path for block device lookups by major:minor
const SYSFS_DEV_BLOCK: &str = "/sys/dev/block";

/// Sysfs path for checking if a device is a block device
const SYSFS_BLOCK: &str = "/sys/block";

/// Device path prefix
const DEV_PATH: &str = "/dev";

/// Represents a detected root block device
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootDevice {
    /// Base block device path (e.g., /dev/sda, /dev/nvme0n1, /dev/mmcblk0)
    pub path: PathBuf,
    /// Device name without /dev/ prefix (e.g., sda, nvme0n1, mmcblk0)
    pub name: String,
    /// Partition separator ("" for sda, "p" for nvme0n1/mmcblk0)
    pub partition_sep: String,
    /// Root partition number (e.g., 2 for rootA or 3 for rootB)
    pub root_partition: u32,
}

impl RootDevice {
    /// Get the device path for a specific partition number
    pub fn partition_path(&self, partition: u32) -> PathBuf {
        PathBuf::from(format!(
            "{}{}{}",
            self.path.display(),
            self.partition_sep,
            partition
        ))
    }

    /// Get the device name for a specific partition number
    pub fn partition_name(&self, partition: u32) -> String {
        format!("{}{}{}", self.name, self.partition_sep, partition)
    }
}

/// Detect the root block device from the current root filesystem
///
/// Uses stat(2) to get major:minor of "/" and looks up the device via sysfs.
/// This method works for all device types without hardcoding device names.
pub fn detect_root_device() -> Result<RootDevice> {
    // Get major:minor of root filesystem
    let root_stat = std::fs::metadata("/")?;

    #[cfg(unix)]
    let (major, minor) = {
        use std::os::unix::fs::MetadataExt;
        let dev = root_stat.dev();
        // Extract major and minor from dev_t
        // On Linux: major = (dev >> 8) & 0xff, minor = dev & 0xff (simplified)
        // Using nix for proper extraction
        (
            nix::sys::stat::major(dev),
            nix::sys::stat::minor(dev),
        )
    };

    #[cfg(not(unix))]
    let (major, minor) = {
        return Err(PartitionError::RootDeviceNotFound(
            "Non-Unix platform not supported".to_string(),
        ));
    };

    // Lookup device name via sysfs
    // /sys/dev/block/8:2 -> symlink to ../../devices/.../sda/sda2
    let sysfs_path = PathBuf::from(format!("{}/{}:{}", SYSFS_DEV_BLOCK, major, minor));

    if !sysfs_path.exists() {
        return Err(PartitionError::RootDeviceNotFound(format!(
            "sysfs path not found: {}",
            sysfs_path.display()
        )));
    }

    let device_link = std::fs::read_link(&sysfs_path)?;
    let partition_name = device_link
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            PartitionError::RootDeviceNotFound(format!(
                "Invalid sysfs link: {}",
                device_link.display()
            ))
        })?;

    // Parse the partition name to extract base device and partition number
    let (base_name, partition_sep, partition_num) = parse_partition_name(partition_name)?;

    let device_path = PathBuf::from(format!("{}/{}", DEV_PATH, base_name));

    // Verify the base device exists
    if !device_path.exists() {
        return Err(PartitionError::RootDeviceNotFound(format!(
            "Block device not found: {}",
            device_path.display()
        )));
    }

    Ok(RootDevice {
        path: device_path,
        name: base_name.to_string(),
        partition_sep: partition_sep.to_string(),
        root_partition: partition_num,
    })
}

/// Parse a partition name into base device name, separator, and partition number
///
/// Examples:
/// - "sda2" -> ("sda", "", 2)
/// - "nvme0n1p2" -> ("nvme0n1", "p", 2)
/// - "mmcblk0p2" -> ("mmcblk0", "p", 2)
/// - "vda2" -> ("vda", "", 2)
fn parse_partition_name(name: &str) -> Result<(&str, &str, u32)> {
    // Find the trailing digits (partition number)
    let digit_start = name
        .rfind(|c: char| !c.is_ascii_digit())
        .map(|i| i + 1)
        .unwrap_or(0);

    if digit_start == 0 || digit_start >= name.len() {
        return Err(PartitionError::RootDeviceNotFound(format!(
            "Cannot parse partition number from: {}",
            name
        )));
    }

    let partition_str = &name[digit_start..];
    let partition_num: u32 = partition_str.parse().map_err(|_| {
        PartitionError::RootDeviceNotFound(format!(
            "Invalid partition number in: {}",
            name
        ))
    })?;

    let base_with_sep = &name[..digit_start];

    // Check if there's a 'p' separator (NVMe, MMC)
    let (base_name, separator) = if base_with_sep.ends_with('p') {
        let potential_base = &base_with_sep[..base_with_sep.len() - 1];
        // Verify this is actually a block device (not just ending in 'p')
        let sysfs_check = PathBuf::from(format!("{}/{}", SYSFS_BLOCK, potential_base));
        if sysfs_check.exists() {
            (potential_base, "p")
        } else {
            // No 'p' separator, the 'p' is part of the device name
            (base_with_sep, "")
        }
    } else {
        (base_with_sep, "")
    };

    // Final verification: check the base device exists in sysfs
    let sysfs_base = PathBuf::from(format!("{}/{}", SYSFS_BLOCK, base_name));
    if !sysfs_base.exists() {
        return Err(PartitionError::RootDeviceNotFound(format!(
            "Base block device not found in sysfs: {}",
            base_name
        )));
    }

    Ok((base_name, separator, partition_num))
}

/// Strip partition from a device name, returning base name and separator
///
/// Public helper for use in partition layout detection.
pub fn strip_partition_from_name(partition_name: &str) -> Result<(String, String)> {
    let (base, sep, _) = parse_partition_name(partition_name)?;
    Ok((base.to_string(), sep.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests use mock data since actual sysfs access requires root
    // Integration tests on real hardware will validate the full flow

    #[test]
    fn test_root_device_partition_path() {
        let device = RootDevice {
            path: PathBuf::from("/dev/sda"),
            name: "sda".to_string(),
            partition_sep: "".to_string(),
            root_partition: 2,
        };

        assert_eq!(device.partition_path(1), PathBuf::from("/dev/sda1"));
        assert_eq!(device.partition_path(2), PathBuf::from("/dev/sda2"));
        assert_eq!(device.partition_path(7), PathBuf::from("/dev/sda7"));
    }

    #[test]
    fn test_root_device_partition_path_nvme() {
        let device = RootDevice {
            path: PathBuf::from("/dev/nvme0n1"),
            name: "nvme0n1".to_string(),
            partition_sep: "p".to_string(),
            root_partition: 2,
        };

        assert_eq!(device.partition_path(1), PathBuf::from("/dev/nvme0n1p1"));
        assert_eq!(device.partition_path(2), PathBuf::from("/dev/nvme0n1p2"));
    }

    #[test]
    fn test_root_device_partition_path_mmc() {
        let device = RootDevice {
            path: PathBuf::from("/dev/mmcblk0"),
            name: "mmcblk0".to_string(),
            partition_sep: "p".to_string(),
            root_partition: 2,
        };

        assert_eq!(device.partition_path(1), PathBuf::from("/dev/mmcblk0p1"));
        assert_eq!(device.partition_path(5), PathBuf::from("/dev/mmcblk0p5"));
    }

    #[test]
    fn test_root_device_partition_name() {
        let device = RootDevice {
            path: PathBuf::from("/dev/sda"),
            name: "sda".to_string(),
            partition_sep: "".to_string(),
            root_partition: 2,
        };

        assert_eq!(device.partition_name(1), "sda1");
        assert_eq!(device.partition_name(2), "sda2");
    }

    #[test]
    fn test_root_device_partition_name_nvme() {
        let device = RootDevice {
            path: PathBuf::from("/dev/nvme0n1"),
            name: "nvme0n1".to_string(),
            partition_sep: "p".to_string(),
            root_partition: 2,
        };

        assert_eq!(device.partition_name(1), "nvme0n1p1");
        assert_eq!(device.partition_name(2), "nvme0n1p2");
    }
}
