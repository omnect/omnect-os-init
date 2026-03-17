# Project Context

## 1. Architecture & Tech Stack
- **Role:** Initramfs init process for omnect Secure OS
- **Runtime:** Runs as PID 1 in initramfs before switch_root
- **Language:** Rust (safety-critical, no_std-friendly patterns)
- **Target:** Embedded Linux (x86-64 EFI with GRUB, ARM with U-Boot)

## 2. Key Files
- `src/main.rs`: Entry point, mounts essential filesystems, initializes logging
- `src/lib.rs`: Library exports for all modules
- `src/error.rs`: Hierarchical error types (`InitramfsError`, subsystem errors)
- `src/early_init.rs`: Mounts `/dev`, `/proc`, `/sys` before anything else
- `src/bootloader/mod.rs`: Trait-based abstraction over GRUB/U-Boot
- `src/bootloader/grub.rs`: GRUB implementation using `grub-editenv`
- `src/bootloader/uboot.rs`: U-Boot implementation using `fw_printenv`/`fw_setenv`
- `src/config/mod.rs`: Parses `/proc/cmdline` and `/etc/os-release`
- `src/logging/kmsg.rs`: Writes to `/dev/kmsg` with kernel log levels

## 3. Build & Test Commands
- **Build:** `cargo build` / `cargo build --release`
- **Check:** `cargo check`
- **Test:** `cargo test`
- **Lint:** `cargo clippy -- -D warnings`
- **Format:** `cargo fmt -- --check`

## 4. Feature Flags
| Feature | Purpose |
|---------|---------|
| `core` | Default, required functionality |
| `factory-reset` | Backup/wipe/restore operations |
| `flash-mode-1` | Disk cloning |
| `flash-mode-2` | Network flashing |
| `flash-mode-3` | HTTP/HTTPS flashing |
| `resize-data` | Auto-resize data partition |
| `persistent-var-log` | Persistent `/var/log` mount |

## 5. Runtime Constraints
- **No heap allocator dependency** for early init paths
- **Read-only rootfs:** All state goes to `/data` or bootloader env
- **Logging:** Available only after `/dev` is mounted
- **Exit behavior:** 
  - Release image: infinite loop on fatal error (prevent reboot loops)
  - Debug image: spawn shell for debugging

## 6. Key Patterns
- **Error handling:** `thiserror` for typed errors, `Result<T>` everywhere
- **Bootloader abstraction:** `dyn Bootloader` trait for GRUB/U-Boot
- **Compression:** fsck exit code (integer) stored in bootloader env; full output written to `/data/var/log/fsck/<partition>.log`
- **Idempotent mounts:** `is_mounted()` check before mounting

## 7. Integration Points
- **Kernel cmdline:** `rootpart=`, `rootblk=`, `root=`, `quiet`
- **os-release:** `OMNECT_RELEASE_IMAGE`, `MACHINE_FEATURES`, `DISTRO_FEATURES`
- **Device symlinks:** Creates `/dev/omnect/{boot,rootfs,data,...}`
- **ODS:** Prepares runtime files for `omnect-device-service`