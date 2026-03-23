//! Integration tests for boot device detection.
//!
//! Tests the pure pipelines extracted from `device_from_fsuuid` and
//! `detect_partition_table_type` with realistic fixture strings, covering all
//! storage types and boot paths used in omnect-os.

use std::path::PathBuf;

use omnect_os_init::partition::device::parse_cmdline_param;
use omnect_os_init::partition::layout::PartitionLayout;
use omnect_os_init::partition::{
    RootDevice, parse_sfdisk_output, partition_names, root_device_from_blkid,
};

// ---------------------------------------------------------------------------
// Fixture strings
// ---------------------------------------------------------------------------

const SFDISK_GPT: &str = "\
Disk /dev/sda: 30 GiB, 32212254720 bytes, 62914560 sectors
Disk model: QEMU HARDDISK
Units: sectors of 1 * 512 = 512 bytes
Sector size (logical/physical): 512 bytes / 512 bytes
Disklabel type: gpt
Disk identifier: 11111111-2222-3333-4444-555555555555";

const SFDISK_DOS: &str = "\
Disk /dev/mmcblk0: 7.28 GiB, 7818182656 bytes, 15269888 sectors
Units: sectors of 1 * 512 = 512 bytes
Sector size (logical/physical): 512 bytes / 512 bytes
Disklabel type: dos
Disk identifier: 0xdeadbeef";

// ---------------------------------------------------------------------------
// 6a: GRUB pipeline — root_device_from_blkid
// ---------------------------------------------------------------------------

#[test]
fn test_grub_sata_root_a() {
    let rd = root_device_from_blkid("/dev/sda1", 2).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/sda"));
    assert_eq!(rd.partition_sep, "");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/sda2"));
}

#[test]
fn test_grub_sata_root_b() {
    let rd = root_device_from_blkid("/dev/sda1", 3).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/sda"));
    assert_eq!(rd.partition_sep, "");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/sda3"));
}

#[test]
fn test_grub_nvme_root_a() {
    let rd = root_device_from_blkid("/dev/nvme0n1p1", 2).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/nvme0n1"));
    assert_eq!(rd.partition_sep, "p");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/nvme0n1p2"));
}

#[test]
fn test_grub_nvme_root_b() {
    let rd = root_device_from_blkid("/dev/nvme0n1p1", 3).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/nvme0n1"));
    assert_eq!(rd.partition_sep, "p");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/nvme0n1p3"));
}

#[test]
fn test_grub_nvme_multi_namespace() {
    let rd = root_device_from_blkid("/dev/nvme1n2p1", 2).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/nvme1n2"));
    assert_eq!(rd.partition_sep, "p");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/nvme1n2p2"));
}

#[test]
fn test_grub_emmc() {
    let rd = root_device_from_blkid("/dev/mmcblk0p1", 2).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/mmcblk0"));
    assert_eq!(rd.partition_sep, "p");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/mmcblk0p2"));
}

#[test]
fn test_grub_sd_second_slot() {
    let rd = root_device_from_blkid("/dev/mmcblk1p1", 2).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/mmcblk1"));
    assert_eq!(rd.partition_sep, "p");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/mmcblk1p2"));
}

#[test]
fn test_grub_usb_sata_naming() {
    let rd = root_device_from_blkid("/dev/sdb1", 2).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/sdb"));
    assert_eq!(rd.partition_sep, "");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/sdb2"));
}

#[test]
fn test_grub_virtio() {
    let rd = root_device_from_blkid("/dev/vda1", 2).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/vda"));
    assert_eq!(rd.partition_sep, "");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/vda2"));
}

// ---------------------------------------------------------------------------
// 6b: U-Boot pipeline — split_partition_suffix via RootDevice construction
//
// The U-Boot path calls device_from_path which is not pub; test the same
// logic via root_device_from_blkid using the full root partition path as
// the boot_part_dev argument with part_num equal to the partition's own
// number (boot and root are the same call site in the pure function).
// ---------------------------------------------------------------------------

#[test]
fn test_uboot_emmc_root_a() {
    // U-Boot passes root=/dev/mmcblk0p2 — boot part is the same device.
    // We verify the suffix-splitting logic produces the right base + sep.
    let rd = root_device_from_blkid("/dev/mmcblk0p2", 2).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/mmcblk0"));
    assert_eq!(rd.partition_sep, "p");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/mmcblk0p2"));
}

#[test]
fn test_uboot_emmc_root_b() {
    let rd = root_device_from_blkid("/dev/mmcblk0p3", 3).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/mmcblk0"));
    assert_eq!(rd.partition_sep, "p");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/mmcblk0p3"));
}

#[test]
fn test_uboot_sd_root_b() {
    let rd = root_device_from_blkid("/dev/mmcblk1p3", 3).unwrap();
    assert_eq!(rd.base, PathBuf::from("/dev/mmcblk1"));
    assert_eq!(rd.partition_sep, "p");
    assert_eq!(rd.root_partition, PathBuf::from("/dev/mmcblk1p3"));
}

// ---------------------------------------------------------------------------
// 6c: Full pipeline — cmdline → root_device_from_blkid → parse_sfdisk_output
//     → PartitionLayout
// ---------------------------------------------------------------------------

fn build_layout(rd: RootDevice, sfdisk_out: &str) -> PartitionLayout {
    let table_type = parse_sfdisk_output(sfdisk_out, &rd.base).unwrap();
    PartitionLayout::detect_from_parts(rd, table_type)
}

#[test]
fn test_full_pipeline_x86_sata_grub_root_a() {
    let cmdline = "rootpart=2 bootpart_fsuuid=ABCD-1234 ro quiet";
    let part_num: u32 = parse_cmdline_param(cmdline, "rootpart")
        .unwrap()
        .unwrap()
        .parse()
        .unwrap();

    let rd = root_device_from_blkid("/dev/sda1", part_num).unwrap();
    let layout = build_layout(rd, SFDISK_GPT);

    assert_eq!(layout.device.base, PathBuf::from("/dev/sda"));
    assert_eq!(
        layout.partitions.get(partition_names::DATA),
        Some(&PathBuf::from("/dev/sda7"))
    );
    assert_eq!(layout.root_current(), PathBuf::from("/dev/sda2"));
}

#[test]
fn test_full_pipeline_x86_nvme_grub_root_b() {
    let cmdline = "rootpart=3 bootpart_fsuuid=ABCD-1234 ro quiet";
    let part_num: u32 = parse_cmdline_param(cmdline, "rootpart")
        .unwrap()
        .unwrap()
        .parse()
        .unwrap();

    let rd = root_device_from_blkid("/dev/nvme0n1p1", part_num).unwrap();
    let layout = build_layout(rd, SFDISK_GPT);

    assert_eq!(layout.device.base, PathBuf::from("/dev/nvme0n1"));
    assert_eq!(layout.root_current(), PathBuf::from("/dev/nvme0n1p3"));
    assert_eq!(
        layout.partitions.get(partition_names::DATA),
        Some(&PathBuf::from("/dev/nvme0n1p7"))
    );
}

#[test]
fn test_full_pipeline_arm_emmc_uboot_root_a() {
    let rd = root_device_from_blkid("/dev/mmcblk0p2", 2).unwrap();
    let layout = build_layout(rd, SFDISK_DOS);

    assert_eq!(layout.device.base, PathBuf::from("/dev/mmcblk0"));
    assert_eq!(layout.root_current(), PathBuf::from("/dev/mmcblk0p2"));
    assert_eq!(
        layout.partitions.get(partition_names::DATA),
        Some(&PathBuf::from("/dev/mmcblk0p8"))
    );
}

#[test]
fn test_full_pipeline_arm_sd_uboot_root_b() {
    let rd = root_device_from_blkid("/dev/mmcblk1p3", 3).unwrap();
    let layout = build_layout(rd, SFDISK_DOS);

    assert_eq!(layout.device.base, PathBuf::from("/dev/mmcblk1"));
    assert_eq!(layout.root_current(), PathBuf::from("/dev/mmcblk1p3"));
}

#[test]
fn test_full_pipeline_x86_virtio_grub() {
    let cmdline = "rootpart=2 bootpart_fsuuid=ABCD-1234 ro quiet";
    let part_num: u32 = parse_cmdline_param(cmdline, "rootpart")
        .unwrap()
        .unwrap()
        .parse()
        .unwrap();

    let rd = root_device_from_blkid("/dev/vda1", part_num).unwrap();
    let layout = build_layout(rd, SFDISK_GPT);

    assert_eq!(layout.device.base, PathBuf::from("/dev/vda"));
    assert_eq!(
        layout.partitions.get(partition_names::DATA),
        Some(&PathBuf::from("/dev/vda7"))
    );
}
