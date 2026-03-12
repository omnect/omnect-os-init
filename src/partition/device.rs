//! Root device detection from kernel command line parameters.
//!
//! Supports two cmdline formats:
//!
//! 1. **omnect format** (`rootpart=N`): omnect-os sets a bare partition number
//!    (e.g. `rootpart=2`). The base block device is discovered by probing common
//!    device paths, mirroring the bash `grub-sh`/`uboot-sh` logic.
//!
//! 2. **standard Linux format** (`root=/dev/<device>`): full device path
//!    (e.g. `root=/dev/mmcblk0p2`). Used as a fallback.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::partition::{PartitionError, Result};

const DEVICE_WAIT_TIMEOUT_SECS: u64 = 30;
const DEVICE_POLL_INTERVAL_MS: u64 = 100;

/// Candidate base block devices, searched in order (mirrors bash grub-sh).
/// NVMe uses the "p" partition separator; the others use none.
const SEARCH_ROOTBLK: &[(&str, &str)] = &[
    ("/dev/sdb", ""),
    ("/dev/sda", ""),
    ("/dev/nvme0n1", "p"),
    ("/dev/mmcblk0", "p"),
];

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
pub fn detect_root_device() -> Result<RootDevice> {
    detect_root_device_from_cmdline("/proc/cmdline")
}

/// Internal implementation with configurable cmdline path for testing.
pub(crate) fn detect_root_device_from_cmdline(cmdline_path: &str) -> Result<RootDevice> {
    let cmdline = fs::read_to_string(cmdline_path).map_err(|e| {
        PartitionError::DeviceDetection(format!("failed to read {}: {}", cmdline_path, e))
    })?;

    // omnect-os passes `rootpart=N` (a bare partition number, e.g. "2").
    // Try this first; fall back to the standard Linux `root=/dev/...` form.
    if let Some(part_num_str) = parse_cmdline_param(&cmdline, "rootpart")? {
        let part_num: u32 = part_num_str.parse().map_err(|_| {
            PartitionError::DeviceDetection(format!(
                "rootpart= value is not a valid partition number: {}",
                part_num_str
            ))
        })?;
        return detect_by_rootpart(&cmdline, part_num);
    }

    // Standard Linux `root=/dev/<device>` fallback.
    if let Some(root_param) = parse_cmdline_param(&cmdline, "root")? {
        if !root_param.starts_with("/dev/") {
            return Err(PartitionError::DeviceDetection(format!(
                "root= must be a device path starting with /dev/, got: {}",
                root_param
            )));
        }
        return detect_by_root_path(&cmdline, &root_param);
    }

    Err(PartitionError::DeviceDetection(
        "neither rootpart= nor root= found in kernel command line".into(),
    ))
}

/// Detects the root device from a bare partition number (`rootpart=N`).
///
/// Mirrors the bash `rootblk_dev_generate_dev_omnect` logic:
/// 1. If `omnect_rootblk=<base>` is set in cmdline, use that directly.
/// 2. Otherwise probe `SEARCH_ROOTBLK` candidates in order.
fn detect_by_rootpart(cmdline: &str, part_num: u32) -> Result<RootDevice> {
    // If the bootloader cached the base device from a previous boot, use it.
    if let Some(hint) = parse_cmdline_param(cmdline, "omnect_rootblk")? {
        let base = PathBuf::from(&hint);
        // Determine separator from cached base name
        let sep = if hint.ends_with('p') || hint.contains("nvme") || hint.contains("mmcblk") {
            // hint may or may not include the trailing 'p'; derive it properly
            derive_separator_from_base(&base)
        } else {
            String::new()
        };
        let root_partition = PathBuf::from(format!("{}{}{}", hint, sep, part_num));
        wait_for_device(&root_partition)?;
        log::info!("root device from omnect_rootblk hint: {}", base.display());
        return Ok(RootDevice {
            base,
            partition_sep: sep,
            root_partition,
        });
    }

    // No cached hint — probe candidates in order (mirrors bash search_rootblk array).
    for (base_str, sep) in SEARCH_ROOTBLK {
        let candidate = PathBuf::from(format!("{}{}{}", base_str, sep, part_num));
        log::info!("probing {}", candidate.display());

        // Wait up to 2 s per candidate (matches bash sleep 0.1 × 20 iterations).
        if wait_for_device_timeout(&candidate, Duration::from_secs(2)).is_ok() {
            log::info!("found root device base: {}", base_str);
            return Ok(RootDevice {
                base: PathBuf::from(base_str),
                partition_sep: sep.to_string(),
                root_partition: candidate,
            });
        }
    }

    Err(PartitionError::DeviceDetection(format!(
        "failed to find a block device with partition {}",
        part_num
    )))
}

/// Detects the root device from a full device path (`root=/dev/...`).
fn detect_by_root_path(cmdline: &str, root_param: &str) -> Result<RootDevice> {
    wait_for_device(&PathBuf::from(root_param))?;

    let partition_path = fs::canonicalize(root_param).map_err(|e| {
        PartitionError::DeviceDetection(format!("failed to canonicalize {}: {}", root_param, e))
    })?;

    let (base, partition_sep) = derive_base_device(&partition_path)?;

    if let Some(hint) = parse_cmdline_param(cmdline, "omnect_rootblk")? {
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

fn wait_for_device(device: &Path) -> Result<()> {
    wait_for_device_timeout(device, Duration::from_secs(DEVICE_WAIT_TIMEOUT_SECS))
}

fn wait_for_device_timeout(device: &Path, timeout: Duration) -> Result<()> {
    let poll_interval = Duration::from_millis(DEVICE_POLL_INTERVAL_MS);
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
        thread::sleep(poll_interval);
    }
}

/// Derives the partition separator ("p" or "") for a base device path.
/// NVMe and MMC devices use "p"; SATA/virtio do not.
fn derive_separator_from_base(base: &Path) -> String {
    let name = base
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();

    // nvme0n1 and mmcblk0 use the "p" separator
    if name.contains("nvme") || name.starts_with("mmcblk") {
        "p".to_string()
    } else {
        String::new()
    }
}

/// Parses a parameter value from kernel command line.
///
/// Handles both `key=value` and `key="value with spaces"` formats.
pub(crate) fn parse_cmdline_param(cmdline: &str, key: &str) -> Result<Option<String>> {
    let prefix = format!("{}=", key);

    for token in cmdline.split_whitespace() {
        if let Some(value) = token.strip_prefix(&prefix) {
            let value = value.trim_matches('"');
            return Ok(Some(value.to_string()));
        }
    }

    Ok(None)
}

/// Derives the base block device and partition separator from a full partition path.
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

    let parent = partition_path.parent().unwrap_or_else(|| Path::new("/dev"));

    // NVMe/MMC: nvme0n1p2, mmcblk0p2 - partition number after 'p'
    if let Some(pos) = partition_name.rfind('p') {
        let suffix = &partition_name[pos + 1..];
        if suffix.chars().all(|c| c.is_ascii_digit()) && !suffix.is_empty() {
            let base_name = &partition_name[..pos];
            if Path::new(&format!("/sys/block/{}", base_name)).exists() {
                return Ok((parent.join(base_name), "p".to_string()));
            }
        }
    }

    // SATA/virtio: sda2, vda2 - partition number appended directly
    let mut base_end = partition_name.len();
    while base_end > 0 && partition_name[..base_end].ends_with(|c: char| c.is_ascii_digit()) {
        base_end -= 1;
    }

    if base_end < partition_name.len() && base_end > 0 {
        let base_name = &partition_name[..base_end];
        if Path::new(&format!("/sys/block/{}", base_name)).exists() {
            return Ok((parent.join(base_name), String::new()));
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
    fn test_parse_cmdline_param_rootpart() {
        let cmdline = "rootpart=2 console=ttyS0,115200 quiet";
        assert_eq!(
            parse_cmdline_param(cmdline, "rootpart").unwrap(),
            Some("2".to_string())
        );
        assert_eq!(parse_cmdline_param(cmdline, "root").unwrap(), None);
    }

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
        assert_eq!(parse_cmdline_param(cmdline, "rootpart").unwrap(), None);
    }

    #[test]
    fn test_parse_cmdline_param_complex() {
        let cmdline = "rootpart=2 coherent_pool=1M console=tty0 console=ttyS0,115200 \
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
    fn test_derive_separator_nvme() {
        assert_eq!(
            derive_separator_from_base(Path::new("/dev/nvme0n1")),
            "p"
        );
    }

    #[test]
    fn test_derive_separator_mmc() {
        assert_eq!(
            derive_separator_from_base(Path::new("/dev/mmcblk0")),
            "p"
        );
    }

    #[test]
    fn test_derive_separator_sata() {
        assert_eq!(
            derive_separator_from_base(Path::new("/dev/sda")),
            ""
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
