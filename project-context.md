# Project Context

## 1. Architecture & Tech Stack
- **Role:** Initramfs init process for omnect Secure OS
- **Runtime:** Runs as PID 1 in initramfs before switch_root
- **Language:** Rust (safety-critical, no_std-friendly patterns)
- **Target:** Embedded Linux (x86-64 EFI with GRUB, ARM with U-Boot)

## 2. Key Files
- `src/main.rs`: PID 1 shim — early init, logging setup, calls `omnect_os_init::run_init()`
- `src/lib.rs`: Library exports and `pub fn run_init()` (unit-testable boot orchestration)
- `src/error.rs`: Hierarchical error types (`InitramfsError`, subsystem errors)
- `src/early_init.rs`: Mounts `/dev`, `/proc`, `/sys` before anything else
- `src/bootloader/mod.rs`: Trait-based abstraction over GRUB/U-Boot
- `src/bootloader/grub.rs`: GRUB implementation using `grub-editenv`
- `src/bootloader/uboot.rs`: U-Boot implementation using `fw_printenv`/`fw_setenv`
- `src/config/mod.rs`: Parses `/proc/cmdline`; build-time constants from Yocto env via `build.rs`
- `src/logging/kmsg.rs`: Writes to `/dev/kmsg` with kernel log levels
- `src/mode/mod.rs`: `BootMode` enum, `BootContext<'a>`, `detect()`
- `src/mode/normal.rs`: Normal boot handler — overlays, fs-links, ODS runtime files, switch_root
- `src/partition/device.rs`: Detects root block device from cmdline (GRUB UUID or U-Boot path)
- `src/filesystem/overlayfs.rs`: Sets up overlayfs for `/etc`, `/home`; bind-mounts `/var/lib`, `/usr/local`
- `src/runtime/switch_root.rs`: MS_MOVE + chroot transition to real rootfs; execs init

## 3. Build & Test Commands
- **Build:** `cargo build` / `cargo build --release`
- **Check:** `cargo check`
- **Format:** `cargo fmt -- --check`
- **Lint:** `cargo clippy --tests --features <grub|uboot> -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module`
- **Test:** Run all four valid feature combinations:
  ```
  cargo test --features grub,gpt
  cargo test --features grub,dos
  cargo test --features uboot,gpt
  cargo test --features uboot,dos
  ```
- **Audit:** `cargo audit`

## 4. Feature Flags
| Feature | Purpose |
|---------|---------|
| `core` | Default, required functionality |
| `grub` | GRUB bootloader support (mutually exclusive with `uboot`) |
| `uboot` | U-Boot bootloader support (mutually exclusive with `grub`) |
| `gpt` | GPT partition table (primary partitions 1-7; mutually exclusive with `dos`) |
| `dos` | DOS/MBR partition table (extended at slot 4, logical 5-8; mutually exclusive with `gpt`) |
| `persistent-var-log` | Persistent `/var/log` mount |
| `release-image` | Release behaviour: infinite loop on fatal error |

## 5. Runtime Constraints
- **Heap allocation is used freely** (`String`, `PathBuf`, `HashMap`); the OS image provides a standard allocator
- **Read-only rootfs:** All state goes to `/data` or bootloader env
- **Logging:** Available only after `/dev` is mounted
- **Exit behavior:** 
  - Release image: infinite loop on fatal error (prevent reboot loops)
  - Debug image: spawn shell for debugging

## 6. Key Patterns
- **Error handling:** `thiserror` for typed errors, `Result<T>` everywhere
- **Bootloader abstraction:** `dyn Bootloader` trait for GRUB/U-Boot
- **Compression:** fsck exit code and full output stored in bootloader env as gzip+base64(`"exit_code\noutput"`); full output also written to `/data/var/log/fsck/<partition>.log`
- **Idempotent mounts:** `is_mounted()` check before mounting
- **No magic path strings:** All filesystem paths must be `const` values. Group related paths in a dedicated `pub mod mount_points` (or equivalent) rather than using inline string literals.
- **File organization:** `use`, `const`, `static`, and `type` declarations must appear at the top of the file, before any `fn`, `impl`, `struct`, or `enum` definitions. Exceptions: `use super::*` and imports inside `#[cfg(test)] mod tests` blocks are placed within those blocks.

## 7. Integration Points
- **Kernel cmdline:** `rootpart=` (GRUB: root partition number), `bootpart_fsuuid=` (GRUB: boot partition UUID), `root=` (U-Boot: full root device path), `init=` (optional init binary override), `quiet` (suppress console output); `rootblk=` is parsed for device symlink naming only — no logic reads it
- **Device symlinks:** Creates `/dev/omnect/{boot,rootfs,data,...}`
- **ODS:** Prepares runtime files for `omnect-device-service`

## 8. Planned Features (not yet implemented)

### BootMode variants
The `BootMode` enum (`src/mode/mod.rs`) currently only has `Normal`. The following variants are planned:
- `FactoryReset(FactoryResetConfig)` — wipes data partition, re-provisions device
- `Resize` — resizes partitions on first boot after image flash
- `FlashMode(FlashKind)` — enables in-field OS flashing

When implementing a new variant:
1. Add the variant to `BootMode` and update `BootMode::detect()` to read the relevant bootloader env key. If the key is absent or the bootloader is unavailable, `detect()` must return `Normal` (degraded boot).
2. Add typed payload structs as needed (define them in `src/mode/mod.rs` near the `BootMode` enum).
3. Add `BootloaderEnvKey` entries for the detection keys.
4. Add a handler module under `src/mode/` mirroring `src/mode/normal.rs`.
5. Cover in tests: env-var present + live bootloader, env-var present + no bootloader (degraded fallback to `Normal`), env-var absent.

## 9. Documentation Standards

### Source-code comments and doc-strings
- **Explain "why", not "what":** The code shows what it does; comments explain constraints, non-obvious rationale, or invariants.
- **No history in comments:** Do not reference previous implementations ("legacy bash", "previously this was…"), PR numbers, or merge order.
- **No forward scaffolding in comments:** Do not describe features not yet implemented in the same comment block. Track planned work in section 8 of this file instead.
- **Concise doc-strings:** A doc-string should be as long as it needs to be and no longer. Avoid restating the function signature or obvious behaviour.