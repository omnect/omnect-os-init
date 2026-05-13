# Design: Boot Mode Dispatch Architecture (v2)

**Date:** 2026-04-28 (revised after v2 review)
**Status:** Draft
**Supersedes:** `2026-04-22-boot-mode-dispatch-design.md`
**Branch:** `feat/boot-mode-dispatch` (planned)

---

## Problem

`main.rs::run()` is a hardcoded sequential chain that orchestrates every
initramfs step directly. As additional execution modes (factory-reset, resize,
flash-mode) are added, this function would grow into an unmaintainable tangle of
conditional logic. Each new mode is an alternative execution *path*, not another
sequential step appended to normal boot.

---

## Goal

Introduce a `BootMode` enum with top-level dispatch so that:

- `main.rs` becomes a thin ~15-line PID-1 shim
- `run()` moves into the library so it is unit-testable
- Each mode lives in its own isolated module
- The normal boot path is functionally unchanged
- Future modes (factory-reset, resize, flash-mode) can be implemented in
  separate PRs without touching shared code

---

## Architecture

### Module Structure

```
src/
├── main.rs                  # Thin PID-1 shim: early init, logging, call run_init()
├── lib.rs                   # + pub mod mode; + pub fn run_init()
├── mode/
│   ├── mod.rs               # BootMode enum, detect(), BootContext, ROOTFS_DIR
│   └── normal.rs            # Current run() logic minus preflight
├── bootloader/              # Unchanged
├── config/                  # Unchanged
├── early_init.rs            # Unchanged
├── error.rs                 # Unchanged
├── filesystem/              # Unchanged
├── logging/                 # Unchanged
├── partition/               # Unchanged
└── runtime/                 # Unchanged
```

**`ROOTFS_DIR` relocation:** the `const ROOTFS_DIR: &str = "/rootfs"` currently in `main.rs`
moves to `src/mode/mod.rs` where it is used by `run_init()` and accessible to mode handlers.

---

### `BootMode` Enum

This PR introduces only the `Normal` variant. Future mode variants (`FactoryReset`,
`Resize`, `FlashMode`) are added in their respective implementation PRs alongside
their detection logic, typed payloads, and `BootloaderEnvKey` additions.

Shipping detection of modes the binary cannot handle would create new boot
failures on first-boot devices (where `omnect_resized_data` has never been set)
and on field devices that happen to have legacy env vars present. Detection of a
mode must not land before the mode implementation.

```rust
pub enum BootMode {
    Normal,
    // FactoryReset(FactoryResetConfig) — added in the factory-reset PR
    // Resize                           — added in the resize PR
    // FlashMode(FlashKind)             — added in the flash-mode PR
}
```

**Future-PR guidance for typed payloads:**
- `FlashMode` → `FlashKind { Disk, Network, Http }` enum (never `u8`)
- `FactoryReset` → `FactoryResetConfig::new(raw: String) -> Self` constructor
  with private inner field, so validation can be added in one place without
  changing `BootMode`'s public API
- New `BootloaderEnvKey` variants (`FlashMode`, `FactoryReset`, `ResizedData`)
  are added alongside their detection logic in the respective PRs

---

### `BootMode::detect()`

Accepts `Option<&dyn Bootloader>`. The signature is fixed now so future mode
variants can be added without changing the call site in `run()`. With only
`Normal` in the enum this PR, the body is a no-op: no env vars are read and
`Normal` is always returned. Detection logic for each future mode is added
alongside its variant in the respective implementation PR.

```rust
impl BootMode {
    // `_bl` prefix: intentionally unused until the first mode variant lands.
    // Rename to `bl` and add detection logic in the respective implementation PR.
    pub fn detect(_bl: Option<&dyn Bootloader>) -> Result<Self> {
        Ok(Self::Normal)
    }
}
```

---

### `BootContext`

All mode handlers receive a single context struct. This prevents argument-list
drift as future modes add dependencies.

```rust
pub struct BootContext<'a> {
    pub config: &'a Config,
    pub layout: &'a PartitionLayout,
    pub rootfs: &'a Path,
    pub bootloader: Option<Box<dyn Bootloader>>,
    pub ods_status: OdsStatus,
}
```

---

### `lib.rs` changes

Two additions to `lib.rs`:

```rust
pub mod mode;          // wires src/mode/ into the library
pub fn run_init() -> Result<()> { ... }   // see body below
```

`run_init()` is exposed as a top-level library function so tests can call
`omnect_os_init::run_init()` directly without spawning a subprocess.

---

### `lib.rs::run_init()` — preflight and dispatch

`run_init()` lives in `lib.rs` and is reachable from unit tests via
`omnect_os_init::run_init()`.

**Critical ordering constraint:** `persist_fsck_results` must be called before
`mount_result?` is propagated. The `FsckRequiresReboot` reboot path exits through
`mount_result?`; the fsck diagnostics must be in the bootloader env before that
reboot fires. This ordering is preserved verbatim from the current code.

**Degraded-boot tolerance:** `create_bootloader()` failure is not fatal. A
corrupted grubenv is a recoverable condition; the device boots degraded (ODS
bootloader-dependent state skipped, mode forced to `Normal`).

```rust
// src/lib.rs
pub fn run_init() -> Result<()> {
    let config = Config::load()?;
    let rootfs = Path::new(ROOTFS_DIR);

    let root_device = detect_root_device(&config.cmdline)?;
    let layout = PartitionLayout::new(root_device)?;
    create_omnect_symlinks(&layout)?;

    let mut ods_status = OdsStatus::new();

    // Mount all partitions; boot must be mounted before create_bootloader()
    // (GRUB reads grubenv from rootfs/boot/EFI/BOOT/grubenv).
    let mount_result = mount_partitions(&layout, rootfs, &mut ods_status);

    // Best-effort: bootloader may be unavailable (corrupted grubenv, etc.)
    let mut bootloader_opt: Option<Box<dyn Bootloader>> = match create_bootloader() {
        Ok(bl) => Some(bl),
        Err(e) => {
            warn!("Bootloader unavailable: {e}; fsck results will not be persisted");
            None
        }
    };

    // Persist fsck results BEFORE propagating mount_result (FsckRequiresReboot path).
    if let Some(ref mut bl) = bootloader_opt {
        persist_fsck_results(&ods_status, bl.as_mut(), rootfs);
    }

    mount_result?;

    let mode = BootMode::detect(bootloader_opt.as_deref())?;

    let ctx = BootContext { config: &config, layout: &layout, rootfs, bootloader: bootloader_opt, ods_status };

    // #[allow(clippy::single_match)]: single arm is intentional scaffolding;
    // additional variants land with their implementation PRs.
    #[allow(clippy::single_match)]
    match mode {
        BootMode::Normal => mode::normal::run(ctx),
    }
}
```

No dispatch stubs for unimplemented modes exist. The `match` is exhaustive over
the single `Normal` variant and requires no `InitramfsError` additions.

---

### `main.rs` after refactor

`main.rs` becomes a thin shim. `run()`, `ROOTFS_DIR`, and all imports that
belonged to `run()` move to `lib.rs`. `main.rs` retains only: `main()`,
`handle_fatal_error()`, `spawn_emergency_shell()`, `spawn_debug_shell()`.

```rust
// src/main.rs (sketch)
fn main() {
    if let Err(e) = mount_essential_filesystems() { spawn_emergency_shell(); }
    // ... logger init ...
    if let Err(e) = omnect_os_init::run_init() {
        error!("Initramfs failed: {e}");
        handle_fatal_error(e, cfg!(feature = "release-image"));
    }
}
```

---

### `mode/normal.rs`

Receives `BootContext` and contains the current post-mount logic verbatim:
setup overlays, fs-links, ODS runtime files, switch_root. It does **not** repeat
any preflight steps.

---

### Mount-precondition contract

Stated in `mode/mod.rs` module-level doc:

> Mode handlers are invoked with **all partitions mounted**: rootfs read-only at
> `/rootfs`, boot at `/rootfs/boot`, factory/data/cert/etc at their standard
> mount points. `persist_fsck_results` has already run. Handlers own the
> lifecycle of any overlay or bind mounts and must not assume additional preflight
> will occur. Future modes (factory-reset, flash-mode) that need to unmount
> partitions before acting do so internally.

This contract is intentionally broad for this PR. The factory-reset and
flash-mode implementation PRs may refine the preflight to split `mount_partitions`
into stages if partial-mount strategies are required.

---

## Error Handling

No new error variants are added to `InitramfsError` or `BootloaderError`.
The `match mode { }` is exhaustive over the single `Normal` variant, so no
error paths exist in the dispatch itself.

---

## Testing

### `BootMode::detect()` tests

`detect()` currently always returns `Normal`, so the test matrix is minimal.
Its value is verifying the infrastructure compiles and the `Option` contract
is honoured under all four feature combos.

| Test | Setup | Expected |
|------|-------|----------|
| Normal — live bootloader | `detect(Some(&mock))` | `BootMode::Normal` |
| Normal — degraded boot (no bootloader) | `detect(None)` | `BootMode::Normal` |

Expanded detection tests (priority ordering, `FlashKind` parsing, invalid values)
are added alongside each future mode variant in its own implementation PR.

### Existing tests

All four feature-combo integration tests (`grub+gpt`, `grub+dos`, `uboot+gpt`,
`uboot+dos`) must pass unchanged — the normal boot path is functionally
identical.

---

## Scope of This PR

| Change | Nature |
|--------|--------|
| `run()` moved from `main.rs` to `lib.rs` as `run_init()` | Code moved + renamed |
| `ROOTFS_DIR` moved from `main.rs` to `mode/mod.rs` | Code moved |
| `main.rs` reduced to ~25 lines | Code moved |
| New `src/mode/mod.rs` | New: `BootMode { Normal }`, `detect()`, `BootContext` |
| New `src/mode/normal.rs` | Code moved verbatim |
| `pub mod mode;` added to `lib.rs` | New |
| `run_init()` added to `lib.rs` | New function wrapping moved code |
| `run_init()` call in `main.rs` | New call site |
| `BootMode::detect()` unit tests | New tests |
| All other modules | **Untouched** |

**Out of scope (deferred to implementation PRs):**
- `BootMode::{ FactoryReset, Resize, FlashMode }` variants
- `FlashKind` enum, `FactoryResetConfig` struct
- `BootloaderEnvKey::{ FlashMode, FactoryReset, ResizedData }` variants
- factory-reset, resize, and flash-mode implementations
- splitting `mount_partitions()` into stages

---

## Future Duplication Risks

Not in scope here, but must be addressed when factory-reset and flash-mode are implemented:

| Risk | Where duplication will appear | Prevention |
|------|-------------------------------|------------|
| Overlay setup | factory-reset and resize both need `etc` writable | Extract `setup_overlays(ctx) -> Result<()>` callable from any mode |
| fsck persistence | each mode that mounts partitions needs `persist_fsck_results` | keep as shared helper in `boot_sequence.rs` (already there) |
| Switch-root boilerplate | Normal and "factory-reset complete" both end with `switch_root` | Extract `finalize_and_switch_root(ctx)` when second mode needs it |
| Reboot escalation | each mode handler decides reboot vs. shell | centralised in `handle_fatal_error` already |

`BootContext` (this PR) is the prerequisite for all of the above helpers.

---

## Verification

```bash
cargo fmt -- --check
cargo clippy --tests --features grub,gpt -- -D warnings
cargo clippy --tests --features grub,dos -- -D warnings
cargo clippy --tests --features uboot,gpt -- -D warnings
cargo clippy --tests --features uboot,dos -- -D warnings
cargo test --features grub,gpt
cargo test --features grub,dos
cargo test --features uboot,gpt
cargo test --features uboot,dos
cargo build --features grub,gpt
cargo build --features grub,dos
cargo build --features uboot,gpt
cargo build --features uboot,dos
```

> **Note:** `core` is a transitive dependency of `grub`, `uboot`, `gpt`, and `dos`
> (each declared as `= ["core"]` in `Cargo.toml`). Explicit `--no-default-features
> --features core,...` is redundant and omitted for consistency.
