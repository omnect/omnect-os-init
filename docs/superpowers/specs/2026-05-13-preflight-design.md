# Preflight: Separate One-Time Prep from Mode Dispatch

**Date:** 2026-05-13
**Status:** Draft
**Supersedes:** `2026-05-12-first-boot-mode-design.md`

---

## Problem

The current `feat/resize-data` branch introduced `BootMode::FirstBoot` to name the
resize operation in dispatch. This works for N=1 but models the wrong abstraction:

1. **`BootMode::FirstBoot` conflates two orthogonal axes.** A device doing a resize
   is still booting normally — only the *preparation* differs, not the *boot mode*.
   `BootMode` should describe navigation (which code path owns the rest of boot);
   resize is preparation that any mode could benefit from.

2. **Guard logic for resize leaks into `BootMode::detect()`**, making the
   mode-selection function responsible for both "decide what mode to run" and
   "decide which prep ran". Every new prep step would add another branch to
   `detect()`.

3. **Doesn't scale.** Two orthogonal prep steps with two orthogonal boot modes
   produce four variant combinations, or convoluted detect() logic.

4. **Factory reset interaction is unresolved.** A factory reset on first boot
   should still benefit from resize (the partition being wiped should be
   correctly sized). `BootMode::FirstBoot` + `BootMode::FactoryReset` cannot
   coexist without variant multiplication or special-casing.

5. **Naming collision.** `is_first_boot` already names a different event in
   `src/filesystem/overlayfs.rs` (the etc-overlay seeding event). A
   `BootMode::FirstBoot` variant adds a second meaning to the same phrase.

---

## Approach

Introduce a **preflight phase** between core-mount/bootloader-open and mode dispatch.
Preflight is a flat sequence of independently feature-gated, idempotent prep steps.
Each step owns its own trigger (guard check). Steps cannot change the boot mode; they
only prepare the system so any mode can proceed.

```
run_init()
├── discover     — root device, layout, symlinks, core mount, open bootloader env
├── preflight    — conditional one-time prep (feature-gated, idempotent)
└── dispatch     — BootMode::detect() → mode handler → switch_root
```

`BootMode` reverts to describing only navigation (`Normal` today; `FactoryReset`,
`FlashMode` in future PRs). `BootMode::detect()` contains no prep-step logic.

---

## Architecture

### Preflight module

`src/preflight/mod.rs` provides a `PreflightCtx` value type and a `run()` sequencer:

```rust
#[non_exhaustive]
pub struct PreflightCtx<'a> {
    pub layout: &'a PartitionLayout,
    pub bootloader: Option<&'a mut dyn Bootloader>,
}

pub fn run(mut ctx: PreflightCtx<'_>) -> Result<()> {
    #[cfg(feature = "resize-data")]
    resize_data::run(&mut ctx)?;
    Ok(())
}
```

Adding a future step = one new file + one `#[cfg]` line in `run()`. Order and
conditions are visible at the call site. No traits, no registry, no allocation.

### Preflight resize-data step

`src/preflight/resize_data.rs` is a thin coordinator that owns the guard logic and
delegates the disk work to `filesystem::resize_data`:

```rust
pub fn run(ctx: &mut PreflightCtx<'_>) -> Result<()> {
    let Some(ref mut bl) = ctx.bootloader else {
        log::warn!("Bootloader unavailable; skipping resize-data preflight");
        return Ok(());
    };
    if bl.get_env(BootloaderEnvKey::ResizedData)?.is_some() {
        return Ok(()); // guard present — already resized
    }
    filesystem::resize_data::resize_if_needed(ctx.layout, bl)
}
```

**Guard ownership:** The `omnect_resized_data` env-var check lives here, not in
`BootMode::detect()`. A preflight step owns its own trigger. Hoisting guard checks
into `detect()` couples step-specific logic into mode-selection.

### `filesystem::resize_data` (unchanged)

`src/filesystem/resize_data.rs` retains the pure disk operation: sgdisk/parted
partition resize, ensure_mtab, fsck, resize2fs, sync. No guard logic. No
`BootloaderEnvKey::ResizedData` reference. It now only sets the guard on success:

```rust
pub fn resize_if_needed(
    layout: &PartitionLayout,
    bootloader: &mut dyn Bootloader,
) -> Result<()> { ... }
```

### `BootMode` (reverted to navigation-only)

`src/mode/mod.rs` removes `FirstBoot` and the resize guard from `detect()`:

```rust
pub enum BootMode {
    Normal,
    // FactoryReset, FlashMode — future PRs
}

impl BootMode {
    pub fn detect(bl: Option<&dyn Bootloader>) -> Result<Self> {
        let _ = bl;
        Ok(Self::Normal)
    }
}
```

`BootloaderEnvKey::ResizedData` is removed from `BootloaderEnvKey` (or kept if
needed for reading after boot — but its write path no longer goes through detect).

### Orchestrator wiring (`src/lib.rs`)

```rust
mount_core_partitions(&layout, rootfs, &mut ods_status)?;

let mut bootloader_opt: Option<Box<dyn Bootloader>> = match open_bootloader_env() {
    Ok(bl) => Some(bl),
    Err(e) => {
        warn!("Bootloader environment unavailable: {e}; booting in degraded mode");
        None
    }
};

// Preflight: idempotent one-time prep before mode dispatch.
preflight::run(preflight::PreflightCtx {
    layout: &layout,
    bootloader: bootloader_opt.as_deref_mut(),
})?;

let mode = BootMode::detect(bootloader_opt.as_deref())?;
let ctx = BootContext::new(&config, &layout, rootfs, bootloader_opt, ods_status);
match mode {
    BootMode::Normal => mode::normal::run(ctx),
}
```

### Module layout (after)

```
src/
├── preflight/                  ← NEW
│   ├── mod.rs                  — PreflightCtx + run() sequencer
│   └── resize_data.rs          — guard check + calls filesystem::resize_data
├── filesystem/
│   └── resize_data.rs          — pure disk op (unchanged; no guard logic)
├── mode/
│   ├── mod.rs                  — BootMode::Normal only; no resize guard in detect()
│   └── normal.rs               — no resize cfg gate (already clean)
└── lib.rs                      — calls preflight::run() between bootloader and dispatch
```

**Removed:** `src/mode/first_boot.rs`, `BootMode::FirstBoot` variant, resize guard
branch in `BootMode::detect()`.

---

## Behaviour Preservation

| Behaviour | Before (FirstBoot) | After (preflight) |
|---|---|---|
| Resize skipped when bootloader unavailable | ✓ (detect returns Normal) | ✓ (preflight step warns + returns Ok) |
| Resize skipped when guard present | ✓ (detect returns Normal) | ✓ (preflight step returns Ok) |
| Resize skipped when data partition missing | ✓ (filesystem layer) | ✓ (filesystem layer unchanged) |
| Resize sets guard after success | ✓ | ✓ |
| Resize runs after core mount, before data mount | ✓ | ✓ |
| Resize failure is fatal | ✓ | ✓ |
| `normal::run` mounts remaining + overlays + ODS + switch_root | ✓ | ✓ |
| Degraded boot when bootloader unavailable | ✓ | ✓ |

Zero functional change.

---

## Rationale: Why Not `BootMode::FirstBoot`

See `plan-preflight.md` §3.1 for the full analysis. Summary:

- Resize is *preparation*, not *navigation*. It does not select a code path; it
  adjusts the disk so the chosen code path can proceed.
- A `BootMode::FirstBoot` cannot coexist cleanly with `BootMode::FactoryReset`.
  Preflight runs before dispatch, so factory reset gets a correctly-sized partition
  without any variant multiplication.
- `BootMode::detect()` should be a single-concern function (mode selection).
  Guard checks for prep steps belong with those steps.

---

## Future: When to Evolve the Preflight Shape

Today's flat `cfg`-gated free functions are correct for N=1. Triggers to introduce
enum dispatch or a trait registry are documented in `plan-preflight.md` §11.
Preflight-failure policy for non-Normal modes (`FactoryReset` should survive a
preflight failure) is documented in `plan-preflight.md` §10.4 — deferred until
`BootMode::FactoryReset` lands.
