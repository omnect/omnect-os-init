# Design: BootMode::FirstBoot and resize-data relocation

**Date:** 2026-05-12
**Status:** Superseded by `2026-05-13-preflight-design.md`
**Branch:** TBD (follows feat/resize-data)

> **Note:** This design was superseded before implementation. `BootMode::FirstBoot` and
> `mode::first_boot::run` were never shipped. Resize-data was implemented as a preflight
> step instead — see `2026-05-13-preflight-design.md`. Retained for design history.

---

## 1. Motivation

The current implementation has three structural tensions:

1. **`resize_data.rs` lives in `mode/`** but is a disk transformation, not a mode
   handler. It has no `BootContext`, no `switch_root`, and no OS boot sequence.
   It belongs with its peers in `filesystem/` (fsck, mounts, overlays).

2. **The resize guard (`omnect_resized_data`) is checked inside
   `resize_if_needed()`** rather than in `BootMode::detect()`, which is the
   correct place for dispatch decisions. The dispatcher (`lib.rs`) never knows a
   special first-boot operation is occurring, and `BootMode` describes every
   first boot as `Normal` even when disk transformation runs.

3. **`mode::normal::run()` contains a compile-time gate for resize** — coupling
   a feature-specific operation into the generic normal boot path.

---

## 2. Benefits

| Concern | Before | After |
|---|---|---|
| Where resize logic lives | `mode/resize_data.rs` | `filesystem/resize_data.rs` |
| Where guard check lives | Inside `resize_if_needed()` | `BootMode::detect()` |
| What `normal::run()` does | Resize + full boot | Full boot only |
| First boot named in dispatch | No | Yes (`BootMode::FirstBoot`) |
| Home for future first-boot ops | None | `mode::first_boot` |

---

## 3. Architecture

### Boot dispatch flow

```
run_init()
  └── BootMode::detect(bootloader)
        ├── [resize-data feature, bootloader Some, guard absent] → FirstBoot
        └── [otherwise]                                          → Normal

FirstBoot::run(ctx)
  ├── filesystem::resize_data::resize_if_needed(layout, bootloader)
  └── mode::normal::run(ctx)

Normal::run(ctx)
  ├── mount_remaining_partitions
  ├── persist_fsck_results
  ├── setup overlays
  ├── create_ods_runtime_files
  └── switch_root
```

### Module layout (after)

```
src/
  filesystem/
    mod.rs
    boot_sequence.rs
    fsck.rs
    resize_data.rs     ← moved from mode/
    ...
  mode/
    mod.rs             (BootMode enum + detect)
    first_boot.rs      ← NEW
    normal.rs          (no resize gate)
```

---

## 4. Component Changes

### `src/mode/resize_data.rs` → `src/filesystem/resize_data.rs`

- File moved; module doc comment updated.
- Remove the `omnect_resized_data` guard check and the "bootloader unavailable"
  early return from `resize_if_needed()` — both move to `BootMode::detect()`.
- Function signature unchanged:
  `pub fn resize_if_needed(layout, bootloader: &mut Option<Box<dyn Bootloader>>)`
- Internal `crate::` paths updated for new module location.

### `src/filesystem/mod.rs`

- Add `#[cfg(feature = "resize-data")] pub mod resize_data;`
- Re-export `resize_if_needed`.

### `src/mode/mod.rs`

- Add `#[cfg(feature = "resize-data")] pub mod first_boot;`
- Add enum variant: `#[cfg(feature = "resize-data")] FirstBoot,`
- `BootMode::detect()`: when bootloader is `Some` and `ResizedData` guard is
  absent → return `FirstBoot`. Bootloader `None` or guard present → `Normal`.

### `src/mode/first_boot.rs` (new)

```rust
pub fn run(mut ctx: BootContext<'_>) -> Result<()> {
    crate::filesystem::resize_data::resize_if_needed(ctx.layout, &mut ctx.bootloader)?;
    crate::mode::normal::run(ctx)
}
```

### `src/mode/normal.rs`

- Remove the `#[cfg(feature = "resize-data")]` call to `resize_if_needed`.

### `src/lib.rs`

- Add match arm:
  `#[cfg(feature = "resize-data")] BootMode::FirstBoot => mode::first_boot::run(ctx)`

---

## 5. Error Handling

| Case | Behavior |
|---|---|
| Bootloader `None` at detect | `Normal` returned — resize skipped (degraded boot) |
| `get_env(ResizedData)` fails in detect | Error propagated — init fails |
| Resize fails in `first_boot::run()` | Fatal — propagated to `run_init()` |
| Guard present at detect | `Normal` returned — resize skipped |

No new error variants required. The existing `ResizeDataError` / `InitramfsError`
hierarchy covers all cases.

---

## 6. Testing

### New test cases in `BootMode::detect()` (`#[cfg(feature = "resize-data")]`)

- With live bootloader + `ResizedData` guard absent → `FirstBoot`
- With live bootloader + `ResizedData` guard present → `Normal`

Existing tests (both returning `Normal`) remain valid — they cover the
`None`-bootloader degraded path and are feature-independent.

### `resize_if_needed()` tests

Module path updates only (`filesystem::resize_data` instead of
`mode::resize_data`). No logic changes.

### `first_boot::run()` integration test

Mock bootloader (guard absent) → verifies resize + normal boot sequence
completes without error.

### Feature matrix

All four combinations must pass (with `resize-data` feature to exercise `FirstBoot`):

```
cargo test --features grub,gpt,resize-data
cargo test --features grub,dos,resize-data
cargo test --features uboot,gpt,resize-data
cargo test --features uboot,dos,resize-data
```
