# Boot Mode Dispatch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Introduce a `BootMode` enum with top-level dispatch so `main.rs` becomes a thin PID-1 shim, `run_init()` lives in the library (unit-testable), and each future mode can be added in its own isolated PR without touching shared code.

**Architecture:** A new `src/mode/` module owns `BootMode { Normal }`, `BootContext`, and `detect()`. `run_init()` is promoted to `lib.rs` as a public function that does preflight (device detection, mount, fsck persistence, bootloader init) then dispatches to `mode::normal::run(ctx)`. `main.rs` shrinks to early init + logging setup + one call to `omnect_os_init::run_init()`.

**Tech Stack:** Rust, `thiserror`, `log`, feature flags `grub|uboot × gpt|dos` (all four combos must pass). No new crate dependencies.

**Branch:** `feat/boot-mode-dispatch`

---

## File Map

| Action | File | Responsibility |
|--------|------|----------------|
| Create | `src/mode/mod.rs` | `ROOTFS_DIR`, `BootMode`, `BootContext`, `detect()`, unit tests |
| Create | `src/mode/normal.rs` | Post-mount logic moved verbatim from `run()` in `main.rs` |
| Modify | `src/lib.rs` | Add `pub mod mode;`, `use` imports, `pub fn run_init()` |
| Modify | `src/main.rs` | Remove `run()` + `ROOTFS_DIR`, call `omnect_os_init::run_init()` |

---

## Task 1 — Write failing `BootMode::detect()` tests

**Files:**
- Create: `src/mode/mod.rs`

- [ ] **Step 1.1: Create `src/mode/mod.rs` with tests only (will not compile)**

```rust
// src/mode/mod.rs — test scaffold; implementation follows in Task 2

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootloader::MockBootloader;

    #[test]
    fn detect_normal_with_live_bootloader() {
        let mock = MockBootloader::new();
        let mode = BootMode::detect(Some(&mock)).unwrap();
        assert!(matches!(mode, BootMode::Normal));
    }

    #[test]
    fn detect_normal_degraded_boot_no_bootloader() {
        let mode = BootMode::detect(None).unwrap();
        assert!(matches!(mode, BootMode::Normal));
    }
}
```

- [ ] **Step 1.2: Run to verify compile failure**

```bash
cargo test --features grub,gpt 2>&1 | head -20
```

Expected: compile error — `BootMode` and `BootContext` not found.

---

## Task 2 — Implement `BootMode`, `BootContext`, `detect()` in `src/mode/mod.rs`

**Files:**
- Modify: `src/mode/mod.rs`

- [ ] **Step 2.1: Replace `src/mode/mod.rs` with full implementation**

```rust
// src/mode/mod.rs
use std::path::Path;

use crate::{
    Bootloader,
    Result,
    config::Config,
    partition::PartitionLayout,
    runtime::OdsStatus,
};

pub mod normal;

/// The root filesystem mount point inside the initramfs.
///
/// Defined here (not in `main.rs`) so `run_init()` and all mode handlers
/// share a single source of truth.
pub const ROOTFS_DIR: &str = "/rootfs";

/// Context passed to every mode handler.
///
/// Mode handlers are invoked with **all partitions mounted**: rootfs read-only
/// at `/rootfs`, boot at `/rootfs/boot`, factory/data/cert/etc at their
/// standard mount points. `persist_fsck_results` has already run. Handlers
/// own the lifecycle of any overlay or bind mounts and must not assume
/// additional preflight will occur. Future modes (factory-reset, flash-mode)
/// that need to unmount partitions before acting do so internally.
pub struct BootContext<'a> {
    pub config: &'a Config,
    pub layout: &'a PartitionLayout,
    pub rootfs: &'a Path,
    pub bootloader: Option<Box<dyn Bootloader>>,
    pub ods_status: OdsStatus,
}

/// The detected boot mode to execute.
///
/// Only `Normal` ships in this PR. Future variants (`FactoryReset`, `Resize`,
/// `FlashMode`) are added in their respective implementation PRs alongside
/// their detection logic, typed payloads, and `BootloaderEnvKey` additions.
pub enum BootMode {
    Normal,
    // FactoryReset(FactoryResetConfig) — added in the factory-reset PR
    // Resize                           — added in the resize PR
    // FlashMode(FlashKind)             — added in the flash-mode PR
}

impl BootMode {
    /// Detect the boot mode from bootloader environment variables.
    ///
    /// Accepts `Option<&dyn Bootloader>`. Returns `Normal` when the bootloader
    /// is absent (degraded boot: no env vars readable → no special mode).
    ///
    /// The `_bl` parameter is intentionally unused until the first additional
    /// mode variant lands. Rename to `bl` and add detection logic in the
    /// respective implementation PR.
    pub fn detect(_bl: Option<&dyn Bootloader>) -> Result<Self> {
        Ok(Self::Normal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootloader::MockBootloader;

    #[test]
    fn detect_normal_with_live_bootloader() {
        let mock = MockBootloader::new();
        let mode = BootMode::detect(Some(&mock)).unwrap();
        assert!(matches!(mode, BootMode::Normal));
    }

    #[test]
    fn detect_normal_degraded_boot_no_bootloader() {
        let mode = BootMode::detect(None).unwrap();
        assert!(matches!(mode, BootMode::Normal));
    }
}
```

> **Note:** `pub mod normal;` references `src/mode/normal.rs` which doesn't exist yet.
> This will fail to compile until Task 3 creates that file.

- [ ] **Step 2.2: Verify the expected compile error is about `normal.rs`, not `BootMode`**

```bash
cargo check --features grub,gpt 2>&1 | head -10
```

Expected: error about `normal` module not found (not about `BootMode` or `BootContext`).

---

## Task 3 — Create `src/mode/normal.rs`

**Files:**
- Create: `src/mode/normal.rs`

This is the post-mount logic from the current `run()` in `src/main.rs`, extracted verbatim.
No new tests: all four existing integration-test feature combos cover this path end-to-end.

- [ ] **Step 3.1: Create `src/mode/normal.rs`**

```rust
// src/mode/normal.rs
use std::path::Path;

use log::info;

use crate::{
    Result,
    filesystem::{setup_data_overlay, setup_etc_overlay, setup_raw_rootfs_mount},
    mode::BootContext,
    runtime::{ODS_RUNTIME_DIR, create_fs_links, create_ods_runtime_files, switch_root},
};

pub fn run(ctx: BootContext<'_>) -> Result<()> {
    let BootContext {
        config,
        rootfs,
        bootloader,
        ods_status,
        ..
    } = ctx;

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

- [ ] **Step 3.2: Verify `src/mode/` compiles in isolation**

```bash
cargo check --features grub,gpt 2>&1 | head -20
```

Expected: either passes, or only errors about `mode` not yet wired into `lib.rs` (which is fine at this stage — the module tree is not yet attached).

---

## Task 4 — Update `src/lib.rs`

**Files:**
- Modify: `src/lib.rs`

Add `pub mod mode;`, the required `use` imports, and `pub fn run_init()`.
The existing `pub mod`/`pub use` lines are preserved; new items are inserted per the file-organization rule (all `use` before `pub mod`).

- [ ] **Step 4.1: Replace `src/lib.rs` with the updated version**

```rust
//! omnect-os-init library
//!
//! This library provides the core functionality for the omnect-os init process.
//! It replaces the bash-based initramfs scripts with a type-safe Rust implementation.

use std::path::Path;

use log::{info, warn};

use crate::{
    config::Config,
    filesystem::{mount_partitions, persist_fsck_results},
    mode::{BootContext, BootMode, ROOTFS_DIR},
    partition::{PartitionLayout, create_omnect_symlinks, detect_root_device},
    runtime::OdsStatus,
};

pub mod bootloader;
pub mod config;
pub mod early_init;
pub mod error;
pub mod filesystem;
pub mod logging;
pub mod mode;
pub mod partition;
pub mod runtime;

// Re-export main types for convenience
pub use crate::bootloader::{Bootloader, create_bootloader};
pub use crate::early_init::mount_essential_filesystems;
pub use crate::error::{InitramfsError, Result};
pub use crate::logging::KmsgLogger;

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

    // Mount all partitions; boot must be mounted before create_bootloader()
    // (GRUB reads grubenv from rootfs/boot/EFI/BOOT/grubenv).
    let mount_result = mount_partitions(&layout, rootfs, &mut ods_status);

    // Best-effort: a corrupted grubenv is a recoverable degraded-boot condition.
    // Promote failure to None so the rest of init proceeds; ODS bootloader-dependent
    // state is skipped rather than aborting a boot that otherwise succeeds.
    let mut bootloader_opt: Option<Box<dyn Bootloader>> = match create_bootloader() {
        Ok(bl) => Some(bl),
        Err(e) => {
            warn!("Bootloader unavailable: {e}; fsck results will not be persisted");
            None
        }
    };

    // Persist fsck results BEFORE propagating mount_result.
    // FsckRequiresReboot exits through mount_result?; diagnostics must be in
    // the bootloader env before that reboot fires.
    if let Some(ref mut bl) = bootloader_opt {
        persist_fsck_results(&ods_status, bl.as_mut(), rootfs);
    }

    mount_result?;

    let mode = BootMode::detect(bootloader_opt.as_deref())?;

    let ctx = BootContext {
        config: &config,
        layout: &layout,
        rootfs,
        bootloader: bootloader_opt,
        ods_status,
    };

    // single_match: intentional scaffolding — additional variants land with
    // their implementation PRs.
    #[allow(clippy::single_match)]
    match mode {
        BootMode::Normal => mode::normal::run(ctx),
    }
}
```

- [ ] **Step 4.2: Verify the library compiles**

```bash
cargo check --features grub,gpt 2>&1
```

Expected: clean (no errors, no warnings with `-D warnings` in effect via clippy; `check` itself doesn't enforce `-D warnings` but watch for obvious issues).

---

## Task 5 — Slim down `src/main.rs`

**Files:**
- Modify: `src/main.rs`

Remove `run()`, `ROOTFS_DIR`, and all imports that were only used in `run()`.
Change the call site to `omnect_os_init::run_init()`.

- [ ] **Step 5.1: Replace `src/main.rs` with the slimmed-down version**

```rust
//! omnect-os-init - Rust-based init process for omnect-os initramfs
//!
//! This binary replaces the bash-based initramfs scripts with a type-safe
//! Rust implementation.

use std::process;
use std::thread;
use std::time::Duration;

use log::{error, warn};

use omnect_os_init::{
    error::{FilesystemError, InitramfsError},
    logging::{KmsgLogger, log_fatal},
    mount_essential_filesystems,
};

/// Sleep duration for fatal error loop (seconds)
const FATAL_ERROR_SLEEP_SECS: u64 = 60;
const BASH_CMD: &str = "/bin/bash";
const SH_CMD: &str = "/bin/sh";

fn main() {
    // Mount essential filesystems first (/dev, /proc, /sys, /run)
    if let Err(e) = mount_essential_filesystems() {
        eprintln!("FATAL: Failed to mount essential filesystems: {}", e);
        spawn_emergency_shell();
    }

    // Release vs. debug mode is a build-time property via the `release-image` feature.
    let is_release_image = cfg!(feature = "release-image");

    // Initialize logging — fatal if /dev/kmsg cannot be opened or logger already set.
    // log_fatal() opens /dev/kmsg directly so the message reaches the kernel ring buffer
    // even before the global logger is registered.
    let logger_result = KmsgLogger::new()
        .map_err(|e| InitramfsError::Io(std::io::Error::other(format!("Failed to open kmsg: {e}"))))
        .and_then(|logger| {
            logger.init().map_err(|e| {
                InitramfsError::Io(std::io::Error::other(format!(
                    "Logger initialization failed: {e}"
                )))
            })
        });
    if let Err(ref e) = logger_result {
        log_fatal(&format!("{e}"));
        handle_fatal_error(logger_result.unwrap_err(), is_release_image);
    }

    // Run main initialization
    if let Err(e) = omnect_os_init::run_init() {
        error!("Initramfs failed: {e}");
        handle_fatal_error(e, is_release_image);
    }
}

/// Handle fatal errors based on image type
fn handle_fatal_error(error: InitramfsError, is_release: bool) -> ! {
    // fsck exit code 2 means fsck explicitly requests a reboot before mounting.
    if matches!(
        error,
        InitramfsError::Filesystem(FilesystemError::FsckRequiresReboot { .. })
    ) {
        error!("fsck requires reboot: {}", error);
        let _ = nix::sys::reboot::reboot(nix::sys::reboot::RebootMode::RB_AUTOBOOT);
        // reboot(2) should not return; loop as a last resort
        loop {
            thread::sleep(Duration::from_secs(FATAL_ERROR_SLEEP_SECS));
        }
    }

    if is_release {
        // Release image: loop forever to prevent reboot loops
        loop {
            error!("FATAL: {}", error);
            thread::sleep(Duration::from_secs(FATAL_ERROR_SLEEP_SECS));
        }
    } else {
        // Debug image: spawn shell
        warn!("Debug mode: spawning shell due to error: {}", error);
        spawn_debug_shell();
    }
}

/// Spawn emergency shell (before logging available)
fn spawn_emergency_shell() -> ! {
    // PID 1 must never exit. Respawn the shell so the operator can retry.
    // Use eprintln! — the kmsg logger may not be initialised yet at this point.
    loop {
        match process::Command::new(SH_CMD).status() {
            Ok(status) => eprintln!("Emergency shell exited with {status} — respawning"),
            Err(e) => {
                eprintln!(
                    "Failed to spawn emergency shell ({e}) — retrying in {FATAL_ERROR_SLEEP_SECS}s"
                );
                thread::sleep(Duration::from_secs(FATAL_ERROR_SLEEP_SECS));
            }
        }
    }
}

/// Spawn debug shell for debugging
fn spawn_debug_shell() -> ! {
    // PID 1 must never exit — the kernel would panic. Respawn the shell
    // in a loop so the operator can re-enter after an accidental exit.
    loop {
        let status = process::Command::new(BASH_CMD)
            .arg("--init-file")
            .arg("/dev/null")
            .status();

        match status {
            Ok(_) => log::info!("debug shell exited — respawning"),
            Err(e) => {
                log::warn!("bash unavailable ({e}), falling back to sh");
                match process::Command::new(SH_CMD).status() {
                    Ok(_) => log::info!("sh exited — respawning"),
                    Err(e) => {
                        log::error!("sh also unavailable ({e}) — sleeping before retry");
                        thread::sleep(Duration::from_secs(FATAL_ERROR_SLEEP_SECS));
                    }
                }
            }
        }
    }
}
```

---

## Task 6 — Verify all feature combinations

- [ ] **Step 6.1: Format check**

```bash
cargo fmt -- --check
```

Expected: exits 0, no output.

- [ ] **Step 6.2: Clippy — all four combos**

```bash
cargo clippy --tests --features grub,gpt -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module
cargo clippy --tests --features grub,dos -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module
cargo clippy --tests --features uboot,gpt -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module
cargo clippy --tests --features uboot,dos -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module
```

Expected: all four exit 0, no warnings.

- [ ] **Step 6.3: Tests — all four combos**

```bash
cargo test --features grub,gpt
cargo test --features grub,dos
cargo test --features uboot,gpt
cargo test --features uboot,dos
```

Expected: all pass, including the two new `mode::tests::detect_*` tests.

- [ ] **Step 6.4: Build — all four combos**

```bash
cargo build --features grub,gpt
cargo build --features grub,dos
cargo build --features uboot,gpt
cargo build --features uboot,dos
```

Expected: all four produce a binary without errors.

---

## Task 7 — Commit

- [ ] **Step 7.1: Stage and commit**

```bash
git add src/mode/mod.rs src/mode/normal.rs src/lib.rs src/main.rs
git commit -m "feat(mode): introduce BootMode dispatch scaffold

- Add src/mode/mod.rs: BootMode { Normal }, BootContext, detect()
- Add src/mode/normal.rs: post-mount logic moved from main.rs::run()
- Promote run_init() to lib.rs as a public function (unit-testable)
- Slim main.rs to PID-1 shim (~25 lines); remove run() and ROOTFS_DIR
- Add two unit tests for BootMode::detect() covering live and degraded-boot paths

Normal boot path is functionally unchanged. Future mode variants
(FactoryReset, Resize, FlashMode) land in their own PRs alongside
detection logic and typed payloads.

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```
