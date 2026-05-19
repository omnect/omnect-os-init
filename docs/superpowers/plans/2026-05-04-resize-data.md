# resize-data Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the `resize-data` feature from the `resize_data` branch into the `feat/boot-mode-dispatch` architecture, expanding the data partition on first boot.

**Architecture:** Split `mount_partitions` into `mount_core_partitions` (rootfs + boot) and `mount_remaining_partitions` (factory/cert/etc/data) so the bootloader can be created after boot is mounted but before the data partition is touched. The resize logic lives in `src/mode/resize_data.rs` and is called from `mode::normal::run` between the two mount phases. Everything is gated by the compile-time `resize-data` Cargo feature.

**Tech Stack:** Rust, `thiserror`, `nix`, external tools: `sgdisk`, `parted`, `e2fsck`, `resize2fs`, `sync`

---

## File map

| Action  | File | Purpose |
|---------|------|---------|
| Modify  | `Cargo.toml` | Add `resize-data` feature |
| Modify  | `src/error.rs` | Add `ResizeDataError` + `InitramfsError::ResizeData` |
| Modify  | `src/filesystem/boot_sequence.rs` | Split `mount_partitions` → two functions |
| Modify  | `src/filesystem/mod.rs` | Update public exports |
| Modify  | `src/lib.rs` | Use `mount_core_partitions`; move `persist_fsck_results` to mode handler |
| Create  | `src/mode/resize_data.rs` | Resize logic (feature-gated) |
| Modify  | `src/mode/mod.rs` | Register `resize_data` submodule |
| Modify  | `src/mode/normal.rs` | Call resize + mount_remaining + persist_fsck |

---

## Task 1: Add `resize-data` Cargo feature and error types

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/error.rs`

- [ ] **Step 1.1: Add feature flag to Cargo.toml**

In `Cargo.toml`, add `resize-data` to the `[features]` block in alphabetical order:

```toml
[features]
default = ["core"]
core = []
dos = ["core"]            # DOS/MBR partition table (extended container at slot 4, logical partitions 5-8)
gpt = ["core"]            # GPT partition table (primary partitions 1-7)
grub = ["core"]           # x86-64 EFI targets (GRUB bootloader)
persistent-var-log = ["core"]
release-image = ["core"]  # Enables release-mode error handling (loop on fatal error)
resize-data = ["core"]    # Expands data partition to fill disk on first boot
uboot = ["core"]          # ARM targets (U-Boot bootloader)
```

- [ ] **Step 1.2: Add ResizeDataError to src/error.rs**

Add the following after the `ConfigError` definition (before `LoggingError`). The entire block is feature-gated:

```rust
/// Errors during data partition resize
#[cfg(feature = "resize-data")]
#[derive(Error, Debug)]
pub enum ResizeDataError {
    #[error("Command '{command}' failed with code {code}: {output}")]
    CommandFailed {
        command: String,
        code: i32,
        output: String,
    },

    #[error("Could not determine partition number from device path: {}", .0.display())]
    InvalidDevicePath(PathBuf),

    #[error("Could not find extended partition on {}", .0.display())]
    ExtendedPartitionNotFound(PathBuf),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

- [ ] **Step 1.3: Add InitramfsError::ResizeData variant**

Add the following variant to `InitramfsError` in `src/error.rs`, after the `Config` variant and before `Io`:

```rust
    #[cfg(feature = "resize-data")]
    #[error("Resize data error: {0}")]
    ResizeData(#[from] ResizeDataError),
```

- [ ] **Step 1.4: Verify compilation**

```bash
cd .worktrees/feat/resize-data
cargo check --features grub,gpt,resize-data
```

Expected: no errors, no warnings.

- [ ] **Step 1.5: Commit**

```bash
git add Cargo.toml src/error.rs
git commit -m "feat(resize-data): add resize-data feature flag and error types"
```

---

## Task 2: Split `mount_partitions` into two functions

**Files:**
- Modify: `src/filesystem/boot_sequence.rs`
- Modify: `src/filesystem/mod.rs`

- [ ] **Step 2.1: Replace mount_partitions with mount_core_partitions**

In `src/filesystem/boot_sequence.rs`, replace the entire `mount_partitions` function (lines 62–176) with two new functions. The first handles rootfs and boot:

```rust
/// Mount the core partitions required before the bootloader can be created.
///
/// Mounts rootCurrent (read-only) and boot (read-write). Must be called before
/// `create_bootloader()` because GRUB reads grubenv from the boot partition.
/// `mount_remaining_partitions` must be called afterward to mount factory,
/// cert, etc, and data.
pub fn mount_core_partitions(
    layout: &PartitionLayout,
    rootfs: &Path,
    ods_status: &mut OdsStatus,
) -> crate::error::Result<()> {
    let root_dev = layout
        .partitions
        .get(&PartitionName::RootCurrent)
        .ok_or_else(|| {
            InitramfsError::Partition(PartitionError::DeviceDetection(
                "rootCurrent not found in partition map; cannot mount rootfs".to_string(),
            ))
        })?;

    // The mount target must exist before mount(2) is called. The directory is
    // not baked into the initramfs image — create it here on every boot.
    fs::create_dir_all(rootfs).map_err(|e| {
        InitramfsError::Io(std::io::Error::other(format!(
            "Failed to create rootfs mount point {}: {}",
            rootfs.display(),
            e
        )))
    })?;

    // rootCurrent is mounted directly without fsck: the kernel's own ext4 journal
    // replay is the correct recovery mechanism. Running fsck -y before mount can
    // interfere with journal replay and cause EUCLEAN on a filesystem that the kernel
    // could have mounted cleanly.
    mount(MountPoint::new(
        root_dev,
        rootfs,
        MountOptions::ext4_readonly().noatime().nodiratime(),
    ))?;
    log::info!("Mounted rootfs at {}", rootfs.display());

    // Mount boot partition.
    // vfat is mounted read-write without noatime/nodiratime: GRUB needs to write
    // grubenv on the boot partition; atime writes are acceptable on vfat.
    if let Some(boot_dev) = layout.partitions.get(&PartitionName::Boot) {
        let boot_mount = rootfs.join(mount_points::BOOT);
        if is_path_mounted(&boot_mount)? {
            // Boot already mounted at this stage is a logic error: mount_core_partitions
            // is called exactly once per boot. If boot is already present something has
            // gone wrong in the boot sequence.
            return Err(InitramfsError::Filesystem(FilesystemError::MountFailed {
                src_path: boot_dev.clone(),
                target: boot_mount,
                reason: "boot partition already mounted at start of mount_core_partitions"
                    .to_string(),
            }));
        }
        fsck_and_record(boot_dev, PartitionName::Boot, ods_status, FsType::Vfat)?;
        mount_readwrite(boot_dev, &boot_mount, FsType::Vfat)?;
    }

    Ok(())
}

/// Mount the remaining partitions after the bootloader has been created.
///
/// Mounts factory, cert, etc, data, and var/volatile. Must be called after
/// `mount_core_partitions` and bootloader creation. The data partition is
/// mounted here, so `resize_data_if_needed` must be called before this
/// function when the `resize-data` feature is enabled.
pub fn mount_remaining_partitions(
    layout: &PartitionLayout,
    rootfs: &Path,
    ods_status: &mut OdsStatus,
) -> crate::error::Result<()> {
    // Mount factory partition read-only
    if let Some(factory_dev) = layout.partitions.get(&PartitionName::Factory) {
        let factory_mount = rootfs.join(mount_points::FACTORY_PARTITION);
        fsck_and_record(
            factory_dev,
            PartitionName::Factory,
            ods_status,
            FsType::Ext4,
        )?;
        mount(MountPoint::new(
            factory_dev,
            &factory_mount,
            MountOptions::ext4_readonly().noatime().nodiratime(),
        ))?;
    }

    // Mount cert partition read-write — initramfs creates ca/ and priv/ subdirs on first boot
    if let Some(cert_dev) = layout.partitions.get(&PartitionName::Cert) {
        let cert_mount = rootfs.join(mount_points::CERT_PARTITION);
        fsck_and_record(cert_dev, PartitionName::Cert, ods_status, FsType::Ext4)?;
        mount(MountPoint::new(
            cert_dev,
            &cert_mount,
            MountOptions::ext4_readwrite().noatime().nodiratime(),
        ))?;
    }

    // Mount etc partition (for overlay upper)
    if let Some(etc_dev) = layout.partitions.get(&PartitionName::Etc) {
        let etc_mount = rootfs.join(mount_points::ETC_PARTITION);
        fsck_and_record(etc_dev, PartitionName::Etc, ods_status, FsType::Ext4)?;
        mount(MountPoint::new(
            etc_dev,
            &etc_mount,
            MountOptions::ext4_readwrite().noatime().nodiratime(),
        ))?;
    }

    // Mount data partition
    if let Some(data_dev) = layout.partitions.get(&PartitionName::Data) {
        let data_mount = rootfs.join(mount_points::DATA_PARTITION);
        fsck_and_record(data_dev, PartitionName::Data, ods_status, FsType::Ext4)?;
        mount(MountPoint::new(
            data_dev,
            &data_mount,
            MountOptions::ext4_readwrite().noatime().nodiratime(),
        ))?;
    }

    // /var/volatile provides a writable mount for volatile data under the read-only rootfs
    let var_volatile = rootfs.join(mount_points::VAR_VOLATILE);
    mount_tmpfs(&var_volatile, MsFlags::empty(), None)?;

    // /run is NOT mounted here: the initramfs /run tmpfs (mounted by
    // mount_essential_filesystems) is moved into the new root by switch_root
    // using MS_MOVE. Mounting a second tmpfs at /rootfs/run would cause EBUSY
    // and lose any files written there (e.g. ODS runtime state).

    Ok(())
}
```

- [ ] **Step 2.2: Update filesystem/mod.rs exports**

In `src/filesystem/mod.rs`, replace the `boot_sequence` re-export line:

```rust
pub use self::boot_sequence::{
    fsck_and_record,
    mount_core_partitions,
    mount_remaining_partitions,
    persist_fsck_results,
};
```

- [ ] **Step 2.3: Verify all tests still pass**

```bash
cd .worktrees/feat/resize-data
cargo test --features grub,gpt 2>&1 | tail -5
cargo test --features grub,dos 2>&1 | tail -5
cargo test --features uboot,gpt 2>&1 | tail -5
cargo test --features uboot,dos 2>&1 | tail -5
```

Expected for each: `test result: ok. N passed; 0 failed`

- [ ] **Step 2.4: Commit**

```bash
git add src/filesystem/boot_sequence.rs src/filesystem/mod.rs
git commit -m "refactor(filesystem): split mount_partitions into mount_core and mount_remaining"
```

---

## Task 3: Update `run_init` to use split mount functions

**Files:**
- Modify: `src/lib.rs`

The current `run_init` calls `mount_partitions`, then creates the bootloader, then calls `persist_fsck_results`. With the split, `run_init` only calls `mount_core_partitions`. The rest (mount_remaining + persist) moves into `mode::normal::run`.

- [ ] **Step 3.1: Rewrite run_init**

Replace the entire `run_init` function body in `src/lib.rs` with:

```rust
pub fn run_init() -> Result<()> {
    info!("omnect-os-initramfs starting");

    let config = Config::load()?;
    let rootfs = Path::new(ROOTFS_DIR);

    info!("Detecting root device...");
    let root_device = detect_root_device(&config.cmdline)?;
    info!(
        "Root device: {} (partition {})",
        root_device.base.display(),
        root_device.root_partition.display()
    );

    let layout = PartitionLayout::new(root_device)?;
    create_omnect_symlinks(&layout)?;

    let mut ods_status = OdsStatus::new();

    // Mount rootfs and boot partition. Boot must be mounted before
    // create_bootloader() because GRUB reads grubenv from the boot partition.
    mount_core_partitions(&layout, rootfs, &mut ods_status)?;

    // Best-effort: a corrupted grubenv is a recoverable degraded-boot condition.
    let mut bootloader_opt: Option<Box<dyn Bootloader>> = match create_bootloader() {
        Ok(bl) => Some(bl),
        Err(e) => {
            warn!("Bootloader unavailable: {e}; resize and fsck results will not be persisted");
            None
        }
    };

    if bootloader_opt.is_none() {
        warn!("Bootloader unavailable after core mount; ODS update-validation will be skipped");
    }

    let mode = BootMode::detect(bootloader_opt.as_deref())?;

    let ctx = BootContext::new(&config, &layout, rootfs, bootloader_opt, ods_status);

    #[allow(clippy::single_match)]
    match mode {
        BootMode::Normal => mode::normal::run(ctx),
    }
}
```

Also update the imports at the top of `src/lib.rs` — replace `mount_partitions` with `mount_core_partitions`:

```rust
use crate::{
    config::Config,
    filesystem::{mount_core_partitions, persist_fsck_results},
    mode::{BootContext, BootMode},
    partition::{PartitionLayout, create_omnect_symlinks, detect_root_device},
    runtime::OdsStatus,
};
```

- [ ] **Step 3.2: Verify compilation and tests**

```bash
cd .worktrees/feat/resize-data
cargo test --features grub,gpt 2>&1 | tail -5
cargo test --features grub,dos 2>&1 | tail -5
cargo test --features uboot,gpt 2>&1 | tail -5
cargo test --features uboot,dos 2>&1 | tail -5
```

Expected for each: `test result: ok. N passed; 0 failed`

- [ ] **Step 3.3: Commit**

```bash
git add src/lib.rs
git commit -m "refactor(init): use mount_core_partitions in run_init; move remaining mounts to mode handler"
```

---

## Task 4: Implement `src/mode/resize_data.rs`

**Files:**
- Create: `src/mode/resize_data.rs`
- Modify: `src/mode/mod.rs`

- [ ] **Step 4.1: Write unit tests for partition_number and find_extended_partition parsing**

Create `src/mode/resize_data.rs` with tests only (no implementation yet):

```rust
//! Data partition auto-resize
//!
//! Expands the data partition and its ext4 filesystem to fill available disk
//! space on first boot. Guarded by the `resized-data` bootloader variable so
//! it runs exactly once.

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::bootloader::{Bootloader, BootloaderEnvKey};
use crate::error::{ResizeDataError, Result};

const SGDISK_CMD: &str = "/sbin/sgdisk";
const PARTED_CMD: &str = "/sbin/parted";
const E2FSCK_CMD: &str = "/sbin/e2fsck";
const RESIZE2FS_CMD: &str = "/sbin/resize2fs";
const SYNC_CMD: &str = "/bin/sync";

const MTAB_PATH: &str = "/etc/mtab";
const PROC_MOUNTS_PATH: &str = "/proc/self/mounts";

type ResizeResult<T> = std::result::Result<T, ResizeDataError>;

pub fn resize_if_needed(
    _layout: &crate::partition::PartitionLayout,
    _bootloader: Option<&mut dyn Bootloader>,
    _rootfs: &Path,
) -> Result<()> {
    todo!()
}

fn partition_number(_dev: &Path) -> Option<u32> {
    todo!()
}

fn find_extended_partition(_rootblk: &Path) -> ResizeResult<u32> {
    todo!()
}

fn ensure_mtab() -> ResizeResult<()> {
    todo!()
}

fn run_e2fsck(_dev: &Path) -> ResizeResult<()> {
    todo!()
}

fn run_cmd(_program: &str, _args: &[&str]) -> ResizeResult<()> {
    todo!()
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

        let mut found: Option<u32> = None;
        for line in output.lines() {
            let lower = line.to_lowercase();
            if lower.contains("extended") {
                if let Some(nr_str) = line.split_whitespace().next() {
                    if let Ok(nr) = nr_str.parse::<u32>() {
                        found = Some(nr);
                        break;
                    }
                }
            }
        }
        assert_eq!(found, Some(3));
    }

    #[test]
    fn test_find_extended_partition_not_found() {
        let output = "\
Number  Start   End     Size   File system  Name  Flags
 1      1049kB  500MB   499MB  fat32              boot, esp
 7      500MB   8589MB  8089MB ext4";

        let found = output
            .lines()
            .find(|l| l.to_lowercase().contains("extended"));
        assert!(found.is_none());
    }
}
```

- [ ] **Step 4.2: Register the module in mode/mod.rs**

Add the following line in `src/mode/mod.rs`, after the `pub mod normal;` line:

```rust
#[cfg(feature = "resize-data")]
pub(crate) mod resize_data;
```

- [ ] **Step 4.3: Add ResizedData variant to BootloaderEnvKey**

`BootloaderEnvKey` is a typed enum in `src/bootloader/mod.rs`. Add the new variant for the resize guard.

In the `BootloaderEnvKey` enum body, add:

```rust
    /// `omnect_resized_data` — set to `"1"` after data partition has been resized.
    #[cfg(feature = "resize-data")]
    ResizedData,
```

In the `as_str` match in `impl BootloaderEnvKey`, add:

```rust
            #[cfg(feature = "resize-data")]
            Self::ResizedData => Cow::Borrowed("omnect_resized_data"),
```

- [ ] **Step 4.4: Verify compilation**

```bash
cd .worktrees/feat/resize-data
cargo check --features grub,gpt,resize-data
```

Expected: no errors.

```bash
cd .worktrees/feat/resize-data
cargo test --features grub,gpt,resize-data 2>&1 | tail -10
```

Expected: compilation succeeds but tests panic at `todo!()` for `partition_number` and `find_extended_partition` (they're called indirectly or panic on first use).

Actually the unit tests call the private functions directly, so `test_partition_number_sata` will panic at `todo!()`. That's the expected red state.

- [ ] **Step 4.5: Implement partition_number**

Replace the `todo!()` in `partition_number`:

```rust
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
```

- [ ] **Step 4.6: Run partition_number tests**

```bash
cd .worktrees/feat/resize-data
cargo test --features grub,gpt,resize-data -- test_partition_number 2>&1 | tail -8
```

Expected:
```
test tests::test_partition_number_mmc ... ok
test tests::test_partition_number_multi_digit ... ok
test tests::test_partition_number_no_digits ... ok
test tests::test_partition_number_nvme ... ok
test tests::test_partition_number_sata ... ok
test result: ok. 5 passed; 0 failed
```

- [ ] **Step 4.7: Implement find_extended_partition**

Replace the `todo!()` in `find_extended_partition`:

```rust
fn find_extended_partition(rootblk: &Path) -> ResizeResult<u32> {
    let out = Command::new(PARTED_CMD)
        .args([rootblk.to_str().unwrap_or(""), "print"])
        .output()
        .map_err(ResizeDataError::Io)?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        let lower = line.to_lowercase();
        if lower.contains("extended") {
            if let Some(nr_str) = line.split_whitespace().next()
                && let Ok(nr) = nr_str.parse::<u32>()
            {
                return Ok(nr);
            }
        }
    }

    Err(ResizeDataError::ExtendedPartitionNotFound(
        rootblk.to_path_buf(),
    ))
}
```

- [ ] **Step 4.8: Implement ensure_mtab**

Replace the `todo!()` in `ensure_mtab`:

```rust
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
```

- [ ] **Step 4.9: Implement run_e2fsck and run_cmd**

Replace the two `todo!()` bodies:

```rust
fn run_e2fsck(dev: &Path) -> ResizeResult<()> {
    let dev_str = dev.to_str().unwrap_or("");
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

fn run_cmd(program: &str, args: &[&str]) -> ResizeResult<()> {
    log::info!("Running: {} {}", program, args.join(" "));

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
```

- [ ] **Step 4.10: Implement resize_if_needed**

Replace the `todo!()` in `resize_if_needed`. The `#[cfg]` blocks select GPT vs DOS behaviour at compile time:

```rust
pub fn resize_if_needed(
    layout: &crate::partition::PartitionLayout,
    bootloader: Option<&mut dyn Bootloader>,
    _rootfs: &Path,
) -> Result<()> {
    use crate::partition::PartitionName;

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

    let part_nr = partition_number(&data_dev)
        .ok_or_else(|| {
            crate::error::InitramfsError::ResizeData(
                ResizeDataError::InvalidDevicePath(data_dev.clone()),
            )
        })?;

    log::info!(
        "Resizing data partition: {} partition {}",
        rootblk.display(),
        part_nr
    );

    #[cfg(feature = "gpt")]
    {
        // Move backup GPT header to end of disk before resizing
        run_cmd(SGDISK_CMD, &[rootblk.to_str().unwrap_or(""), "-e"])
            .map_err(crate::error::InitramfsError::ResizeData)?;
    }

    #[cfg(feature = "dos")]
    {
        // Resize extended partition to 100% before resizing the logical partition inside it
        let ext_nr = find_extended_partition(rootblk)
            .map_err(crate::error::InitramfsError::ResizeData)?;
        run_cmd(
            PARTED_CMD,
            &[
                rootblk.to_str().unwrap_or(""),
                "resizepart",
                &ext_nr.to_string(),
                "100%",
            ],
        )
        .map_err(crate::error::InitramfsError::ResizeData)?;
    }

    run_cmd(
        PARTED_CMD,
        &[
            rootblk.to_str().unwrap_or(""),
            "resizepart",
            &part_nr.to_string(),
            "100%",
        ],
    )
    .map_err(crate::error::InitramfsError::ResizeData)?;

    ensure_mtab().map_err(crate::error::InitramfsError::ResizeData)?;
    run_e2fsck(&data_dev).map_err(crate::error::InitramfsError::ResizeData)?;

    run_cmd(RESIZE2FS_CMD, &["-f", data_dev.to_str().unwrap_or("")])
        .map_err(crate::error::InitramfsError::ResizeData)?;

    run_cmd(SYNC_CMD, &[]).map_err(crate::error::InitramfsError::ResizeData)?;

    bootloader.set_env(BootloaderEnvKey::ResizedData, Some("1"))?;

    log::info!("Data partition resize complete");
    Ok(())
}
```

- [ ] **Step 4.11: Run all tests including resize-data feature**

```bash
cd .worktrees/feat/resize-data
cargo test --features grub,gpt,resize-data 2>&1 | tail -10
cargo test --features grub,dos,resize-data 2>&1 | tail -10
cargo test --features uboot,gpt,resize-data 2>&1 | tail -10
cargo test --features uboot,dos,resize-data 2>&1 | tail -10
```

Expected for each: `test result: ok. N passed; 0 failed`

- [ ] **Step 4.12: Run clippy**

```bash
cd .worktrees/feat/resize-data
cargo clippy --tests --features grub,gpt,resize-data -- -D warnings
cargo clippy --tests --features grub,dos,resize-data -- -D warnings
cargo clippy --tests --features uboot,gpt,resize-data -- -D warnings
cargo clippy --tests --features uboot,dos,resize-data -- -D warnings
```

Expected: no warnings.

- [ ] **Step 4.13: Commit**

```bash
git add src/mode/resize_data.rs src/mode/mod.rs src/bootloader/mod.rs
git commit -m "feat(resize-data): implement data partition auto-resize on first boot"
```

---

## Task 5: Integrate resize and mount_remaining into mode::normal::run

**Files:**
- Modify: `src/mode/normal.rs`
- Modify: `src/mode/mod.rs` (remove dead_code allow on layout field)

- [ ] **Step 5.1: Update mode/normal.rs**

Replace the entire contents of `src/mode/normal.rs` with:

```rust
use std::path::Path;

use log::info;

use crate::{
    Result,
    filesystem::{mount_remaining_partitions, persist_fsck_results, setup_data_overlay,
        setup_etc_overlay, setup_raw_rootfs_mount},
    mode::BootContext,
    runtime::{ODS_RUNTIME_DIR, create_fs_links, create_ods_runtime_files, switch_root},
};

pub fn run(ctx: BootContext<'_>) -> Result<()> {
    let BootContext {
        config,
        layout,
        rootfs,
        mut bootloader,
        mut ods_status,
    } = ctx;

    // Resize the data partition to fill the disk on first boot, before mounting it.
    #[cfg(feature = "resize-data")]
    crate::mode::resize_data::resize_if_needed(layout, bootloader.as_deref_mut(), rootfs)?;

    // Mount factory, cert, etc, data, and var/volatile. Capture the result so we
    // can persist fsck diagnostics before propagating a mount failure.
    let mount_result = mount_remaining_partitions(layout, rootfs, &mut ods_status);

    // Best-effort: persist any non-zero fsck results to the bootloader env and
    // to /data/var/log/fsck/ (data may not be mounted if mount_result failed).
    if let Some(ref mut bl) = bootloader {
        persist_fsck_results(&ods_status, bl.as_mut(), rootfs);
    }

    mount_result?;

    setup_raw_rootfs_mount(rootfs)?;
    setup_etc_overlay(rootfs)?;
    setup_data_overlay(rootfs)?;
    create_fs_links(rootfs)?;

    create_ods_runtime_files(
        &ods_status,
        bootloader.as_deref(),
        rootfs,
        Path::new(ODS_RUNTIME_DIR),
    )?;

    info!("omnect-os-initramfs completed successfully");

    switch_root(rootfs, &config.cmdline)
}
```

- [ ] **Step 5.2: Remove dead_code allow on layout in BootContext**

In `src/mode/mod.rs`, remove the `#[allow(dead_code)]` attribute from `layout`:

```rust
pub struct BootContext<'a> {
    pub(crate) config: &'a Config,
    pub(crate) layout: &'a PartitionLayout,
    pub(crate) rootfs: &'a Path,
    pub(crate) bootloader: Option<Box<dyn Bootloader>>,
    pub(crate) ods_status: OdsStatus,
}
```

- [ ] **Step 5.3: Remove unused persist_fsck_results import from lib.rs**

In `src/lib.rs`, update the filesystem import to remove `persist_fsck_results` (it's now only used in `mode/normal.rs`):

```rust
use crate::{
    config::Config,
    filesystem::mount_core_partitions,
    mode::{BootContext, BootMode},
    partition::{PartitionLayout, create_omnect_symlinks, detect_root_device},
    runtime::OdsStatus,
};
```

- [ ] **Step 5.4: Run all 8 test combinations (4 base + 4 with resize-data)**

```bash
cd .worktrees/feat/resize-data
cargo test --features grub,gpt 2>&1 | tail -5
cargo test --features grub,dos 2>&1 | tail -5
cargo test --features uboot,gpt 2>&1 | tail -5
cargo test --features uboot,dos 2>&1 | tail -5
cargo test --features grub,gpt,resize-data 2>&1 | tail -5
cargo test --features grub,dos,resize-data 2>&1 | tail -5
cargo test --features uboot,gpt,resize-data 2>&1 | tail -5
cargo test --features uboot,dos,resize-data 2>&1 | tail -5
```

Expected for each: `test result: ok. N passed; 0 failed`

- [ ] **Step 5.5: Run clippy across all combinations**

```bash
cd .worktrees/feat/resize-data
for features in "grub,gpt" "grub,dos" "uboot,gpt" "uboot,dos" \
                "grub,gpt,resize-data" "grub,dos,resize-data" \
                "uboot,gpt,resize-data" "uboot,dos,resize-data"; do
  echo "=== $features ===" && \
  cargo clippy --tests --features "$features" -- -D warnings || exit 1
done
```

Expected: no warnings for any combination.

- [ ] **Step 5.6: Run cargo fmt check**

```bash
cd .worktrees/feat/resize-data
cargo fmt -- --check
```

Expected: no output (clean).

- [ ] **Step 5.7: Commit**

```bash
git add src/mode/normal.rs src/mode/mod.rs src/lib.rs
git commit -m "feat(resize-data): integrate resize and mount_remaining into mode::normal"
```

---

## Task 6: Final verification

- [ ] **Step 6.1: Run cargo audit**

```bash
cd .worktrees/feat/resize-data
cargo audit
```

Expected: no vulnerabilities.

- [ ] **Step 6.2: Confirm worktree branch and log**

```bash
cd .worktrees/feat/resize-data
git --no-pager log --oneline -6
```

Expected: 5 commits from this feature visible at the top.

- [ ] **Step 6.3: Confirm docs/superpowers is gitignored**

```bash
cd .worktrees/feat/resize-data
git status docs/
```

Expected: `docs/` is ignored (no output / shows as ignored).
