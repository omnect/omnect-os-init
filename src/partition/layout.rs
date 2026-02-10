//! Partition layout detection
//!
//! Detects GPT vs DOS partition tables and builds a partition map
//! with appropriate partition numbers for each type.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::PartitionError;
use crate::partition::{Result, RootDevice};

/// Command to query partition table
const SFDISK_CMD: &str = "sfdisk";

/// Partition table types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionTableType {
    /// GUID Partition Table (modern, used on x86-64 EFI)
    Gpt,
    /// DOS/MBR partition table (legacy, used on some ARM)
    Dos,
}

impl std::fmt::Display for PartitionTableType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gpt => write!(f, "GPT"),
            Self::Dos => write!(f, "DOS/MBR"),
        }
    }
}

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
    /// Partition table type
    pub table_type: PartitionTableType,
    /// Map of partition name to device path
    pub partitions: HashMap<String, PathBuf>,
    /// The root device
    pub device: RootDevice,
}

impl PartitionLayout {
    /// Detect partition layout from a root device
    pub fn detect(device: &RootDevice) -> Result<Self> {
        let table_type = detect_partition_table_type(&device.path)?;
        let partitions = build_partition_map(device, table_type);

        Ok(Self {
            table_type,
            partitions,
            device: device.clone(),
        })
    }

    /// Get the device path for a named partition
    pub fn get(&self, name: &str) -> Option<&PathBuf> {
        self.partitions.get(name)
    }

    /// Get the current root partition path (rootA or rootB based on boot)
    pub fn root_current(&self) -> PathBuf {
        if self.device.root_partition == PARTITION_NUM_ROOT_A {
            self.partitions
                .get(partition_names::ROOT_A)
                .cloned()
                .unwrap_or_else(|| self.device.partition_path(PARTITION_NUM_ROOT_A))
        } else {
            self.partitions
                .get(partition_names::ROOT_B)
                .cloned()
                .unwrap_or_else(|| self.device.partition_path(PARTITION_NUM_ROOT_B))
        }
    }
}

/// Partition numbers for GPT layout
const PARTITION_NUM_BOOT: u32 = 1;
const PARTITION_NUM_ROOT_A: u32 = 2;
const PARTITION_NUM_ROOT_B: u32 = 3;
const PARTITION_NUM_FACTORY_GPT: u32 = 4;
const PARTITION_NUM_CERT_GPT: u32 = 5;
const PARTITION_NUM_ETC_GPT: u32 = 6;
const PARTITION_NUM_DATA_GPT: u32 = 7;

/// Partition numbers for DOS layout (with extended partition)
const PARTITION_NUM_EXTENDED_DOS: u32 = 4;
const PARTITION_NUM_FACTORY_DOS: u32 = 5;
const PARTITION_NUM_CERT_DOS: u32 = 6;
const PARTITION_NUM_ETC_DOS: u32 = 7;
const PARTITION_NUM_DATA_DOS: u32 = 8;

/// Detect partition table type using sfdisk
fn detect_partition_table_type(device: &Path) -> Result<PartitionTableType> {
    let output = Command::new(SFDISK_CMD)
        .arg("-l")
        .arg(device)
        .output()
        .map_err(|e| PartitionError::InvalidPartitionTable {
            device: device.to_path_buf(),
            reason: format!("Failed to run sfdisk: {}", e),
        })?;

    if !output.status.success() {
        return Err(PartitionError::InvalidPartitionTable {
            device: device.to_path_buf(),
            reason: format!(
                "sfdisk failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse sfdisk output to determine table type
    // Look for "Disklabel type: gpt" or "Disklabel type: dos"
    for line in stdout.lines() {
        let line_lower = line.to_lowercase();
        if line_lower.contains("disklabel type:") || line_lower.contains("label-id:") {
            if line_lower.contains("gpt") {
                return Ok(PartitionTableType::Gpt);
            } else if line_lower.contains("dos") || line_lower.contains("mbr") {
                return Ok(PartitionTableType::Dos);
            }
        }
        // Alternative format: "label: gpt" or "label: dos"
        if line_lower.starts_with("label:") {
            if line_lower.contains("gpt") {
                return Ok(PartitionTableType::Gpt);
            } else if line_lower.contains("dos") {
                return Ok(PartitionTableType::Dos);
            }
        }
    }

    Err(PartitionError::InvalidPartitionTable {
        device: device.to_path_buf(),
        reason: "Could not determine partition table type from sfdisk output".to_string(),
    })
}

/// Build partition map based on table type
fn build_partition_map(
    device: &RootDevice,
    table_type: PartitionTableType,
) -> HashMap<String, PathBuf> {
    let mut partitions = HashMap::new();

    // Common partitions (same number for both GPT and DOS)
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

    // Table-type specific partitions
    match table_type {
        PartitionTableType::Gpt => {
            partitions.insert(
                partition_names::FACTORY.to_string(),
                device.partition_path(PARTITION_NUM_FACTORY_GPT),
            );
            partitions.insert(
                partition_names::CERT.to_string(),
                device.partition_path(PARTITION_NUM_CERT_GPT),
            );
            partitions.insert(
                partition_names::ETC.to_string(),
                device.partition_path(PARTITION_NUM_ETC_GPT),
            );
            partitions.insert(
                partition_names::DATA.to_string(),
                device.partition_path(PARTITION_NUM_DATA_GPT),
            );
        }
        PartitionTableType::Dos => {
            // DOS has an extended partition container
            partitions.insert(
                partition_names::EXTENDED.to_string(),
                device.partition_path(PARTITION_NUM_EXTENDED_DOS),
            );
            partitions.insert(
                partition_names::FACTORY.to_string(),
                device.partition_path(PARTITION_NUM_FACTORY_DOS),
            );
            partitions.insert(
                partition_names::CERT.to_string(),
                device.partition_path(PARTITION_NUM_CERT_DOS),
            );
            partitions.insert(
                partition_names::ETC.to_string(),
                device.partition_path(PARTITION_NUM_ETC_DOS),
            );
            partitions.insert(
                partition_names::DATA.to_string(),
                device.partition_path(PARTITION_NUM_DATA_DOS),
            );
        }
    }

    partitions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_device_sda() -> RootDevice {
        RootDevice {
            path: PathBuf::from("/dev/sda"),
            name: "sda".to_string(),
            partition_sep: "".to_string(),
            root_partition: 2,
        }
    }

    fn create_test_device_nvme() -> RootDevice {
        RootDevice {
            path: PathBuf::from("/dev/nvme0n1"),
            name: "nvme0n1".to_string(),
            partition_sep: "p".to_string(),
            root_partition: 2,
        }
    }

    fn create_test_device_mmc() -> RootDevice {
        RootDevice {
            path: PathBuf::from("/dev/mmcblk0"),
            name: "mmcblk0".to_string(),
            partition_sep: "p".to_string(),
            root_partition: 3, // rootB
        }
    }

    #[test]
    fn test_partition_map_gpt_sata() {
        let device = create_test_device_sda();
        let map = build_partition_map(&device, PartitionTableType::Gpt);

        assert_eq!(map.get(partition_names::BOOT), Some(&PathBuf::from("/dev/sda1")));
        assert_eq!(map.get(partition_names::ROOT_A), Some(&PathBuf::from("/dev/sda2")));
        assert_eq!(map.get(partition_names::ROOT_B), Some(&PathBuf::from("/dev/sda3")));
        assert_eq!(map.get(partition_names::FACTORY), Some(&PathBuf::from("/dev/sda4")));
        assert_eq!(map.get(partition_names::CERT), Some(&PathBuf::from("/dev/sda5")));
        assert_eq!(map.get(partition_names::ETC), Some(&PathBuf::from("/dev/sda6")));
        assert_eq!(map.get(partition_names::DATA), Some(&PathBuf::from("/dev/sda7")));
        assert_eq!(map.get(partition_names::EXTENDED), None); // No extended partition in GPT
    }

    #[test]
    fn test_partition_map_dos_sata() {
        let device = create_test_device_sda();
        let map = build_partition_map(&device, PartitionTableType::Dos);

        assert_eq!(map.get(partition_names::BOOT), Some(&PathBuf::from("/dev/sda1")));
        assert_eq!(map.get(partition_names::ROOT_A), Some(&PathBuf::from("/dev/sda2")));
        assert_eq!(map.get(partition_names::ROOT_B), Some(&PathBuf::from("/dev/sda3")));
        assert_eq!(map.get(partition_names::EXTENDED), Some(&PathBuf::from("/dev/sda4")));
        assert_eq!(map.get(partition_names::FACTORY), Some(&PathBuf::from("/dev/sda5")));
        assert_eq!(map.get(partition_names::CERT), Some(&PathBuf::from("/dev/sda6")));
        assert_eq!(map.get(partition_names::ETC), Some(&PathBuf::from("/dev/sda7")));
        assert_eq!(map.get(partition_names::DATA), Some(&PathBuf::from("/dev/sda8")));
    }

    #[test]
    fn test_partition_map_gpt_nvme() {
        let device = create_test_device_nvme();
        let map = build_partition_map(&device, PartitionTableType::Gpt);

        assert_eq!(map.get(partition_names::BOOT), Some(&PathBuf::from("/dev/nvme0n1p1")));
        assert_eq!(map.get(partition_names::ROOT_A), Some(&PathBuf::from("/dev/nvme0n1p2")));
        assert_eq!(map.get(partition_names::DATA), Some(&PathBuf::from("/dev/nvme0n1p7")));
    }

    #[test]
    fn test_partition_map_dos_mmc() {
        let device = create_test_device_mmc();
        let map = build_partition_map(&device, PartitionTableType::Dos);

        assert_eq!(map.get(partition_names::BOOT), Some(&PathBuf::from("/dev/mmcblk0p1")));
        assert_eq!(map.get(partition_names::ROOT_A), Some(&PathBuf::from("/dev/mmcblk0p2")));
        assert_eq!(map.get(partition_names::ROOT_B), Some(&PathBuf::from("/dev/mmcblk0p3")));
        assert_eq!(map.get(partition_names::DATA), Some(&PathBuf::from("/dev/mmcblk0p8")));
    }

    #[test]
    fn test_partition_table_type_display() {
        assert_eq!(PartitionTableType::Gpt.to_string(), "GPT");
        assert_eq!(PartitionTableType::Dos.to_string(), "DOS/MBR");
    }

    #[test]
    fn test_root_current_root_a() {
        let device = create_test_device_sda(); // root_partition = 2 (rootA)
        let layout = PartitionLayout {
            table_type: PartitionTableType::Gpt,
            partitions: build_partition_map(&device, PartitionTableType::Gpt),
            device,
        };

        assert_eq!(layout.root_current(), PathBuf::from("/dev/sda2"));
    }

    #[test]
    fn test_root_current_root_b() {
        let device = create_test_device_mmc(); // root_partition = 3 (rootB)
        let layout = PartitionLayout {
            table_type: PartitionTableType::Dos,
            partitions: build_partition_map(&device, PartitionTableType::Dos),
            device,
        };

        assert_eq!(layout.root_current(), PathBuf::from("/dev/mmcblk0p3"));
    }
}
