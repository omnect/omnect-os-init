//! Integration tests for boot device detection.
//!
//! Tests the pure pipelines extracted from `device_from_fsuuid`
//! with realistic fixture strings, covering all storage types and boot paths
//! used in omnect-os.

use std::path::PathBuf;

use omnect_os_init::partition::layout::PartitionLayout;
use omnect_os_init::partition::{RootDevice, partition_names};

#[cfg(feature = "uboot")]
use omnect_os_init::partition::parse_device_path;
#[cfg(feature = "grub")]
use omnect_os_init::partition::root_device_from_blkid;

#[cfg(feature = "gpt")]
use omnect_os_init::config::CmdlineConfig;

// ---------------------------------------------------------------------------
// Fixture strings
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 6a: GRUB pipeline — root_device_from_blkid
// ---------------------------------------------------------------------------

#[cfg(feature = "grub")]
#[test]
fn test_grub_sata_root_a() {
    let rd = root_device_from_blkid("/dev/sda1", 2).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/sda"));
    assert_eq!(rd.partition_sep, "");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/sda2"));
}

#[cfg(feature = "grub")]
#[test]
fn test_grub_sata_root_b() {
    let rd = root_device_from_blkid("/dev/sda1", 3).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/sda"));
    assert_eq!(rd.partition_sep, "");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/sda3"));
}

#[cfg(feature = "grub")]
#[test]
fn test_grub_nvme_root_a() {
    let rd = root_device_from_blkid("/dev/nvme0n1p1", 2).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/nvme0n1"));
    assert_eq!(rd.partition_sep, "p");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/nvme0n1p2"));
}

#[cfg(feature = "grub")]
#[test]
fn test_grub_nvme_root_b() {
    let rd = root_device_from_blkid("/dev/nvme0n1p1", 3).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/nvme0n1"));
    assert_eq!(rd.partition_sep, "p");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/nvme0n1p3"));
}

#[cfg(feature = "grub")]
#[test]
fn test_grub_nvme_multi_namespace() {
    let rd = root_device_from_blkid("/dev/nvme1n2p1", 2).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/nvme1n2"));
    assert_eq!(rd.partition_sep, "p");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/nvme1n2p2"));
}

#[cfg(feature = "grub")]
#[test]
fn test_grub_emmc() {
    let rd = root_device_from_blkid("/dev/mmcblk0p1", 2).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/mmcblk0"));
    assert_eq!(rd.partition_sep, "p");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/mmcblk0p2"));
}

#[cfg(feature = "grub")]
#[test]
fn test_grub_sd_second_slot() {
    let rd = root_device_from_blkid("/dev/mmcblk1p1", 2).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/mmcblk1"));
    assert_eq!(rd.partition_sep, "p");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/mmcblk1p2"));
}

#[cfg(feature = "grub")]
#[test]
fn test_grub_usb_sata_naming() {
    let rd = root_device_from_blkid("/dev/sdb1", 2).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/sdb"));
    assert_eq!(rd.partition_sep, "");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/sdb2"));
}

#[cfg(feature = "grub")]
#[test]
fn test_grub_virtio() {
    let rd = root_device_from_blkid("/dev/vda1", 2).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/vda"));
    assert_eq!(rd.partition_sep, "");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/vda2"));
}

// ---------------------------------------------------------------------------
// 6b: U-Boot pipeline — device_from_path
//
// The U-Boot path uses device_from_path which takes the full root= device path.
// ---------------------------------------------------------------------------

#[cfg(feature = "uboot")]
#[test]
fn test_uboot_emmc_root_a() {
    let rd = parse_device_path("/dev/mmcblk0p2").unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/mmcblk0"));
    assert_eq!(rd.partition_sep, "p");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/mmcblk0p2"));
}

#[cfg(feature = "uboot")]
#[test]
fn test_uboot_emmc_root_b() {
    let rd = parse_device_path("/dev/mmcblk0p3").unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/mmcblk0"));
    assert_eq!(rd.partition_sep, "p");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/mmcblk0p3"));
}

#[cfg(feature = "uboot")]
#[test]
fn test_uboot_sd_root_b() {
    let rd = parse_device_path("/dev/mmcblk1p3").unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/mmcblk1"));
    assert_eq!(rd.partition_sep, "p");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/mmcblk1p3"));
}

// ---------------------------------------------------------------------------
// 6c: Full pipeline — cmdline → root_device_from_blkid → PartitionLayout
// ---------------------------------------------------------------------------

fn build_layout(rd: RootDevice) -> PartitionLayout {
    PartitionLayout::new(rd).unwrap()
}

#[cfg(feature = "gpt")]
#[test]
fn test_full_pipeline_x86_sata_grub_root_a() {
    let cmdline = "rootpart=2 bootpart_fsuuid=ABCD-1234 ro quiet";
    let part_num: u32 = CmdlineConfig::parse(cmdline)
        .get("rootpart")
        .unwrap()
        .parse()
        .unwrap();

    let rd = root_device_from_blkid("/dev/sda1", part_num).unwrap();
    let layout = build_layout(rd);

    assert_eq!(layout.device.base, PathBuf::from("/dev/sda"));
    assert_eq!(
        layout.partitions.get(partition_names::DATA),
        Some(&PathBuf::from("/dev/sda7"))
    );
    assert_eq!(layout.root_current(), PathBuf::from("/dev/sda2"));
}

#[cfg(feature = "gpt")]
#[test]
fn test_full_pipeline_x86_nvme_grub_root_b() {
    let cmdline = "rootpart=3 bootpart_fsuuid=ABCD-1234 ro quiet";
    let part_num: u32 = CmdlineConfig::parse(cmdline)
        .get("rootpart")
        .unwrap()
        .parse()
        .unwrap();

    let rd = root_device_from_blkid("/dev/nvme0n1p1", part_num).unwrap();
    let layout = build_layout(rd);

    assert_eq!(layout.device.base, PathBuf::from("/dev/nvme0n1"));
    assert_eq!(layout.root_current(), PathBuf::from("/dev/nvme0n1p3"));
    assert_eq!(
        layout.partitions.get(partition_names::DATA),
        Some(&PathBuf::from("/dev/nvme0n1p7"))
    );
}

#[cfg(feature = "dos")]
#[test]
fn test_full_pipeline_arm_emmc_uboot_root_a() {
    let rd = parse_device_path("/dev/mmcblk0p2").unwrap();
    let layout = build_layout(rd);

    assert_eq!(layout.device.base, PathBuf::from("/dev/mmcblk0"));
    assert_eq!(layout.root_current(), PathBuf::from("/dev/mmcblk0p2"));
    assert_eq!(
        layout.partitions.get(partition_names::DATA),
        Some(&PathBuf::from("/dev/mmcblk0p8"))
    );
}

#[cfg(feature = "dos")]
#[test]
fn test_full_pipeline_arm_sd_uboot_root_b() {
    let rd = parse_device_path("/dev/mmcblk1p3").unwrap();
    let layout = build_layout(rd);

    assert_eq!(layout.device.base, PathBuf::from("/dev/mmcblk1"));
    assert_eq!(layout.root_current(), PathBuf::from("/dev/mmcblk1p3"));
}

#[cfg(feature = "gpt")]
#[test]
fn test_full_pipeline_x86_virtio_grub() {
    let cmdline = "rootpart=2 bootpart_fsuuid=ABCD-1234 ro quiet";
    let part_num: u32 = CmdlineConfig::parse(cmdline)
        .get("rootpart")
        .unwrap()
        .parse()
        .unwrap();

    let rd = root_device_from_blkid("/dev/vda1", part_num).unwrap();
    let layout = build_layout(rd);

    assert_eq!(layout.device.base, PathBuf::from("/dev/vda"));
    assert_eq!(
        layout.partitions.get(partition_names::DATA),
        Some(&PathBuf::from("/dev/vda7"))
    );
}
