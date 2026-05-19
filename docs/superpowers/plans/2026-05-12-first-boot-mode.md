# First-Boot Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Introduce `BootMode::FirstBoot` so the dispatcher correctly names first-boot
disk operations, and move `resize_data.rs` to `filesystem/` where it belongs as a disk
transformation.

**Architecture:** `BootMode::detect()` returns `FirstBoot` when the `omnect_resized_data`
guard is absent and a live bootloader is available. `mode::first_boot::run()` calls
`filesystem::resize_data::resize_if_needed()` then delegates to `mode::normal::run()`.
Guard detection moves from `resize_if_needed()` to `detect()`.

**Tech Stack:** Rust, `#[cfg(feature = "resize-data")]` feature flag, `cargo test` for
verification.

**Worktree:** `.worktrees/feat/resize-data`

---

## File Map

| File | Change |
|---|---|
| `src/mode/mod.rs` | Add `BootMode::FirstBoot` variant; update `detect()`; add module |
| `src/mode/first_boot.rs` | NEW — calls resize then delegates to normal |
| `src/mode/normal.rs` | Remove `#[cfg(feature = "resize-data")]` resize call |
| `src/mode/resize_data.rs` | DELETE — moved to filesystem |
| `src/filesystem/mod.rs` | Add `#[cfg(feature = "resize-data")] pub mod resize_data` + re-export |
| `src/filesystem/resize_data.rs` | NEW (moved) — remove guard + bootloader-None early return; tighten signature |
| `src/lib.rs` | Add `FirstBoot` match arm; remove `#[allow(clippy::single_match)]` |

---

## Task 1: Write failing detection tests

**Files:**
- Modify: `src/mode/mod.rs`

- [ ] **Step 1.1: Add two failing tests in `mod.rs`**

Append inside the existing `#[cfg(test)] mod tests` block in `src/mode/mod.rs`:

```rust
    #[cfg(feature = "resize-data")]
    #[test]
    fn detect_first_boot_when_guard_absent() {
        // No ResizedData key → first boot
        let mock = create_mock_bootloader();
        let mode = BootMode::detect(Some(&mock)).unwrap();
        assert!(matches!(mode, BootMode::FirstBoot));
    }

    #[cfg(feature = "resize-data")]
    #[test]
    fn detect_normal_when_guard_present() {
        // ResizedData already set → normal boot
        let mock = create_mock_bootloader()
            .with_env(crate::bootloader::BootloaderEnvKey::ResizedData, "1");
        let mode = BootMode::detect(Some(&mock)).unwrap();
        assert!(matches!(mode, BootMode::Normal));
    }
```

- [ ] **Step 1.2: Verify tests fail to compile**

```bash
cd .worktrees/feat/resize-data
cargo test --features grub,gpt,resize-data 2>&1 | head -20
```

Expected: compile error — `BootMode::FirstBoot` not found.

---

## Task 2: Scaffold `BootMode::FirstBoot` and `mode::first_boot`

**Files:**
- Modify: `src/mode/mod.rs`
- Create: `src/mode/first_boot.rs`
- Modify: `src/lib.rs`

- [ ] **Step 2.1: Add `FirstBoot` variant to `BootMode`**

In `src/mode/mod.rs`, change the enum to:

```rust
/// The detected boot mode to execute.
pub enum BootMode {
    #[cfg(feature = "resize-data")]
    FirstBoot,
    Normal,
}
```

- [ ] **Step 2.2: Add `first_boot` module declaration**

In `src/mode/mod.rs`, add below the existing module declarations:

```rust
#[cfg(feature = "resize-data")]
pub mod first_boot;
pub mod normal;
```

- [ ] **Step 2.3: Create stub `src/mode/first_boot.rs`**

```rust
use crate::{Result, mode::BootContext};

pub fn run(_ctx: BootContext<'_>) -> Result<()> {
    todo!("first_boot::run not yet implemented")
}
```

- [ ] **Step 2.4: Add `FirstBoot` match arm in `src/lib.rs`**

Replace the existing match block:

```rust
    match mode {
        #[cfg(feature = "resize-data")]
        BootMode::FirstBoot => mode::first_boot::run(ctx),
        BootMode::Normal => mode::normal::run(ctx),
    }
```

Also remove the `#[allow(clippy::single_match)]` attribute above the match (it's no
longer a single-variant match on resize-data builds).

- [ ] **Step 2.5: Verify tests compile but `detect_first_boot_when_guard_absent` fails**

```bash
cd .worktrees/feat/resize-data
cargo test --features grub,gpt,resize-data detect_first_boot 2>&1
```

Expected: test compiles and runs, but **FAILS** — `detect()` still returns `Normal`.

---

## Task 3: Update `BootMode::detect()` to return `FirstBoot`

**Files:**
- Modify: `src/mode/mod.rs`

- [ ] **Step 3.1: Update `detect()` implementation**

Replace the existing `detect()` function in `src/mode/mod.rs`:

```rust
    /// Detect the boot mode from bootloader environment variables.
    ///
    /// Returns `Normal` when the bootloader is absent (degraded boot).
    pub fn detect(bl: Option<&dyn Bootloader>) -> Result<Self> {
        #[cfg(feature = "resize-data")]
        {
            if let Some(bl) = bl {
                if bl
                    .get_env(crate::bootloader::BootloaderEnvKey::ResizedData)?
                    .is_none()
                {
                    return Ok(Self::FirstBoot);
                }
            }
        }
        let _ = bl;
        Ok(Self::Normal)
    }
```

- [ ] **Step 3.2: Run all mode tests**

```bash
cd .worktrees/feat/resize-data
cargo test --features grub,gpt,resize-data mode 2>&1
```

Expected: all 4 tests in `mode::tests` pass:
- `detect_normal_with_live_bootloader` ← guard is set in this test? No — the default
  mock has no keys → this now returns `FirstBoot`.

> ⚠️ **Note:** `detect_normal_with_live_bootloader` uses a mock with no keys. After
> this change it will return `FirstBoot`, not `Normal` — so this test **will fail**.
> That test was written before `FirstBoot` existed and its assertion is now wrong.
> Update it:

In `mod.rs` tests, change `detect_normal_with_live_bootloader` to:

```rust
    #[test]
    fn detect_normal_with_live_bootloader() {
        // Guard is set → normal boot even with a live bootloader
        let mock = create_mock_bootloader()
            .with_env(crate::bootloader::BootloaderEnvKey::ResizedData, "1");
        let mode = BootMode::detect(Some(&mock)).unwrap();
        assert!(matches!(mode, BootMode::Normal));
    }
```

> **On non-resize-data builds** this test still works: `detect()` always returns
> `Normal` regardless of env keys, so adding a key doesn't change the assertion.

- [ ] **Step 3.3: Run all mode tests again and verify all pass**

```bash
cd .worktrees/feat/resize-data
cargo test --features grub,gpt,resize-data mode 2>&1
```

Expected: all 4 tests pass.

- [ ] **Step 3.4: Commit**

```bash
cd .worktrees/feat/resize-data
git add src/mode/mod.rs src/mode/first_boot.rs src/lib.rs
git commit -m "feat(mode): add BootMode::FirstBoot variant and detection logic

- BootMode::FirstBoot returned when omnect_resized_data guard is absent
  and a live bootloader is available; falls back to Normal on degraded boot
- Stub mode::first_boot::run() scaffolds the dispatch chain
- lib.rs match arm added for FirstBoot

Signed-off-by: $(git config user.name) <$(git config user.email)>"
```

---

## Task 4: Move `resize_data.rs` to `filesystem/`

**Files:**
- Create: `src/filesystem/resize_data.rs`
- Delete: `src/mode/resize_data.rs`
- Modify: `src/filesystem/mod.rs`
- Modify: `src/mode/first_boot.rs`
- Modify: `src/mode/normal.rs`

- [ ] **Step 4.1: Create `src/filesystem/resize_data.rs`**

Copy all content from `src/mode/resize_data.rs` into `src/filesystem/resize_data.rs`,
then apply these changes:

**a) Update the module doc comment** (first 6 lines):

```rust
//! Data partition auto-resize
//!
//! Expands the data partition and its ext4 filesystem to fill available disk
//! space on first boot. Called from `mode::first_boot` on every first boot
//! (the caller guarantees the resize guard is absent before invoking this).
```

**b) Remove `BootloaderEnvKey` from the imports** — it's no longer needed here:

```rust
use crate::bootloader::Bootloader;
use crate::error::{ResizeDataError, Result};
use crate::partition::PartitionName;
```

**c) Change the signature of `resize_if_needed`** — remove the `Option` wrapper
(the caller guarantees a live bootloader):

```rust
pub fn resize_if_needed(
    layout: &crate::partition::PartitionLayout,
    bootloader: &mut Box<dyn Bootloader>,
) -> Result<()> {
```

**d) Remove the two early returns** at the top of `resize_if_needed`. Replace the
entire block from `let Some(ref mut bl)` through the guard check with just:

```rust
    let bl = bootloader.as_mut();
```

So the function body starts as:

```rust
pub fn resize_if_needed(
    layout: &crate::partition::PartitionLayout,
    bootloader: &mut Box<dyn Bootloader>,
) -> Result<()> {
    let data_dev = match layout.partitions.get(&PartitionName::Data) {
        Some(d) => d.clone(),
        None => {
            log::warn!("Data partition not found in layout; skipping resize");
            return Ok(());
        }
    };
```

And all subsequent uses of `bl.` become `bootloader.` (since we no longer shadow it).
Specifically, at the end of the function:

```rust
    bootloader.set_env(BootloaderEnvKey::ResizedData, Some("1"))?;
```

Wait — `BootloaderEnvKey` is still needed for `set_env`. Add it back to the import:

```rust
use crate::bootloader::{Bootloader, BootloaderEnvKey};
use crate::error::{ResizeDataError, Result};
use crate::partition::PartitionName;
```

**e) Update the test for `test_guard_does_not_set_flag_on_missing_data_partition`**:

The guard check is gone; this test now verifies that a missing data partition causes an
early Ok return without setting the flag. Update to use `Box<dyn Bootloader>` directly:

```rust
    #[test]
    fn test_resize_skips_when_data_partition_absent() {
        use crate::bootloader::MockBootloader;
        use crate::partition::{PartitionLayout, RootDevice};
        use std::collections::HashMap;

        // Empty layout — no data partition present.
        let layout = PartitionLayout {
            partitions: HashMap::new(),
            device: RootDevice {
                base: std::path::PathBuf::from("/dev/sda"),
                partition_sep: "",
                root_partition: std::path::PathBuf::from("/dev/sda2"),
            },
        };
        let mut bl: Box<dyn crate::bootloader::Bootloader> = Box::new(MockBootloader::new());

        // Returns Ok early with a warning — no real resize attempted.
        assert!(resize_if_needed(&layout, &mut bl).is_ok());
        // Flag must NOT be set because no resize was performed.
        assert!(bl
            .get_env(BootloaderEnvKey::ResizedData)
            .unwrap()
            .is_none());
    }
```

**f) Remove `test_guard_skips_when_already_resized`** entirely. That test verified the
guard check that has now moved to `BootMode::detect()`. The equivalent coverage lives
in `mode::tests::detect_normal_when_guard_present`.

- [ ] **Step 4.2: Delete `src/mode/resize_data.rs`**

```bash
cd .worktrees/feat/resize-data
git rm src/mode/resize_data.rs
```

- [ ] **Step 4.3: Add module to `src/filesystem/mod.rs`**

Add before the `pub use` block:

```rust
#[cfg(feature = "resize-data")]
pub mod resize_data;
```

And add a re-export at the end of the `pub use` block:

```rust
#[cfg(feature = "resize-data")]
pub use self::resize_data::resize_if_needed;
```

- [ ] **Step 4.4: Update `src/mode/first_boot.rs`**

Replace the stub with the real implementation:

```rust
use crate::{Result, mode::BootContext};

pub fn run(mut ctx: BootContext<'_>) -> Result<()> {
    // Safety: FirstBoot is only dispatched when a live bootloader was detected.
    let bl = ctx
        .bootloader
        .as_mut()
        .expect("FirstBoot requires a live bootloader");
    crate::filesystem::resize_data::resize_if_needed(ctx.layout, bl)?;
    crate::mode::normal::run(ctx)
}
```

- [ ] **Step 4.5: Update `src/mode/normal.rs`**

Remove the `#[cfg(feature = "resize-data")]` call to resize:

```rust
pub fn run(ctx: BootContext<'_>) -> Result<()> {
    let BootContext {
        config,
        layout,
        rootfs,
        mut bootloader,
        mut ods_status,
    } = ctx;

    // Capture the result so we can persist fsck diagnostics before propagating
    // a mount failure.
    let mount_result = mount_remaining_partitions(layout, rootfs, &mut ods_status);
    // ...rest unchanged
```

- [ ] **Step 4.6: Verify compilation**

```bash
cd .worktrees/feat/resize-data
cargo check --features grub,gpt,resize-data 2>&1
```

Expected: compiles cleanly (no errors).

- [ ] **Step 4.7: Run full tests for grub+gpt+resize-data**

```bash
cd .worktrees/feat/resize-data
cargo test --features grub,gpt,resize-data 2>&1
```

Expected: all tests pass.

- [ ] **Step 4.8: Commit**

```bash
cd .worktrees/feat/resize-data
git add src/filesystem/ src/mode/ src/lib.rs
git commit -m "refactor(resize-data): move resize_data to filesystem/, introduce first_boot mode

- resize_data.rs relocated from mode/ to filesystem/ (disk transformation,
  not a mode handler)
- resize_if_needed() signature tightened to &mut Box<dyn Bootloader>
  (caller guarantees live bootloader via BootMode::FirstBoot dispatch)
- Guard check and bootloader-None early return removed from resize_if_needed();
  both concerns handled by BootMode::detect() and the dispatch chain
- mode::first_boot::run() implemented: calls resize then delegates to normal
- mode::normal::run() no longer contains the resize-data cfg gate

Signed-off-by: $(git config user.name) <$(git config user.email)>"
```

---

## Task 5: Run full test matrix and lint

- [ ] **Step 5.1: Run all four feature combinations**

```bash
cd .worktrees/feat/resize-data
cargo test --features grub,gpt,resize-data 2>&1 | tail -5
cargo test --features grub,dos,resize-data 2>&1 | tail -5
cargo test --features uboot,gpt,resize-data 2>&1 | tail -5
cargo test --features uboot,dos,resize-data 2>&1 | tail -5
```

Expected for each: `test result: ok. N passed; 0 failed`.

- [ ] **Step 5.2: Clippy on all combinations**

```bash
cd .worktrees/feat/resize-data
cargo clippy --tests --features grub,gpt,resize-data -- -D warnings 2>&1 | tail -10
cargo clippy --tests --features grub,dos,resize-data -- -D warnings 2>&1 | tail -10
cargo clippy --tests --features uboot,gpt,resize-data -- -D warnings 2>&1 | tail -10
cargo clippy --tests --features uboot,dos,resize-data -- -D warnings 2>&1 | tail -10
```

Expected: no warnings or errors.

- [ ] **Step 5.3: Format check**

```bash
cd .worktrees/feat/resize-data
cargo fmt -- --check 2>&1
```

Expected: no output (clean).
