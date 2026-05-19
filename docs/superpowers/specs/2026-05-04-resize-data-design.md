# resize-data: Data Partition Auto-Resize

## Problem

When an omnect OS image is flashed to a disk larger than the image itself, the
data partition occupies only the space baked into the image. The remaining disk
space is wasted unless the partition and its filesystem are expanded on first
boot.

The old `resize_data` branch implemented this but predated the `BootMode`
dispatch architecture introduced in `feat/boot-mode-dispatch`. This design ports
that implementation into the new structure.

## Approach

Port `src/runtime/resize_data.rs` from the `resize_data` branch into
`src/mode/resize_data.rs`, integrating it as a first-boot step within
`mode::normal::run`. This requires splitting `mount_partitions` into two phases
so the bootloader can be created (needs boot mounted) before the data partition
is touched.

## Architecture

### Feature gate

Controlled by a compile-time Cargo feature `resize-data`, consistent with the
`gpt`/`dos`/`grub`/`uboot` feature pattern. The Yocto recipe appends
`resize-data` to the cargo features when `DISTRO_FEATURES` contains
`resize-data`. No runtime `DISTRO_FEATURES` check in Rust code.

### Module structure

```
src/
  mode/
    mod.rs           — adds #[cfg(feature = "resize-data")] pub(crate) mod resize_data;
    resize_data.rs   — NEW: ported resize logic
    normal.rs        — updated: calls resize_if_needed before mount_remaining
  filesystem/
    boot_sequence.rs — updated: mount_partitions split into two functions
  error.rs           — updated: ResizeDataError + InitramfsError::ResizeData variant
  lib.rs             — updated: run_init uses mount_core_partitions
```

### Init sequence change

**Before:**
```
mount_partitions (rootfs + boot + factory + cert + etc + data)
create_bootloader
persist_fsck_results
BootMode::detect
BootContext::new
mode::normal::run
```

**After:**
```
mount_core_partitions (rootfs + boot)
create_bootloader
BootMode::detect
BootContext::new
mode::normal::run:
  [cfg(resize-data)] resize_if_needed
  mount_remaining_partitions (factory + cert + etc + data)
  persist_fsck_results
  setup overlays / ODS / switch_root
```

### fsck persistence on core-mount failure

The current code calls `persist_fsck_results` before propagating
`mount_result?` so that fsck diagnostics are saved even when mounting aborts
early. With the split, a failure in `mount_core_partitions` (e.g. boot fsck
exit 2 requiring reboot) propagates immediately out of `run_init` before the
bootloader is created. Fsck results recorded in `ods_status` at that point are
lost.

This is an acceptable pre-existing trade-off: boot-partition fsck failures are
rare and the kernel ring buffer (kmsg) retains the log output. A future
improvement could create the bootloader earlier (U-Boot does not need boot
mounted), but that is out of scope for this feature.

### `mount_core_partitions` and `mount_remaining_partitions`

`src/filesystem/boot_sequence.rs` gains two public functions:

- `mount_core_partitions(layout, rootfs, ods_status)` — mounts rootCurrent
  (read-only) and boot (read-write). Runs fsck on boot and records the result in
  `ods_status`. Replaces the first half of the current `mount_partitions`.

- `mount_remaining_partitions(layout, rootfs, ods_status)` — mounts factory,
  cert, etc, and data. Runs fsck on each and records results. Replaces the
  second half of the current `mount_partitions`.

The existing `mount_partitions` function is removed.

### `resize_if_needed` public API

```rust
// src/mode/resize_data.rs
pub fn resize_if_needed(
    layout: &PartitionLayout,
    bootloader: Option<&mut dyn Bootloader>,
    rootfs: &Path,
) -> crate::error::Result<()>
```

- If `bootloader` is `None` (degraded boot): log warning, return `Ok(())`.
- If `resized-data` bootloader variable is already set: log info, return `Ok(())`.
- Otherwise run the resize sequence:
  1. GPT: move backup GPT header to end of disk (`sgdisk -e`).
     DOS: resize extended partition to 100% first (`parted resizepart <ext_nr> 100%`).
  2. Resize data partition to 100% (`parted resizepart <part_nr> 100%`).
  3. Ensure `/etc/mtab` exists (symlink to `/proc/self/mounts` if absent).
  4. Run `e2fsck -y`; exit codes 0 and 1 are acceptable.
  5. Run `resize2fs -f <data_dev>`.
  6. Run `sync`.
  7. Set `resized-data=1` in bootloader env.

GPT vs DOS branching is handled via `#[cfg(feature = "gpt")]` and
`#[cfg(feature = "dos")]`, not via a runtime enum.

### Error handling

- `ResizeDataError` (ported from old branch) added to `src/error.rs` behind
  `#[cfg(feature = "resize-data")]`.
- `InitramfsError::ResizeData(#[from] ResizeDataError)` variant added behind
  the same feature gate.
- A resize failure is fatal — the data partition is in an unknown state and
  the boot should not proceed.

### `BootContext` changes

- `layout` field loses `#[allow(dead_code)]` — it is now used by
  `mode::normal::run` for `mount_remaining_partitions` and resize.
- No structural changes to `BootContext` are required.

## Testing

- All unit tests from the old `resize_data` branch are ported to
  `src/mode/resize_data.rs`:
  - `test_partition_number_*` (sata, nvme, mmc, multi-digit, no digits)
  - `test_find_extended_partition_parses_output`
  - `test_find_extended_partition_not_found`
- `mount_core_partitions` and `mount_remaining_partitions` are covered by
  adapting the existing `mount_partitions` integration tests in
  `src/filesystem/boot_sequence.rs`.
- All four feature combinations must pass:
  - `cargo test --features grub,gpt`
  - `cargo test --features grub,dos`
  - `cargo test --features uboot,gpt`
  - `cargo test --features uboot,dos`
  - (and the same four with `,resize-data` appended)

## Out of scope

- Online resize (data partition already mounted).
- Resize of partitions other than data.
- Runtime detection of whether resize is needed (size check before resizing).
  The existing guard (`resized-data` bootloader variable) is sufficient.
