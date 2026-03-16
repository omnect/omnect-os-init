//! Root device detection from kernel command line and sysfs.
//!
//! Supports two omnect-os boot paths depending on the bootloader:
//!
//! - **GRUB** (`rootpart=N`): bare partition number (e.g. `rootpart=2`). The base
//!   block device is found by enumerating `/sys/block/`. If `omnect_rootblk=<base>`
//!   is cached in the cmdline from a previous boot, the sysfs probe is skipped.
//!   When USB storage is detected in sysfs but no removable block device has appeared
//!   yet, the probe waits up to 30s for USB enumeration to complete before probing.
//!
//! - **U-Boot** (`root=/dev/<device>`): full device path set by U-Boot bootargs
//!   (e.g. `root=/dev/mmcblk1p2`). Base device and separator are derived from the path.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::partition::{PartitionError, Result};

const DEVICE_WAIT_TIMEOUT_SECS: u64 = 30;
const DEVICE_POLL_INTERVAL_MS: u64 = 100;
/// Per-candidate probe timeout when enumerating /sys/block/
const PROBE_TIMEOUT_SECS: u64 = 2;

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

    // omnect-os: rootpart=N (bare partition number, e.g. "2")
    if let Some(part_str) = parse_cmdline_param(&cmdline, "rootpart")? {
        let part_num: u32 = part_str.parse().map_err(|_| {
            PartitionError::DeviceDetection(format!(
                "rootpart= is not a valid partition number: {}",
                part_str
            ))
        })?;

        // Fast path: bootloader cached the base device from a previous boot.
        if let Some(hint) = parse_cmdline_param(&cmdline, "omnect_rootblk")? {
            return device_from_hint(&hint, part_num);
        }

        // Generic probe: enumerate /sys/block/ — no hardcoded device names.
        return probe_sysblock(part_num);
    }

    // U-Boot path: root=/dev/<device> (full partition path in bootargs)
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

/// Builds a `RootDevice` from a cached `omnect_rootblk` hint and partition number.
fn device_from_hint(hint: &str, part_num: u32) -> Result<RootDevice> {
    let base = PathBuf::from(hint);
    let sep = partition_sep_for(&base);
    let root_partition = PathBuf::from(format!("{}{}{}", hint, sep, part_num));
    wait_for_device(&root_partition)?;
    log::info!("root device from omnect_rootblk hint: {}", base.display());
    Ok(RootDevice {
        base,
        partition_sep: sep,
        root_partition,
    })
}

/// Enumerates `/sys/block/` to find which real block device has partition `part_num`.
///
/// Filters virtual devices by requiring `/sys/block/<name>/device` to exist —
/// only real hardware (SATA, NVMe, MMC, virtio, USB) has this sysfs entry.
/// Works in initramfs before any block device is mounted because sysfs is
/// populated by the kernel's device driver layer, independent of mount state.
///
/// If USB storage is present in sysfs but no removable block device has appeared
/// yet, waits up to 30s for USB enumeration to complete before probing. This avoids
/// missing a USB boot device that is still being enumerated by the kernel.
///
/// When multiple disks have the same partition number (e.g. USB + internal NVMe),
/// removable devices (USB) are sorted first. This matches the intended boot priority:
/// a removable USB drive is always the intended boot source when present.
fn probe_sysblock(part_num: u32) -> Result<RootDevice> {
    log::debug!("probe_sysblock: searching for partition {}", part_num);

    // If USB storage is present but its block device hasn't appeared in /sys/block/
    // yet, wait for enumeration to complete before probing. This handles the race
    // between kernel USB enumeration and the initramfs boot sequence.
    if usb_storage_present() {
        log::info!("probe_sysblock: USB storage detected, waiting for block device to appear");
        wait_for_removable_block_device(Duration::from_secs(DEVICE_WAIT_TIMEOUT_SECS));
    }

    let mut matches: Vec<(RootDevice, bool)> = fs::read_dir("/sys/block")
        .map_err(|e| PartitionError::DeviceDetection(format!("failed to read /sys/block: {}", e)))?
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let has_device = Path::new(&format!("/sys/block/{}/device", name)).exists();
            // Only real hardware devices have a /device symlink
            if !has_device {
                log::debug!(
                    "probe_sysblock: skipping {} (no /device symlink, virtual)",
                    name
                );
            }
            has_device
        })
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let removable = is_removable(&name);
            let base = PathBuf::from(format!("/dev/{}", name));
            let sep = partition_sep_for(&base);
            let candidate = PathBuf::from(format!("/dev/{}{}{}", name, sep, part_num));
            log::info!(
                "probe_sysblock: probing {} (removable={})",
                candidate.display(),
                removable
            );
            match wait_for_device_timeout(&candidate, Duration::from_secs(PROBE_TIMEOUT_SECS)) {
                Ok(()) => {
                    log::info!(
                        "probe_sysblock: found {} (removable={})",
                        candidate.display(),
                        removable
                    );
                    Some((
                        RootDevice {
                            base,
                            partition_sep: sep,
                            root_partition: candidate,
                        },
                        removable,
                    ))
                }
                Err(_) => {
                    log::debug!(
                        "probe_sysblock: {} did not appear within {}s, skipping",
                        candidate.display(),
                        PROBE_TIMEOUT_SECS
                    );
                    None
                }
            }
        })
        .collect();

    // Removable devices (USB) sort before internal disks (NVMe, SATA, MMC).
    // On USB boot, this ensures the USB drive is preferred over an internal disk
    // that happens to have the same partition number.
    matches.sort_by_key(|(_, removable)| !removable);
    log::debug!(
        "probe_sysblock: candidates after sort: [{}]",
        matches
            .iter()
            .map(|(d, r)| format!("{} (removable={})", d.base.display(), r))
            .collect::<Vec<_>>()
            .join(", ")
    );

    match matches.len() {
        0 => Err(PartitionError::DeviceDetection(format!(
            "no block device found with partition {}",
            part_num
        ))),
        1 => {
            log::info!(
                "probe_sysblock: selected {} (only match)",
                matches[0].0.base.display()
            );
            Ok(matches.remove(0).0)
        }
        _ => {
            // Multiple real disks have this partition number (e.g. during flash-mode).
            // Flash-mode source/target disambiguation is handled in a separate PR.
            let names: Vec<_> = matches
                .iter()
                .map(|(d, removable)| format!("{} (removable={})", d.base.display(), removable))
                .collect();
            log::warn!(
                "probe_sysblock: multiple matches [{}]; selecting first after removable-sort",
                names.join(", ")
            );
            Ok(matches.remove(0).0)
        }
    }
}

/// Returns true if the block device is removable (e.g. USB drive).
///
/// Reads `/sys/block/<name>/removable` — kernel sets this to "1" for removable
/// media (USB, SD card via external reader) and "0" for internal disks (NVMe, SATA, MMC).
fn is_removable(name: &str) -> bool {
    fs::read_to_string(format!("/sys/block/{}/removable", name))
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

/// Returns true if a USB mass storage device is present — either already bound to the
/// `usb-storage` driver or still in the process of being bound (Mass Storage interface class).
///
/// Checks two sysfs paths:
/// - `/sys/bus/usb/drivers/usb-storage/`: symlinks appear here when binding is complete.
/// - `/sys/bus/usb/devices/*/bInterfaceClass == "08"`: appears earlier, during binding,
///   covering the race window between USB enumeration and driver attachment.
fn usb_storage_present() -> bool {
    // Fast path: driver already bound
    if fs::read_dir("/sys/bus/usb/drivers/usb-storage")
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
    {
        return true;
    }

    // Cover the race window: interface enumerated but driver not yet bound
    fs::read_dir("/sys/bus/usb/devices")
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            fs::read_to_string(e.path().join("bInterfaceClass"))
                .map(|c| c.trim() == "08")
                .unwrap_or(false)
        })
}

/// Blocks until at least one removable real block device appears in `/sys/block/`,
/// or until the timeout expires. Called when USB storage is detected but its block
/// device node has not yet appeared — gives the kernel time to finish enumeration.
fn wait_for_removable_block_device(timeout: Duration) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let found = fs::read_dir("/sys/block")
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                Path::new(&format!("/sys/block/{}/device", name)).exists() && is_removable(&name)
            });
        if found {
            log::info!(
                "probe_sysblock: removable block device appeared after {:.1}s",
                start.elapsed().as_secs_f32()
            );
            return;
        }
        thread::sleep(Duration::from_millis(DEVICE_POLL_INTERVAL_MS));
    }
    log::warn!(
        "probe_sysblock: no removable block device appeared within {}s, proceeding anyway",
        timeout.as_secs()
    );
}

/// Returns the partition separator for a block device: `"p"` for NVMe/MMC, `""` for others.
fn partition_sep_for(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name.contains("nvme") || name.starts_with("mmcblk") {
        "p".to_string()
    } else {
        String::new()
    }
}

/// Builds a `RootDevice` from a full `root=/dev/<device>` path (U-Boot boot path).
///
/// Derives the base device and partition separator from the device name.
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

fn wait_for_device(device: &Path) -> Result<()> {
    wait_for_device_timeout(device, Duration::from_secs(DEVICE_WAIT_TIMEOUT_SECS))
}

fn wait_for_device_timeout(device: &Path, timeout: Duration) -> Result<()> {
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
/// Handles both `key=value` and `key="value with spaces"` formats.
pub(crate) fn parse_cmdline_param(cmdline: &str, key: &str) -> Result<Option<String>> {
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
    fn test_parse_cmdline_param_rootpart() {
        let cmdline = "rootpart=2 console=ttyS0,115200 quiet";
        assert_eq!(
            parse_cmdline_param(cmdline, "rootpart").unwrap(),
            Some("2".to_string())
        );
        assert_eq!(
            parse_cmdline_param(cmdline, "omnect_rootblk").unwrap(),
            None
        );
    }

    #[test]
    fn test_parse_cmdline_param_missing() {
        let cmdline = "ro quiet";
        assert_eq!(parse_cmdline_param(cmdline, "rootpart").unwrap(), None);
        assert_eq!(
            parse_cmdline_param(cmdline, "omnect_rootblk").unwrap(),
            None
        );
    }

    #[test]
    fn test_parse_cmdline_param_complex() {
        let cmdline = "rootpart=2 coherent_pool=1M console=ttyS0,115200 \
                       omnect_rootblk=/dev/sda omnect_release_image=1";
        assert_eq!(
            parse_cmdline_param(cmdline, "rootpart").unwrap(),
            Some("2".to_string())
        );
        assert_eq!(
            parse_cmdline_param(cmdline, "omnect_rootblk").unwrap(),
            Some("/dev/sda".to_string())
        );
    }

    #[test]
    fn test_parse_cmdline_omnect_rootblk() {
        let cmdline = "rootpart=2 omnect_rootblk=/dev/sda ro";
        assert_eq!(
            parse_cmdline_param(cmdline, "omnect_rootblk").unwrap(),
            Some("/dev/sda".to_string())
        );
    }

    #[test]
    fn test_partition_sep_for_nvme() {
        assert_eq!(partition_sep_for(Path::new("/dev/nvme0n1")), "p");
    }

    #[test]
    fn test_partition_sep_for_mmc() {
        assert_eq!(partition_sep_for(Path::new("/dev/mmcblk0")), "p");
    }

    #[test]
    fn test_partition_sep_for_sata() {
        assert_eq!(partition_sep_for(Path::new("/dev/sda")), "");
    }

    #[test]
    fn test_partition_sep_for_virtio() {
        assert_eq!(partition_sep_for(Path::new("/dev/vda")), "");
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
    fn test_root_device_partition_path_virtio() {
        let device = RootDevice {
            base: PathBuf::from("/dev/vda"),
            partition_sep: String::new(),
            root_partition: PathBuf::from("/dev/vda2"),
        };
        assert_eq!(device.partition_path(1), PathBuf::from("/dev/vda1"));
        assert_eq!(device.partition_path(7), PathBuf::from("/dev/vda7"));
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
}
