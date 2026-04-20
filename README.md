# omnect-os-init

Rust-based init process for omnect-os initramfs.

## Overview

Replaces 14 bash-based initramfs scripts (~1500 LOC) with a single Rust binary
acting as `/init` in the initramfs. Runs as PID 1 before `switch_root`.

Implemented functionality:

- **Bootloader abstraction**: Unified `Bootloader` trait for GRUB (`grub-editenv`) and U-Boot (`fw_printenv`/`fw_setenv`); fsck output persisted across reboots as gzip+base64 in the bootloader env (encoded via busybox `gzip`/`base64` — no crate dependencies)
- **Configuration**: Parses `/proc/cmdline`; build-time constants from Yocto environment via `build.rs`
- **Partition management**: Root device detection, partition layout (GPT/DOS), `/dev/omnect/*` symlinks
- **Filesystem operations**: fsck, mount manager (RAII), overlayfs for `/etc` and `/home`, bind mounts
- **Logging**: Kernel ring buffer (`/dev/kmsg`) with log level prefixes
- **ODS integration**: Runtime files for `omnect-device-service`
- **fs-links**: Symlink creation from `etc/omnect/fs-link.json` and `etc/omnect/fs-link.d/`
- **switch\_root**: MS_MOVE + chroot + exec systemd (`pivot_root(2)` is not used; ramfs does not support it)

Not yet implemented (planned):

- Factory reset (backup, wipe, restore)
- Flash modes (disk clone, network, HTTP/HTTPS)
- Data partition auto-resize

## Building

```bash
# Debug build (bootloader type must be specified)
cargo build --features grub     # x86-64 EFI targets
cargo build --features uboot    # ARM targets

# Release build (optimized for size)
cargo build --release --features grub
cargo build --release --features uboot

# With additional optional features
cargo build --release --features "grub,persistent-var-log"
```

## Features

| Feature | Description | Status |
|---------|-------------|--------|
| `core` | Core boot sequence (default) | Implemented |
| `grub` | GRUB bootloader support — x86-64 EFI targets | Implemented |
| `uboot` | U-Boot bootloader support — ARM targets | Implemented |
| `gpt` | GPT partition table layout | Implemented |
| `dos` | DOS/MBR partition table layout | Implemented |
| `persistent-var-log` | Bind-mount `/var/log` to data partition | Implemented |
| `release-image` | Release error handling (loop on fatal error) | Implemented |
| `factory-reset` | Factory reset support | Planned |
| `flash-mode-1` | Disk cloning | Planned |
| `flash-mode-2` | Network flashing | Planned |
| `flash-mode-3` | HTTP/HTTPS flashing | Planned |
| `resize-data` | Data partition auto-resize | Planned |

> **Note:** `grub` and `uboot` are mutually exclusive. Exactly one must be set at build time.
> The Yocto recipe selects the correct feature via `CARGO_FEATURES` based on `MACHINE_FEATURES`.

## Testing

```bash
# All four valid feature combinations (bootloader × partition table)
cargo test --features grub,gpt   # x86-64 targets, GPT
cargo test --features grub,dos   # x86-64 targets, DOS/MBR
cargo test --features uboot,gpt  # ARM targets, GPT
cargo test --features uboot,dos  # ARM targets, DOS/MBR

# Verbose output
cargo test --features grub,gpt -- --nocapture
```

## Architecture

```
src/
├── main.rs                  # Entry point (PID 1)
├── lib.rs                   # Library exports
├── error.rs                 # Error type hierarchy
├── early_init.rs            # Mount /dev, /proc, /sys, /run before logging
├── bootloader/
│   ├── mod.rs               # Bootloader trait + build-time selection (grub/uboot feature)
│   ├── grub.rs              # GRUB implementation (grub-editenv)
│   ├── uboot.rs             # U-Boot implementation (fw_printenv/fw_setenv)
│   └── types.rs             # BootloaderType enum
├── config/
│   └── mod.rs               # /proc/cmdline parser; build-time constants via build.rs
├── filesystem/
│   ├── mod.rs               # Public API
│   ├── boot_sequence.rs     # Mount + fsck orchestration (testable with mock bootloaders)
│   ├── fsck.rs              # e2fsck wrapper (all exit codes handled)
│   ├── mount.rs             # Mount primitives (RAII, idempotency checks)
│   └── overlayfs.rs         # /etc overlay, /home overlay, bind mounts
├── logging/
│   ├── mod.rs               # KmsgLogger initializer
│   └── kmsg.rs              # /dev/kmsg writer with kernel log levels
├── partition/
│   ├── mod.rs               # Public API
│   ├── device.rs            # Root device detection (GRUB: blkid/fsuuid, U-Boot: root=)
│   ├── layout.rs            # GPT/DOS partition map builder
│   └── symlinks.rs          # /dev/omnect/* symlink creation
└── runtime/
    ├── mod.rs               # Public API
    ├── fs_link.rs           # fs-link symlink creation
    ├── omnect_device_service.rs  # ODS JSON status file writer
    └── switch_root.rs       # MS_MOVE new root to / + chroot + exec init
```

## License

MIT OR Apache-2.0
