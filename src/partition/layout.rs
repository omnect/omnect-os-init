//! Partition layout
//!
//! Builds the partition map using the partition table type selected at build
//! time via the `gpt` or `dos` Cargo feature.  There is no runtime detection:
//! the table type is a fixed property of the Yocto image.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::error::PartitionError;
use crate::partition::RootDevice;

/// Partition names used in omnect-os
pub mod partition_names {
    pub const BOOT: &str = "boot";
    pub const ROOT_A: &str = "rootA";
    pub const ROOT_B: &str = "rootB";
    pub const FACTORY: &str = "factory";
    pub const CERT: &str = "cert";
    pub const ETC: &str = "etc";
    pub const DATA: &str = "data";
    pub const EXTENDED: &str = "extended";
    pub const ROOT_CURRENT: &str = "rootCurrent";
    pub const ROOTBLK: &str = "rootblk";
}

/// Partition layout for a block device
#[derive(Debug, Clone)]
pub struct PartitionLayout {
    /// Map of partition name to device path
    pub partitions: HashMap<String, PathBuf>,
    /// The root device
    pub device: RootDevice,
}

impl PartitionLayout {
    /// Builds the partition map for the given root device.
    ///
    /// Partition numbering is selected at compile time via the `gpt` or `dos` feature.
    pub fn new(device: RootDevice) -> crate::partition::Result<Self> {
        let partitions = build_partition_map(&device)?;
        Ok(Self { partitions, device })
    }

    /// Get the device path for a named partition
    pub fn get(&self, name: &str) -> Option<&PathBuf> {
        self.partitions.get(name)
    }

    /// Check if current root is rootA (partition 2)
    fn is_root_a(&self) -> bool {
        partition_suffix(&self.device.root_partition) == Some(PARTITION_NUM_ROOT_A)
    }

    /// Get the current root partition path (rootA or rootB based on boot).
    ///
    /// Falls back to reconstructing the path from the device if the map entry
    /// is absent (indicates incomplete initialisation — should not occur in normal boot).
    pub fn root_current(&self) -> PathBuf {
        if self.is_root_a() {
            self.partitions
                .get(partition_names::ROOT_A)
                .cloned()
                .unwrap_or_else(|| {
                    log::warn!("rootA not in partition map; reconstructing path");
                    self.device.partition_path(PARTITION_NUM_ROOT_A)
                })
        } else {
            self.partitions
                .get(partition_names::ROOT_B)
                .cloned()
                .unwrap_or_else(|| {
                    log::warn!("rootB not in partition map; reconstructing path");
                    self.device.partition_path(PARTITION_NUM_ROOT_B)
                })
        }
    }
}

/// Partition numbers shared by both GPT and DOS layouts
const PARTITION_NUM_BOOT: u32 = 1;
const PARTITION_NUM_ROOT_A: u32 = 2;
const PARTITION_NUM_ROOT_B: u32 = 3;

/// GPT layout — partitions 4-7 are all primary
#[cfg(feature = "gpt")]
const PARTITION_NUM_FACTORY: u32 = 4;
#[cfg(feature = "gpt")]
const PARTITION_NUM_CERT: u32 = 5;
#[cfg(feature = "gpt")]
const PARTITION_NUM_ETC: u32 = 6;
#[cfg(feature = "gpt")]
const PARTITION_NUM_DATA: u32 = 7;

/// DOS layout — partition 4 is the extended container; logical partitions start at 5
#[cfg(feature = "dos")]
const PARTITION_NUM_EXTENDED: u32 = 4;
#[cfg(feature = "dos")]
const PARTITION_NUM_FACTORY: u32 = 5;
#[cfg(feature = "dos")]
const PARTITION_NUM_CERT: u32 = 6;
#[cfg(feature = "dos")]
const PARTITION_NUM_ETC: u32 = 7;
#[cfg(feature = "dos")]
const PARTITION_NUM_DATA: u32 = 8;

/// Parse the trailing numeric partition suffix from a device path.
///
/// Examples: `sda2` → Some(2), `mmcblk0p3` → Some(3), `nvme0n1p12` → Some(12).
/// Returns None if the path has no file name or no trailing digit suffix.
///
/// Uses the *trailing* digit run, not the first digit found, so that devices
/// like `mmcblk0p2` (which contain digits in the base name) are handled correctly.
fn partition_suffix(path: &std::path::Path) -> Option<u32> {
    let s = path.file_name().and_then(|s| s.to_str())?;
    let digit_start = s
        .rfind(|c: char| !c.is_ascii_digit())
        .map(|i| i + 1)
        .unwrap_or(0);
    s[digit_start..].parse().ok()
}

/// Build the partition map for the given device.
///
/// Partition numbering is selected at compile time via the `gpt` or `dos` feature.
/// Exactly one of `gpt` or `dos` must be enabled; build.rs enforces this.
fn build_partition_map(device: &RootDevice) -> crate::partition::Result<HashMap<String, PathBuf>> {
    let mut partitions = HashMap::new();

    partitions.insert(
        partition_names::BOOT.to_string(),
        device.partition_path(PARTITION_NUM_BOOT),
    );
    partitions.insert(
        partition_names::ROOT_A.to_string(),
        device.partition_path(PARTITION_NUM_ROOT_A),
    );
    partitions.insert(
        partition_names::ROOT_B.to_string(),
        device.partition_path(PARTITION_NUM_ROOT_B),
    );

    let root_current = match partition_suffix(&device.root_partition) {
        Some(n) if n == PARTITION_NUM_ROOT_A => device.partition_path(PARTITION_NUM_ROOT_A),
        Some(n) if n == PARTITION_NUM_ROOT_B => device.partition_path(PARTITION_NUM_ROOT_B),
        _ => {
            return Err(PartitionError::UnknownRootPartition {
                path: device.root_partition.clone(),
            });
        }
    };
    partitions.insert(partition_names::ROOT_CURRENT.to_string(), root_current);

    #[cfg(feature = "dos")]
    partitions.insert(
        partition_names::EXTENDED.to_string(),
        device.partition_path(PARTITION_NUM_EXTENDED),
    );

    partitions.insert(
        partition_names::FACTORY.to_string(),
        device.partition_path(PARTITION_NUM_FACTORY),
    );
    partitions.insert(
        partition_names::CERT.to_string(),
        device.partition_path(PARTITION_NUM_CERT),
    );
    partitions.insert(
        partition_names::ETC.to_string(),
        device.partition_path(PARTITION_NUM_ETC),
    );
    partitions.insert(
        partition_names::DATA.to_string(),
        device.partition_path(PARTITION_NUM_DATA),
    );

    Ok(partitions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sda_root_a() -> RootDevice {
        RootDevice {
            base: PathBuf::from("/dev/sda"),
            partition_sep: "",
            root_partition: PathBuf::from("/dev/sda2"),
        }
    }

    #[cfg(feature = "gpt")]
    fn nvme_root_a() -> RootDevice {
        RootDevice {
            base: PathBuf::from("/dev/nvme0n1"),
            partition_sep: "p",
            root_partition: PathBuf::from("/dev/nvme0n1p2"),
        }
    }

    fn mmc_root_b() -> RootDevice {
        RootDevice {
            base: PathBuf::from("/dev/mmcblk0"),
            partition_sep: "p",
            root_partition: PathBuf::from("/dev/mmcblk0p3"),
        }
    }

    #[cfg(feature = "gpt")]
    #[test]
    fn test_partition_map_gpt_sata() {
        let map = build_partition_map(&sda_root_a()).unwrap();

        assert_eq!(
            map.get(partition_names::BOOT),
            Some(&PathBuf::from("/dev/sda1"))
        );
        assert_eq!(
            map.get(partition_names::ROOT_A),
            Some(&PathBuf::from("/dev/sda2"))
        );
        assert_eq!(
            map.get(partition_names::ROOT_B),
            Some(&PathBuf::from("/dev/sda3"))
        );
        assert_eq!(
            map.get(partition_names::FACTORY),
            Some(&PathBuf::from("/dev/sda4"))
        );
        assert_eq!(
            map.get(partition_names::CERT),
            Some(&PathBuf::from("/dev/sda5"))
        );
        assert_eq!(
            map.get(partition_names::ETC),
            Some(&PathBuf::from("/dev/sda6"))
        );
        assert_eq!(
            map.get(partition_names::DATA),
            Some(&PathBuf::from("/dev/sda7"))
        );
        assert_eq!(map.get(partition_names::EXTENDED), None);
    }

    #[cfg(feature = "dos")]
    #[test]
    fn test_partition_map_dos_sata() {
        let map = build_partition_map(&sda_root_a()).unwrap();

        assert_eq!(
            map.get(partition_names::BOOT),
            Some(&PathBuf::from("/dev/sda1"))
        );
        assert_eq!(
            map.get(partition_names::ROOT_A),
            Some(&PathBuf::from("/dev/sda2"))
        );
        assert_eq!(
            map.get(partition_names::ROOT_B),
            Some(&PathBuf::from("/dev/sda3"))
        );
        assert_eq!(
            map.get(partition_names::EXTENDED),
            Some(&PathBuf::from("/dev/sda4"))
        );
        assert_eq!(
            map.get(partition_names::FACTORY),
            Some(&PathBuf::from("/dev/sda5"))
        );
        assert_eq!(
            map.get(partition_names::CERT),
            Some(&PathBuf::from("/dev/sda6"))
        );
        assert_eq!(
            map.get(partition_names::ETC),
            Some(&PathBuf::from("/dev/sda7"))
        );
        assert_eq!(
            map.get(partition_names::DATA),
            Some(&PathBuf::from("/dev/sda8"))
        );
    }

    #[cfg(feature = "gpt")]
    #[test]
    fn test_partition_map_gpt_nvme() {
        let map = build_partition_map(&nvme_root_a()).unwrap();

        assert_eq!(
            map.get(partition_names::BOOT),
            Some(&PathBuf::from("/dev/nvme0n1p1"))
        );
        assert_eq!(
            map.get(partition_names::ROOT_A),
            Some(&PathBuf::from("/dev/nvme0n1p2"))
        );
        assert_eq!(
            map.get(partition_names::DATA),
            Some(&PathBuf::from("/dev/nvme0n1p7"))
        );
    }

    #[cfg(feature = "dos")]
    #[test]
    fn test_partition_map_dos_mmc() {
        let map = build_partition_map(&mmc_root_b()).unwrap();

        assert_eq!(
            map.get(partition_names::BOOT),
            Some(&PathBuf::from("/dev/mmcblk0p1"))
        );
        assert_eq!(
            map.get(partition_names::DATA),
            Some(&PathBuf::from("/dev/mmcblk0p8"))
        );
    }

    #[test]
    fn test_root_current_root_a() {
        let device = sda_root_a();
        let layout = PartitionLayout::new(device).unwrap();
        assert_eq!(layout.root_current(), PathBuf::from("/dev/sda2"));
    }

    #[test]
    fn test_root_current_root_b() {
        let device = mmc_root_b();
        let layout = PartitionLayout::new(device).unwrap();
        assert_eq!(layout.root_current(), PathBuf::from("/dev/mmcblk0p3"));
    }

    #[test]
    fn test_partition_suffix() {
        assert_eq!(partition_suffix(&PathBuf::from("/dev/sda2")), Some(2));
        assert_eq!(partition_suffix(&PathBuf::from("/dev/mmcblk0p2")), Some(2));
        assert_eq!(partition_suffix(&PathBuf::from("/dev/mmcblk0p3")), Some(3));
        assert_eq!(
            partition_suffix(&PathBuf::from("/dev/nvme0n1p12")),
            Some(12)
        );
        assert_eq!(partition_suffix(&PathBuf::from("/dev/sda")), None);
    }
}
