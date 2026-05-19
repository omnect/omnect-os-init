# Preflight Phase Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `BootMode::FirstBoot` with a dedicated `preflight` phase that runs
one-time idempotent prep steps before mode dispatch. Resize-data becomes the first
preflight step. `BootMode` reverts to navigation-only.

**Architecture:** See `docs/superpowers/specs/2026-05-13-preflight-design.md`.

**Starting state:** `feat/resize-data` branch with `BootMode::FirstBoot` and
`mode::first_boot::run()` already implemented. `filesystem/resize_data.rs` already
holds the pure disk operation (no guard). The guard currently lives in
`BootMode::detect()`.

**Tech Stack:** Rust, `#[cfg(feature = "resize-data")]`, `cargo test`

**Worktree:** `.worktrees/feat/resize-data`

---

## File Map

| Action | File | Change |
|---|---|---|
| Create | `src/preflight/mod.rs` | `PreflightCtx` + `run()` sequencer |
| Create | `src/preflight/resize_data.rs` | Guard check + calls `filesystem::resize_data` |
| Delete | `src/mode/first_boot.rs` | Removed — replaced by preflight step |
| Modify | `src/mode/mod.rs` | Remove `FirstBoot` variant + resize guard from `detect()` |
| Modify | `src/lib.rs` | Add `preflight::run()` call; remove `FirstBoot` match arm |
| Modify | `src/filesystem/resize_data.rs` | Remove `BootloaderEnvKey::ResizedData` set (moves to preflight step) OR keep as-is (see Task 3) |

---

## Task 1: Create `src/preflight/mod.rs`

**Files:**
- Create: `src/preflight/mod.rs`

- [ ] **Step 1.1: Create the preflight module**

```rust
//! Preflight: conditional one-time prep steps that run after core mount
//! and the bootloader env is open, but before mode dispatch.
//!
//! Each step is independently feature-gated and idempotent — guarded by
//! bootloader env or filesystem state so it runs at most once per trigger.

#[cfg(feature = "resize-data")]
pub mod resize_data;

use crate::{Result, bootloader::Bootloader, partition::PartitionLayout};

/// Context passed to each preflight step.
#[non_exhaustive]
pub struct PreflightCtx<'a> {
    pub layout: &'a PartitionLayout,
    pub bootloader: Option<&'a mut dyn Bootloader>,
}

/// Run all enabled preflight steps in order.
///
/// Steps are independent and idempotent. Order is intentional: resize-data
/// must run before any partition is mounted read-write.
pub fn run(mut ctx: PreflightCtx<'_>) -> Result<()> {
    #[cfg(feature = "resize-data")]
    resize_data::run(&mut ctx)?;
    Ok(())
}
```

- [ ] **Step 1.2: Register module in `src/lib.rs`**

Add `pub mod preflight;` to the module declarations in `src/lib.rs` (alphabetical order).

- [ ] **Step 1.3: Verify compilation**

```bash
cd .worktrees/feat/resize-data
cargo check --features grub,gpt,resize-data 2>&1 | grep "^error"
```

Expected: no errors.

---

## Task 2: Create `src/preflight/resize_data.rs`

The preflight step owns the guard check and delegates disk work to
`filesystem::resize_data::resize_if_needed`.

**Files:**
- Create: `src/preflight/resize_data.rs`

- [ ] **Step 2.1: Create the preflight resize-data step**

```rust
//! Preflight step: data partition auto-resize
//!
//! Checks the `omnect_resized_data` guard and, if absent, expands the data
//! partition to fill available disk space via `filesystem::resize_data`.
//! Runs at most once per image lifetime — the guard prevents re-execution.

use crate::bootloader::BootloaderEnvKey;
use crate::error::Result;
use crate::preflight::PreflightCtx;

pub fn run(ctx: &mut PreflightCtx<'_>) -> Result<()> {
    let Some(ref mut bl) = ctx.bootloader else {
        log::warn!("resize-data preflight: bootloader unavailable; skipping");
        return Ok(());
    };

    if bl.get_env(BootloaderEnvKey::ResizedData)?.is_some() {
        log::debug!("resize-data preflight: guard present; already resized");
        return Ok(());
    }

    crate::filesystem::resize_data::resize_if_needed(ctx.layout, *bl)
}
```

- [ ] **Step 2.2: Verify compilation**

```bash
cd .worktrees/feat/resize-data
cargo check --features grub,gpt,resize-data 2>&1 | grep "^error"
```

Expected: no errors.

---

## Task 3: Update `filesystem::resize_data::resize_if_needed` signature

The current signature takes `&mut Box<dyn Bootloader>`. The preflight step passes
`&mut dyn Bootloader` (unwrapped from the `Option`). Align the signature.

**Files:**
- Modify: `src/filesystem/resize_data.rs`
- Modify: `src/filesystem/mod.rs`

- [ ] **Step 3.1: Change the signature in `resize_if_needed`**

Change:
```rust
pub fn resize_if_needed(
    layout: &crate::partition::PartitionLayout,
    bootloader: &mut Box<dyn Bootloader>,
) -> Result<()> {
```

To:
```rust
pub fn resize_if_needed(
    layout: &crate::partition::PartitionLayout,
    bootloader: &mut dyn Bootloader,
) -> Result<()> {
```

- [ ] **Step 3.2: Update the test in `resize_data.rs`**

The test `test_resize_skips_when_data_partition_absent` creates a
`Box<dyn Bootloader>`. Update the call site to pass `bl.as_mut()`:

```rust
assert!(resize_if_needed(&layout, bl.as_mut()).is_ok());
assert!(bl.get_env(BootloaderEnvKey::ResizedData).unwrap().is_none());
```

- [ ] **Step 3.3: Verify compilation and tests**

```bash
cd .worktrees/feat/resize-data
cargo test --features grub,gpt,resize-data 2>&1 | grep -E "^(error|test result)"
```

Expected: all tests pass.

---

## Task 4: Remove `BootMode::FirstBoot`

Revert `BootMode::detect()` to navigation-only. Remove the `FirstBoot` variant,
the resize guard branch, and `mode::first_boot`.

**Files:**
- Modify: `src/mode/mod.rs`
- Delete: `src/mode/first_boot.rs`
- Modify: `src/lib.rs`

- [ ] **Step 4.1: Update `BootMode` enum and `detect()` in `src/mode/mod.rs`**

Remove `BootMode::FirstBoot` and the resize guard from `detect()`:

```rust
#[cfg(feature = "resize-data")]
pub mod first_boot;    // ← DELETE this line
pub mod normal;

pub enum BootMode {
    // #[cfg(feature = "resize-data")]
    // FirstBoot,          ← DELETE
    Normal,
}

impl BootMode {
    pub fn detect(bl: Option<&dyn Bootloader>) -> Result<Self> {
        let _ = bl;
        Ok(Self::Normal)
    }
}
```

Also remove `BootloaderEnvKey` from the imports in `mod.rs` if no longer used.

Update the tests: `detect_first_boot_when_guard_absent` and
`detect_normal_when_guard_present` are no longer testing mode detection — delete
them. The `detect_normal_with_live_bootloader` test reverts to the simpler form:

```rust
#[test]
fn detect_normal_with_live_bootloader() {
    let mock = create_mock_bootloader();
    let mode = BootMode::detect(Some(&mock)).unwrap();
    assert!(matches!(mode, BootMode::Normal));
}
```

- [ ] **Step 4.2: Delete `src/mode/first_boot.rs`**

```bash
cd .worktrees/feat/resize-data
git rm src/mode/first_boot.rs
```

- [ ] **Step 4.3: Update `src/lib.rs`**

Replace:
```rust
match mode {
    #[cfg(feature = "resize-data")]
    BootMode::FirstBoot => mode::first_boot::run(ctx),
    BootMode::Normal => mode::normal::run(ctx),
}
```

With:
```rust
match mode {
    BootMode::Normal => mode::normal::run(ctx),
}
```

And insert the preflight call between bootloader open and `BootMode::detect`:

```rust
// Preflight: idempotent one-time prep before mode dispatch.
crate::preflight::run(crate::preflight::PreflightCtx {
    layout: &layout,
    bootloader: bootloader_opt.as_deref_mut(),
})?;

let mode = BootMode::detect(bootloader_opt.as_deref())?;
```

- [ ] **Step 4.4: Verify compilation**

```bash
cd .worktrees/feat/resize-data
cargo check --features grub,gpt,resize-data 2>&1 | grep "^error"
cargo check --features uboot,dos 2>&1 | grep "^error"
```

Expected: no errors on any feature combination.

---

## Task 5: Write preflight tests

**Files:**
- Modify: `src/preflight/resize_data.rs`

- [ ] **Step 5.1: Add unit tests to `src/preflight/resize_data.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootloader::{BootloaderEnvKey, MockBootloader};
    use crate::partition::{PartitionLayout, RootDevice};
    use crate::preflight::PreflightCtx;
    use std::collections::HashMap;

    fn empty_layout() -> PartitionLayout {
        PartitionLayout {
            partitions: HashMap::new(),
            device: RootDevice {
                base: std::path::PathBuf::from("/dev/sda"),
                partition_sep: "",
                root_partition: std::path::PathBuf::from("/dev/sda2"),
            },
        }
    }

    #[test]
    fn skips_when_bootloader_unavailable() {
        let layout = empty_layout();
        let mut ctx = PreflightCtx { layout: &layout, bootloader: None };
        assert!(run(&mut ctx).is_ok());
    }

    #[test]
    fn skips_when_guard_present() {
        let layout = empty_layout();
        let mut bl = MockBootloader::new()
            .with_env(BootloaderEnvKey::ResizedData, "1");
        let mut dyn_bl: &mut dyn crate::bootloader::Bootloader = &mut bl;
        let mut ctx = PreflightCtx {
            layout: &layout,
            bootloader: Some(dyn_bl),
        };
        assert!(run(&mut ctx).is_ok());
        // Guard was already set — resize_if_needed never called, guard unchanged.
        assert!(bl.get_env(BootloaderEnvKey::ResizedData).unwrap().is_some());
    }
}
```

- [ ] **Step 5.2: Run tests**

```bash
cd .worktrees/feat/resize-data
cargo test --features grub,gpt,resize-data preflight 2>&1
```

Expected: all preflight tests pass.

---

## Task 6: Full verification

- [ ] **Step 6.1: Run all eight feature combinations**

```bash
cd .worktrees/feat/resize-data
cargo test --features grub,gpt,resize-data 2>&1 | tail -3
cargo test --features grub,dos,resize-data 2>&1 | tail -3
cargo test --features uboot,gpt,resize-data 2>&1 | tail -3
cargo test --features uboot,dos,resize-data 2>&1 | tail -3
cargo test --features grub,gpt 2>&1 | tail -3
cargo test --features grub,dos 2>&1 | tail -3
cargo test --features uboot,gpt 2>&1 | tail -3
cargo test --features uboot,dos 2>&1 | tail -3
```

Expected: `test result: ok. N passed; 0 failed` for all.

- [ ] **Step 6.2: Clippy**

```bash
cd .worktrees/feat/resize-data
cargo clippy --tests --features grub,gpt,resize-data -- -D warnings 2>&1 | tail -5
cargo clippy --tests --features uboot,dos -- -D warnings 2>&1 | tail -5
```

Expected: no warnings or errors.

- [ ] **Step 6.3: Format check**

```bash
cd .worktrees/feat/resize-data
cargo fmt -- --check 2>&1
```

Expected: no output (clean).

- [ ] **Step 6.4: Commit**

```bash
cd .worktrees/feat/resize-data
git add src/preflight/ src/mode/ src/lib.rs src/filesystem/resize_data.rs
git commit -m "refactor(preflight): replace BootMode::FirstBoot with preflight phase

Introduce a preflight sequencer that runs idempotent one-time prep steps
between core mount / bootloader open and mode dispatch.

- src/preflight/mod.rs: PreflightCtx + run() sequencer
- src/preflight/resize_data.rs: guard check + delegate to filesystem layer
- BootMode::FirstBoot removed; detect() is navigation-only again
- mode::first_boot removed
- lib.rs: preflight::run() inserted before BootMode::detect()
- filesystem::resize_data: signature uses &mut dyn Bootloader

BootMode describes navigation. Preflight describes preparation. These are
orthogonal concerns; conflating them in BootMode::FirstBoot would not
scale when BootMode::FactoryReset arrives.

Signed-off-by: $(git config user.name) <$(git config user.email)>"
```
