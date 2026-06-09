# First-Boot Detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the two separate "this is the first boot" markers (`omnect_resized_data` + implicit upper-dir-empty check) with a single unified `omnect_first_boot_done` boot-env key. Surface `OdsStatus.first_boot: bool` so both later initramfs steps and ODS post-boot see the same value.

**Architecture:** Add a new `BootEnvKey::FirstBootDone` variant; remove `BootEnvKey::ResizedData`. Detection runs once in `run_init` after the boot env is opened; the value is stored on `OdsStatus.first_boot`. The marker is written once at the end of a successful boot, just before `switch_root` in `mode::normal::run`. Resize-data's preflight checks the new key; its internal `write_resize_guard` helper is removed (Plan D owns the single sentinel write).

**Tech Stack:** Rust 2024. No new external dependencies.

**Spec:** `docs/superpowers/specs/2026-05-27-first-boot-detection-design.md`.

**Dependencies:** Independent of Plans A and C. Coordinates with Plan B by replacing the env key the preflight reads. May land before or after Plan B; the two plans intentionally split the resize work so the file overlap is minimal.

---

## File map

| Path | Action | Responsibility |
|---|---|---|
| `src/bootloader/mod.rs` | Modify | Add `BootEnvKey::FirstBootDone` variant and `as_str()` arm. Remove `BootEnvKey::ResizedData` variant and arm (last task). |
| `src/runtime/omnect_device_service.rs` | Modify | Add `first_boot: bool` field on `OdsStatus`; always serialized. |
| `src/lib.rs` | Modify | After `apply_boot_env_decision`, compute `first_boot` from the boot env and store on `OdsStatus`. |
| `src/mode/normal.rs` | Modify | Before `switch_root`, write the `FirstBootDone` marker (best-effort) when `ods_status.first_boot` is true. |
| `src/preflight/resize_data.rs` | Modify | Read `BootEnvKey::FirstBootDone` (presence = "not first boot, skip") instead of `BootEnvKey::ResizedData`. Update tests. |
| `src/filesystem/resize_data.rs` | Modify | Remove the `write_resize_guard` helper and its caller. The Plan-D writer in `mode::normal::run` owns the marker write. |

---

## Task 1: Add `BootEnvKey::FirstBootDone` (additive — keep `ResizedData` for now)

**Files:**
- Modify: `src/bootloader/mod.rs`

We add the new variant first so all readers can move to it before the old variant is removed. This avoids a transient broken state between commits.

- [ ] **Step 1.1: Add a failing test**

Append to the existing tests in `src/bootloader/mod.rs`:

```rust
    #[test]
    fn first_boot_done_key_string() {
        // Pin the wire string. ODS / cloud / external tools may match on it
        // so changing it would be a wire-format break.
        assert_eq!(
            BootEnvKey::FirstBootDone.as_str().as_ref(),
            "omnect_first_boot_done"
        );
    }
```

- [ ] **Step 1.2: Verify it fails to compile**

Run: `cargo test --lib bootloader::tests::first_boot_done_key_string 2>&1 | head -20`
Expected: compile error `no variant named FirstBootDone for enum BootEnvKey`.

- [ ] **Step 1.3: Add the variant and its `as_str` arm**

In `src/bootloader/mod.rs`, find the `BootEnvKey` enum:

```rust
pub enum BootEnvKey {
    /// `omnect_validate_update` — OTA update validation state.
    ValidateUpdate,
    /// `omnect_bootloader_updated` — whether the bootloader itself was updated.
    BootloaderUpdated,
    /// `omnect_fsck_<partition>` — fsck result for the given partition.
    FsckStatus(PartitionName),
    /// `omnect_resized_data` — set to `"1"` after data partition has been resized.
    #[cfg(feature = "resize-data")]
    ResizedData,
}
```

Add a new variant (keep `ResizedData` for now):

```rust
pub enum BootEnvKey {
    /// `omnect_validate_update` — OTA update validation state.
    ValidateUpdate,
    /// `omnect_bootloader_updated` — whether the bootloader itself was updated.
    BootloaderUpdated,
    /// `omnect_fsck_<partition>` — fsck result for the given partition.
    FsckStatus(PartitionName),
    /// `omnect_resized_data` — legacy first-boot marker; removed in this plan
    /// after all readers switch to `FirstBootDone`.
    #[cfg(feature = "resize-data")]
    ResizedData,
    /// `omnect_first_boot_done` — set to `"1"` after the first successful
    /// `run_init`. Unified replacement for `ResizedData`. Read by both the
    /// resize-data preflight (Plan D §6.1) and the first-boot detection
    /// in `run_init` (Plan D §3).
    FirstBootDone,
}
```

Add the matching `as_str` arm:

```rust
impl BootEnvKey {
    pub fn as_str(&self) -> Cow<'static, str> {
        match self {
            Self::ValidateUpdate => Cow::Borrowed("omnect_validate_update"),
            Self::BootloaderUpdated => Cow::Borrowed("omnect_bootloader_updated"),
            Self::FsckStatus(p) => Cow::Owned(format!("omnect_fsck_{p}")),
            #[cfg(feature = "resize-data")]
            Self::ResizedData => Cow::Borrowed("omnect_resized_data"),
            Self::FirstBootDone => Cow::Borrowed("omnect_first_boot_done"),
        }
    }
}
```

- [ ] **Step 1.4: Run the test**

Run: `cargo test --lib bootloader::tests::first_boot_done_key_string`
Expected: 1 passed.

- [ ] **Step 1.5: Check clippy + format**

Run: `cargo clippy --all-features --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 1.6: Commit**

```bash
git add src/bootloader/mod.rs
git commit -s -m "feat(bootloader): add BootEnvKey::FirstBootDone variant

Additive change ahead of the unified-marker swap. ResizedData stays
for now so existing readers continue to work; it is removed in a
later commit once nothing reads or writes it.

Spec: docs/superpowers/specs/2026-05-27-first-boot-detection-design.md §2.1"
```

---

## Task 2: Add `OdsStatus.first_boot` field

**Files:**
- Modify: `src/runtime/omnect_device_service.rs`

- [ ] **Step 2.1: Add a failing serialization test**

Append to the existing `#[cfg(test)] mod tests` in `src/runtime/omnect_device_service.rs`:

```rust
    #[test]
    fn first_boot_always_serialized() {
        // Plain bool, always in the JSON. Absence of the key would itself
        // be diagnostic of a bug — see spec §7.
        let s = OdsStatus::new();
        let j = serde_json::to_string(&s).unwrap();
        assert!(
            j.contains("\"first_boot\":false"),
            "default first_boot must be false and serialized; got: {j}"
        );

        let mut s = OdsStatus::new();
        s.first_boot = true;
        let j = serde_json::to_string(&s).unwrap();
        assert!(
            j.contains("\"first_boot\":true"),
            "first_boot=true must be serialized; got: {j}"
        );
    }
```

- [ ] **Step 2.2: Verify it fails to compile**

Run: `cargo test --lib omnect_device_service::tests::first_boot 2>&1 | head -20`
Expected: compile error `no field first_boot on type OdsStatus`.

- [ ] **Step 2.3: Add the field**

In `src/runtime/omnect_device_service.rs`, modify the `OdsStatus` struct to add (e.g. after the `degraded_boot` field):

```rust
    /// `true` iff this boot is the first boot since flashing (i.e. the
    /// `omnect_first_boot_done` marker was absent at run_init time).
    /// Always serialized — absence of the key would itself be a bug.
    pub first_boot: bool,
```

`first_boot: bool` will default to `false` via the existing `#[derive(Default)]` on `OdsStatus`. No change to `OdsStatus::new` is needed.

- [ ] **Step 2.4: Run the test**

Run: `cargo test --lib omnect_device_service::tests::first_boot`
Expected: 1 passed.

- [ ] **Step 2.5: Run the full suite to make sure no test broke**

Run: `cargo test --lib`
Expected: all tests pass.

- [ ] **Step 2.6: Check clippy + format**

Run: `cargo clippy --all-features --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 2.7: Commit**

```bash
git add src/runtime/omnect_device_service.rs
git commit -s -m "feat(ods): add OdsStatus.first_boot field

Plain bool, always serialized. Default false. Populated by run_init
after the boot env is opened; consumed by ODS post-boot and by the
marker writer in mode::normal::run (later commits).

Spec: docs/superpowers/specs/2026-05-27-first-boot-detection-design.md §7"
```

---

## Task 3: Detect first boot in `run_init`

**Files:**
- Modify: `src/lib.rs`

Detection runs immediately after `apply_boot_env_decision` returns the `BootEnvState`. The value is stored on `OdsStatus.first_boot`.

- [ ] **Step 3.1: Add a failing test**

Append a test module to `src/lib.rs` (or extend the existing `#[cfg(test)]` block):

```rust
#[cfg(test)]
mod first_boot_detection_tests {
    use super::*;
    use crate::bootloader::{BootEnv, BootEnvKey, BootEnvState, MockBootEnv};
    use crate::error::BootEnvError;

    #[test]
    fn marker_absent_yields_first_boot_true() {
        let env: BootEnvState = BootEnvState::Available(Box::new(MockBootEnv::new()));
        assert!(compute_first_boot(&env));
    }

    #[test]
    fn marker_present_yields_first_boot_false() {
        let mock = MockBootEnv::new().with_env(BootEnvKey::FirstBootDone, "1");
        let env: BootEnvState = BootEnvState::Available(Box::new(mock));
        assert!(!compute_first_boot(&env));
    }

    #[test]
    fn degraded_env_yields_first_boot_false() {
        // Degraded default per spec §4: don't trigger first-boot side
        // effects (cloud registration etc.) under uncertainty.
        let env = BootEnvState::Degraded(BootEnvError::CommandFailed {
            command: "grub-editenv".into(),
            reason: "test".into(),
        });
        assert!(!compute_first_boot(&env));
    }
}
```

- [ ] **Step 3.2: Verify it fails to compile**

Run: `cargo test --lib first_boot_detection_tests 2>&1 | head -20`
Expected: `cannot find function compute_first_boot in this scope`.

- [ ] **Step 3.3: Add the helper**

Near the existing `set_update_pending` / `read_update_pending` helpers in `src/lib.rs` (or just below `apply_boot_env_decision`), add:

```rust
/// Compute the first-boot flag from the opened boot env.
///
/// - `Available(bl)` → `true` iff `BootEnvKey::FirstBootDone` is **absent**
///   from the env. A `get_env` error that isn't "absent" is conservatively
///   treated as "present" (first_boot = false) to avoid re-running first-boot
///   side effects under uncertainty.
/// - `Degraded(_)` → `false`. Degraded-mode default per spec §4.
fn compute_first_boot(env: &bootloader::BootEnvState) -> bool {
    match env.available() {
        Some(bl) => match bl.get_env(bootloader::BootEnvKey::FirstBootDone) {
            Ok(None) => true,
            Ok(Some(_)) => false,
            Err(e) => {
                log::warn!(
                    "first-boot: get_env failed: {e}; treating as not-first-boot"
                );
                false
            }
        },
        None => false,
    }
}
```

`compute_first_boot` is module-private (`fn`, not `pub fn`) since `run_init` is the only caller. The tests in step 3.1 access it via the `super::*` import.

- [ ] **Step 3.4: Wire the helper into `run_init`**

In `src/lib.rs::run_init`, find the line we added in Plan A (or that exists today):

```rust
    let mut bootloader_env =
        apply_boot_env_decision(decision, core_result, &mut ods_status, rootfs)?;
```

Immediately *after* it (and before the `update_pending` block, if Plan A is in), add:

```rust
    ods_status.first_boot = compute_first_boot(&bootloader_env);
    if ods_status.first_boot {
        info!("first-boot detected (omnect_first_boot_done absent)");
    }
```

- [ ] **Step 3.5: Run the tests**

Run: `cargo test --lib first_boot_detection_tests`
Expected: 3 passed.

Run: `cargo test --lib`
Expected: all tests pass.

- [ ] **Step 3.6: Clippy + format**

Run: `cargo clippy --all-features --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 3.7: Commit**

```bash
git add src/lib.rs
git commit -s -m "feat(lib): detect first boot from FirstBootDone marker

Add compute_first_boot(env) and call it in run_init after
apply_boot_env_decision. Result stored on OdsStatus.first_boot for
later initramfs steps (resize preflight in a follow-up commit) and
for ODS post-boot. Degraded env yields false (spec §4); get_env error
treated as 'not first boot' defensively.

Spec: docs/superpowers/specs/2026-05-27-first-boot-detection-design.md §3-§4"
```

---

## Task 4: Write the marker at end of successful boot

**Files:**
- Modify: `src/mode/normal.rs`

The marker is written once, just before `switch_root` returns control to userspace. Best-effort: a write failure is logged and does not abort the boot.

- [ ] **Step 4.1: Read the current shape of `mode::normal::run`**

Open `src/mode/normal.rs`. The function ends with:

```rust
    create_ods_runtime_files(
        &ods_status,
        bootloader.as_deref(),
        rootfs,
        Path::new(ODS_RUNTIME_DIR),
    )?;

    info!("omnect-os-initramfs completed successfully");

    switch_root(rootfs, &config.cmdline)
```

(The exact variable name for the env is `bootloader` in older trees, or `boot_env` after the BootEnv rename — check the actual file.)

- [ ] **Step 4.2: Add the marker-write block before `switch_root`**

Replace the trailing block of `run` with:

```rust
    create_ods_runtime_files(
        &ods_status,
        bootloader.as_deref(),
        rootfs,
        Path::new(ODS_RUNTIME_DIR),
    )?;

    // Single point of truth: set the first-boot marker iff this was the
    // first boot AND the boot env is writable. Best-effort: a write
    // failure is logged and does not abort the boot (the work has
    // succeeded; the next boot will retry the set).
    if ods_status.first_boot
        && let Some(bl) = bootloader.as_deref_mut()
        && let Err(e) = bl.set_env(crate::bootloader::BootEnvKey::FirstBootDone, Some("1"))
    {
        log::warn!("first-boot marker write failed: {e}; will retry next boot");
    }

    info!("omnect-os-initramfs completed successfully");

    switch_root(rootfs, &config.cmdline)
```

Notes on the variable names and types:
- The `BootContext` destructure at the top of `run` is `let BootContext { config, layout, rootfs, mut bootloader, mut ods_status } = ctx;` — the `mut bootloader` binding is `Box<dyn BootEnv>` wrapped in whatever the current tree uses (Option / BootEnvState). The exact spelling of `bootloader.as_deref_mut()` may need to be `bootloader.available_mut()` if the field is `BootEnvState` rather than `Option<Box<dyn BootEnv>>`. Adapt to match `mode::mod.rs`'s `BootContext` definition.
- If `BootContext.bootloader` is `BootEnvState`, use `bootloader.available_mut()` (returns `Option<&mut dyn BootEnv>`). Either way, the goal is "get a `&mut dyn BootEnv` if the env is available; do nothing if degraded."

- [ ] **Step 4.3: Make sure `ods_status` is read after the marker-write**

Verify `create_ods_runtime_files(&ods_status, ...)` is called *before* the marker write. The marker write only reads `ods_status.first_boot`, so the order does not actually matter for correctness — but the existing ordering should be preserved for diff-readability.

- [ ] **Step 4.4: Add a regression test exercising the writer**

This test is non-trivial because `mode::normal::run` ends with `switch_root` (which exits the process or fails). Instead of testing `run` end-to-end, factor the marker write into a private helper:

```rust
fn write_first_boot_marker(
    first_boot: bool,
    bootloader: &mut crate::bootloader::BootEnvState,
) {
    if first_boot
        && let Some(bl) = bootloader.available_mut()
        && let Err(e) = bl.set_env(crate::bootloader::BootEnvKey::FirstBootDone, Some("1"))
    {
        log::warn!("first-boot marker write failed: {e}; will retry next boot");
    }
}
```

And call it from `run`:

```rust
    write_first_boot_marker(ods_status.first_boot, &mut bootloader);
```

(Match the actual `BootContext` field name — `bootloader` vs `boot_env`.)

Add tests at the bottom of `src/mode/normal.rs`:

```rust
#[cfg(test)]
mod marker_writer_tests {
    use super::*;
    use crate::bootloader::{BootEnvKey, BootEnvState, MockBootEnv};
    use crate::error::BootEnvError;

    #[test]
    fn writes_marker_when_first_boot() {
        let mock = MockBootEnv::new();
        let mut env = BootEnvState::Available(Box::new(mock));
        write_first_boot_marker(true, &mut env);
        let bl = env.available().unwrap();
        assert_eq!(
            bl.get_env(BootEnvKey::FirstBootDone).unwrap(),
            Some("1".to_string())
        );
    }

    #[test]
    fn does_not_write_when_not_first_boot() {
        let mock = MockBootEnv::new();
        let mut env = BootEnvState::Available(Box::new(mock));
        write_first_boot_marker(false, &mut env);
        let bl = env.available().unwrap();
        assert_eq!(bl.get_env(BootEnvKey::FirstBootDone).unwrap(), None);
    }

    #[test]
    fn no_op_on_degraded_env() {
        let mut env = BootEnvState::Degraded(BootEnvError::CommandFailed {
            command: "grub-editenv".into(),
            reason: "test".into(),
        });
        // Must not panic; nothing to assert beyond completion.
        write_first_boot_marker(true, &mut env);
    }
}
```

- [ ] **Step 4.5: Build and run tests**

Run: `cargo test --lib mode::normal`
Expected: 3 passed.

Run: `cargo test --lib`
Expected: all tests pass.

- [ ] **Step 4.6: Clippy + format**

Run: `cargo clippy --all-features --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 4.7: Commit**

```bash
git add src/mode/normal.rs
git commit -s -m "feat(normal-mode): write FirstBootDone marker before switch_root

Single writer for the unified first-boot sentinel. Skipped when
ods_status.first_boot is false (already set) or the boot env is
degraded. Best-effort: write failure is logged, boot continues — the
next boot will retry the set. Factored into write_first_boot_marker
so the side-effect is unit-testable without running switch_root.

Spec: docs/superpowers/specs/2026-05-27-first-boot-detection-design.md §5"
```

---

## Task 5: Switch resize-data preflight to read `FirstBootDone`

**Files:**
- Modify: `src/preflight/resize_data.rs`

After this task, the preflight uses the unified key. The resize logic in `filesystem::resize_data::resize_if_needed` still writes the legacy `BootEnvKey::ResizedData` guard — Task 6 removes that write. Order is intentional: switch readers first, then remove writers.

- [ ] **Step 5.1: Find the existing key read**

In `src/preflight/resize_data.rs::run`, the early-return check is:

```rust
    if let Some(bl) = ctx.boot_env.available_mut()
        && bl.get_env(BootEnvKey::ResizedData)?.is_some()
    {
        log::debug!("resize-data preflight: guard present; already resized");
        return Ok(());
    }
```

(If Plan B has already landed, this lives inside its wrapper; if not, it's near the top of `run`. The exact location is unchanged either way.)

- [ ] **Step 5.2: Replace with `FirstBootDone`**

Replace the `BootEnvKey::ResizedData` reference with `BootEnvKey::FirstBootDone`:

```rust
    if let Some(bl) = ctx.boot_env.available_mut()
        && bl.get_env(BootEnvKey::FirstBootDone)?.is_some()
    {
        log::debug!("resize-data preflight: first-boot marker present; skipping resize");
        return Ok(());
    }
```

- [ ] **Step 5.3: Update tests that reference `BootEnvKey::ResizedData`**

In the test block of `src/preflight/resize_data.rs`, replace each occurrence of:

```rust
MockBootEnv::new().with_env(BootEnvKey::ResizedData, "1")
```

with:

```rust
MockBootEnv::new().with_env(BootEnvKey::FirstBootDone, "1")
```

And update assertions that check `BootEnvKey::ResizedData` is present to instead check `BootEnvKey::FirstBootDone`. Run `grep -n ResizedData src/preflight/resize_data.rs` to find all of them.

- [ ] **Step 5.4: Run tests**

Run: `cargo test --lib --features resize-data preflight::resize_data`
Expected: all tests pass.

- [ ] **Step 5.5: Clippy + format**

Run: `cargo clippy --all-features --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 5.6: Commit**

```bash
git add src/preflight/resize_data.rs
git commit -s -m "feat(preflight): resize-data reads unified FirstBootDone marker

Switch the early-return guard check from BootEnvKey::ResizedData to
BootEnvKey::FirstBootDone. The semantics flip: today 'guard absent ->
run resize', after Plan D 'first-boot true -> run resize'. The
filesystem::resize_data::resize_if_needed implementation still writes
the legacy ResizedData guard for one more commit; removal follows.

Spec: docs/superpowers/specs/2026-05-27-first-boot-detection-design.md §6.1"
```

---

## Task 6: Remove the resize guard write

**Files:**
- Modify: `src/filesystem/resize_data.rs`

`resize_if_needed` currently calls `write_resize_guard` at the end of a successful resize. With the unified marker owned by Plan D's writer in `mode::normal::run`, this call is removed.

- [ ] **Step 6.1: Find the call**

In `src/filesystem/resize_data.rs::resize_if_needed`, the bottom of the function is:

```rust
    run_cmd(SYNC_CMD, &[])?;

    write_resize_guard(bootloader)?;

    log::info!("Data partition resize complete");
    Ok(())
```

- [ ] **Step 6.2: Remove the call**

Replace those lines with:

```rust
    run_cmd(SYNC_CMD, &[])?;

    log::info!("Data partition resize complete");
    Ok(())
```

The `bootloader` parameter on `resize_if_needed` may become unused depending on the rest of the function. If so, prefix the param name with `_`:

```rust
pub fn resize_if_needed(
    layout: &crate::partition::PartitionLayout,
    _bootloader: Option<&mut dyn BootEnv>,
) -> Result<()> {
```

(Don't remove the parameter from the signature — preflight still passes it in and removing it ripples to that file too. The underscore prefix is enough to silence the warning.)

- [ ] **Step 6.3: Remove the now-orphaned `write_resize_guard` function**

Find this helper in `src/filesystem/resize_data.rs`:

```rust
pub(crate) fn write_resize_guard(bootloader: Option<&mut dyn BootEnv>) -> Result<()> {
    if let Some(bl) = bootloader
        && let Err(e) = bl.set_env(BootEnvKey::ResizedData, Some("1"))
    {
        log::warn!(
            "data partition resize completed but guard write failed: {e}; \
             resize will run again on next boot (idempotent)"
        );
    }
    Ok(())
}
```

Remove it entirely. Also remove the `BootEnvKey` import if it is no longer used in this file:

Run: `grep -n "BootEnvKey" src/filesystem/resize_data.rs`
If no hits remain after removing `write_resize_guard`, remove the `use crate::bootloader::{BootEnv, BootEnvKey};` import and replace with `use crate::bootloader::BootEnv;`.

- [ ] **Step 6.4: Update tests that exercised `write_resize_guard` or asserted on `BootEnvKey::ResizedData`**

In the test block of `src/filesystem/resize_data.rs`, find the `test_resize_skips_when_data_partition_absent` test. The assertion line:

```rust
        assert!(bl.get_env(BootEnvKey::ResizedData).unwrap().is_none());
```

Remove it (or change to `BootEnvKey::FirstBootDone` if a follow-up wants to assert the marker is *not* set here). Recommended: remove — the test's purpose is "doesn't panic when data partition absent," not guard-write assertions.

Run: `grep -n "ResizedData\|write_resize_guard" src/filesystem/resize_data.rs`
Expected: no remaining hits.

- [ ] **Step 6.5: Build and run tests**

Run: `cargo build`
Expected: clean build.

Run: `cargo test --lib --features resize-data`
Expected: all tests pass.

Run: `cargo test --lib` (default features — no resize-data; tests cfg-gated away).
Expected: all tests pass.

- [ ] **Step 6.6: Clippy + format**

Run: `cargo clippy --all-features --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 6.7: Commit**

```bash
git add src/filesystem/resize_data.rs
git commit -s -m "refactor(resize-data): remove resize guard write

The unified first-boot marker (BootEnvKey::FirstBootDone) is written
by mode::normal::run at end of successful boot regardless of resize
outcome. The dedicated omnect_resized_data guard is no longer needed.

resize_if_needed's bootloader parameter is kept (preflight still
passes it for symmetry) but unused; prefixed with underscore.

Spec: docs/superpowers/specs/2026-05-27-first-boot-detection-design.md §6.1"
```

---

## Task 7: Remove `BootEnvKey::ResizedData` variant

**Files:**
- Modify: `src/bootloader/mod.rs`

Final cleanup. No reader, no writer references `BootEnvKey::ResizedData` after Tasks 5–6; the variant can be removed.

- [ ] **Step 7.1: Confirm no remaining references**

```bash
grep -rn "BootEnvKey::ResizedData\|ResizedData" src/
```

Expected: only the variant definition itself in `src/bootloader/mod.rs` and possibly references in test files we missed. If anything outside `src/bootloader/mod.rs` matches, fix that file first.

- [ ] **Step 7.2: Remove the variant**

In `src/bootloader/mod.rs::BootEnvKey`, delete the `ResizedData` variant and its `as_str` arm:

```rust
    #[cfg(feature = "resize-data")]
    ResizedData,
```

```rust
    #[cfg(feature = "resize-data")]
    Self::ResizedData => Cow::Borrowed("omnect_resized_data"),
```

Both removed.

- [ ] **Step 7.3: Build**

Run: `cargo build --all-features`
Expected: clean. If anything still references `ResizedData`, the build fails — go back and fix the offender.

- [ ] **Step 7.4: Full test suite**

Run: `cargo test --all-features`
Expected: all tests pass.

- [ ] **Step 7.5: Clippy + format**

Run: `cargo clippy --all-features --all-targets -- -D warnings && cargo fmt -- --check`
Expected: clean.

- [ ] **Step 7.6: Commit**

```bash
git add src/bootloader/mod.rs
git commit -s -m "refactor(bootloader): remove BootEnvKey::ResizedData

No remaining reader or writer references the variant. Field devices
that still have omnect_resized_data set in their boot env are
unaffected — the value is simply unread.

Spec: docs/superpowers/specs/2026-05-27-first-boot-detection-design.md §2.1"
```

---

## Final verification

- [ ] **Step F.1: Full test suite across feature combinations**

```bash
cargo test --all-features
cargo test --no-default-features --features core,grub,gpt
cargo test --no-default-features --features core,uboot,dos
```

Expected: every combination passes.

- [ ] **Step F.2: Clippy + format**

```bash
cargo clippy --all-features --all-targets -- -D warnings
cargo fmt -- --check
```

Expected: clean exit.

- [ ] **Step F.3: Sanity-check the wire format**

```bash
grep -rn "omnect_first_boot_done\|omnect_resized_data" src/
```

Expected: exactly **one** hit for `omnect_first_boot_done` (in `BootEnvKey::as_str`); **zero** hits for `omnect_resized_data` (after Task 7).

- [ ] **Step F.4: JSON spot-check**

In a scratch test confirm:

```rust
let s = OdsStatus::new();
let j = serde_json::to_string(&s).unwrap();
assert!(j.contains("\"first_boot\":false"));
```

Already covered by the test added in Task 2.

---

## Out of scope (follow-ups)

- **Atomicity of `setup_etc_overlay`.** The etc-copy still uses the "upper empty" check; a power failure mid-copy still leaves a partially populated `/etc`. A separate plan migrates `setup_etc_overlay` to read `OdsStatus.first_boot` and reorders the copy → `sync` → marker-write so the marker is only set after a durable copy. The Plan-D writer in Task 4 will naturally pick up the corrected ordering at that time (no code change in this plan).
- **Factory-reset survival.** Design assumes factory-reset will not wipe the boot env. Revisit when factory-reset is specified.
- **OTA-upgrade transitional behavior.** Devices that OTA from an older initramfs see no `omnect_first_boot_done`, run first-boot work (idempotent etc-copy, idempotent resize), and then set the marker. Accepted; no migration code.
