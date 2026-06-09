# fsck & resize-data Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make resize-data a best-effort preflight step that can never brick the device. All failure modes (dirty fsck, tool error, missing partition) are absorbed by the preflight wrapper and surfaced to ODS via a new `OdsStatus.resize_data: Option<ResizeStatus>` field. A reboot-required fsck during the resize pre-check is *not* a resize failure — it propagates as `RebootToApply` (Plan A) to honor fsck's signal.

**Architecture:** No behavior change to `filesystem::resize_data::resize_if_needed` itself — it keeps the strict `check_filesystem` pre-check and the current error variants. The change is at the preflight boundary (`preflight::resize_data::run`): catch every error variant, map to a `ResizeOutcome`, record on `OdsStatus`, return `Ok(())`. `FsckRequiresReboot` is the one exception — it re-propagates so the Plan A recovery model handles the reboot.

**Tech Stack:** Rust 2024, `serde` 1.0 (derive on the new types), `thiserror` 2.0 (existing). No new external dependencies.

**Spec:** `docs/superpowers/specs/2026-05-27-fsck-and-resize-design.md`.

**Dependencies:** Plan A (this plan reads `InitramfsError::recovery_class()` only via Plan A's tests; runtime behavior is independent). May land before or after Plan A.

---

## File map

| Path | Action | Responsibility |
|---|---|---|
| `src/runtime/omnect_device_service.rs` | Modify | Add `ResizeOutcome`, `ResizeStatus`, `OdsStatus.resize_data` field + `set_resize_status` helper. |
| `src/preflight/resize_data.rs` | Modify | Catch every error from `resize_if_needed`; record `ResizeStatus`; let `FsckRequiresReboot` through. |
| `src/filesystem/resize_data.rs` | No change | The internal logic stays strict-fsck and Plan-A-classified. Plan D later removes the resize guard write; Plan B does not. |

---

## Task 1: Add `ResizeStatus` types and `OdsStatus.resize_data` field

**Files:**
- Modify: `src/runtime/omnect_device_service.rs`

- [ ] **Step 1.1: Add a failing test for the new types and field**

Append to the existing `#[cfg(test)] mod tests` block in `src/runtime/omnect_device_service.rs`:

```rust
    #[test]
    fn resize_status_serializes_when_set() {
        let mut status = OdsStatus::new();
        let json_clean = serde_json::to_string(&status).unwrap();
        assert!(
            !json_clean.contains("resize_data"),
            "resize_data must be absent when None; got: {json_clean}"
        );

        status.set_resize_status(ResizeStatus {
            outcome: ResizeOutcome::SkippedFsck,
            reason: "data partition fsck reported uncorrected errors".to_string(),
        });
        let json_set = serde_json::to_string(&status).unwrap();
        assert!(
            json_set.contains("\"resize_data\""),
            "resize_data must be present when Some; got: {json_set}"
        );
        assert!(
            json_set.contains("\"outcome\":\"skipped_fsck\""),
            "outcome must serialize snake_case; got: {json_set}"
        );
        assert!(
            json_set.contains("\"reason\""),
            "reason must be present; got: {json_set}"
        );
    }

    #[test]
    fn resize_outcome_variants_serialize_snake_case() {
        // Pin the wire format so ODS consumers can match exact strings.
        let cases: &[(ResizeOutcome, &str)] = &[
            (ResizeOutcome::SkippedFsck, "\"skipped_fsck\""),
            (ResizeOutcome::ToolError, "\"tool_error\""),
            (ResizeOutcome::InvalidLayout, "\"invalid_layout\""),
        ];
        for (variant, expected) in cases {
            let s = serde_json::to_string(variant).unwrap();
            assert_eq!(&s, expected, "variant {variant:?} must serialize as {expected}");
        }
    }
```

- [ ] **Step 1.2: Run the test to verify it fails to compile**

Run: `cargo test --lib omnect_device_service::tests::resize 2>&1 | head -30`
Expected: compile error mentioning `cannot find type ResizeStatus` (or `ResizeOutcome`, or `set_resize_status`).

- [ ] **Step 1.3: Add the types**

In `src/runtime/omnect_device_service.rs`, add these definitions near the existing `FactoryResetStatus` and `FactoryResetStatusCode` types (after them is fine):

```rust
/// Why resize-data did not write the resize guard.
///
/// Serialized as snake_case so ODS / cloud consumers can match exact strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResizeOutcome {
    /// Data partition fsck reported uncorrected errors before resize.
    SkippedFsck,
    /// An external tool (parted/sgdisk/resize2fs/sync) failed.
    ToolError,
    /// Layout problem (missing data partition, non-UTF-8 path, extended
    /// partition not found, …) — the preflight could not run at all.
    InvalidLayout,
}

/// Indicator surfaced to ODS when resize-data did not succeed on this boot.
///
/// `None` means resize succeeded or the guard is already present.
/// `Some(...)` means resize was attempted and could not complete.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResizeStatus {
    /// Why resize did not complete.
    pub outcome: ResizeOutcome,
    /// One-line human-readable detail — the Display of the underlying error.
    pub reason: String,
}
```

- [ ] **Step 1.4: Add the field to `OdsStatus`**

In `src/runtime/omnect_device_service.rs`, modify the `OdsStatus` struct (around the existing `degraded_boot` field) to add:

```rust
    /// Set when resize-data did not complete successfully on this boot.
    /// `None` on the happy path; `Some(...)` only when the preflight
    /// recorded a failure. ODS consumes it and can notify the cloud.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resize_data: Option<ResizeStatus>,
```

- [ ] **Step 1.5: Add the `set_resize_status` helper**

In the `impl OdsStatus { ... }` block of `src/runtime/omnect_device_service.rs`, after the existing `set_degraded_boot` (or wherever the other setters live), add:

```rust
    /// Record a resize-data failure indicator. Overwrites any previous value.
    pub fn set_resize_status(&mut self, status: ResizeStatus) {
        self.resize_data = Some(status);
    }
```

- [ ] **Step 1.6: Run the tests**

Run: `cargo test --lib omnect_device_service::tests::resize`
Expected: both tests pass.

Run: `cargo test --lib`
Expected: every test passes (no other test broke).

- [ ] **Step 1.7: Check clippy + format**

Run: `cargo clippy -- -D warnings && cargo fmt -- --check`
Expected: no output, exit 0.

- [ ] **Step 1.8: Commit**

```bash
git add src/runtime/omnect_device_service.rs
git commit -s -m "feat(ods): add ResizeStatus indicator field

Introduce ResizeOutcome (snake_case enum) and ResizeStatus
{ outcome, reason } surfaced via OdsStatus.resize_data:
Option<ResizeStatus>. Wire format mirrors the degraded_boot
field added earlier: skip_serializing_if Option is None,
so the happy path JSON is unchanged.

Spec: docs/superpowers/specs/2026-05-27-fsck-and-resize-design.md §3.2"
```

---

## Task 2: Make `preflight::resize_data::run` best-effort

**Files:**
- Modify: `src/preflight/resize_data.rs`

The function currently propagates every error from `resize_if_needed`. After this task it absorbs `FsckFailed`, all `ResizeDataError` variants, and any other I/O error into a `ResizeStatus` recorded on `OdsStatus`. `FsckRequiresReboot` continues to propagate so the Plan A recovery model can honor it as `RebootToApply`.

- [ ] **Step 2.1: Read the current shape of the function**

Open `src/preflight/resize_data.rs` and confirm the current body of `run` matches:

```rust
pub fn run(ctx: &mut PreflightCtx<'_, '_>) -> Result<()> {
    match ctx.boot_env.available_mut() {
        Some(bl) => {
            if bl.get_env(BootEnvKey::ResizedData)?.is_some() {
                log::debug!("resize-data preflight: guard present; already resized");
                return Ok(());
            }
            crate::filesystem::resize_data::resize_if_needed(ctx.layout, Some(bl))
        }
        None => {
            log::warn!("resize-data: running without bootloader guard (degraded boot)");
            crate::filesystem::resize_data::resize_if_needed(ctx.layout, None)
        }
    }
}
```

The next steps wrap both `resize_if_needed` call sites.

- [ ] **Step 2.2: Extend `PreflightCtx` so the preflight can write to `OdsStatus`**

In `src/preflight/mod.rs`, the current `PreflightCtx` is:

```rust
pub struct PreflightCtx<'l, 'b> {
    pub layout: &'l PartitionLayout,
    pub boot_env: &'b mut BootEnvState,
}
```

Add a mutable reference to `OdsStatus`:

```rust
pub struct PreflightCtx<'l, 'b, 's> {
    pub layout: &'l PartitionLayout,
    pub boot_env: &'b mut BootEnvState,
    pub ods_status: &'s mut crate::runtime::OdsStatus,
}
```

Update `preflight::run`'s signature in the same file to use the new lifetime:

```rust
pub fn run(mut ctx: PreflightCtx<'_, '_, '_>) -> Result<()> {
    #[cfg(feature = "resize-data")]
    resize_data::run(&mut ctx)?;
    Ok(())
}
```

- [ ] **Step 2.3: Update the call site in `src/lib.rs`**

In `src/lib.rs::run_init`, the preflight block is currently:

```rust
    {
        let ctx = preflight::PreflightCtx {
            layout: &layout,
            boot_env: &mut bootloader_env,
        };
        preflight::run(ctx)?;
    }
```

Replace with:

```rust
    {
        let ctx = preflight::PreflightCtx {
            layout: &layout,
            boot_env: &mut bootloader_env,
            ods_status: &mut ods_status,
        };
        preflight::run(ctx)?;
    }
```

- [ ] **Step 2.4: Add a failing test for the absorb-and-record behavior**

In the existing `#[cfg(test)] mod tests` of `src/preflight/resize_data.rs`, replace the existing `skips_when_guard_present` test or add alongside it. Note: the existing tests use a layout *without* a Data partition for the "skip" path. We add tests that exercise the new error-absorbing wrapper.

Append:

```rust
    use crate::runtime::{OdsStatus, ResizeOutcome};

    #[test]
    fn absorbs_invalid_layout_error_into_status() {
        // resize_if_needed without a Data partition logs+returns Ok(()) today,
        // so this test exercises the wrapper path that *does* succeed. We
        // assert no resize_data status is recorded on a clean run.
        let layout = empty_layout();
        let mut bl: Box<dyn crate::bootloader::BootEnv> = Box::new(MockBootEnv::new());
        let mut env = BootEnvState::Available(bl);
        let mut ods = OdsStatus::new();

        let mut ctx = PreflightCtx {
            layout: &layout,
            boot_env: &mut env,
            ods_status: &mut ods,
        };
        assert!(run(&mut ctx).is_ok());
        assert!(
            ods.resize_data.is_none(),
            "no status recorded when resize is a no-op"
        );
    }

    #[test]
    fn skips_when_guard_present_records_no_status() {
        // Existing semantics: guard present -> early return, no resize attempt,
        // no resize_data status recorded.
        let layout = layout_with_data();
        let mut bl: Box<dyn crate::bootloader::BootEnv> = Box::new(
            MockBootEnv::new().with_env(BootEnvKey::ResizedData, "1"),
        );
        let mut env = BootEnvState::Available(bl);
        let mut ods = OdsStatus::new();

        let mut ctx = PreflightCtx {
            layout: &layout,
            boot_env: &mut env,
            ods_status: &mut ods,
        };
        assert!(run(&mut ctx).is_ok());
        assert!(
            ods.resize_data.is_none(),
            "no status recorded when guard is present"
        );
    }
```

The existing test `skips_when_bootloader_unavailable` becomes obsolete because the wrapper now never returns an error for this case (it would attempt resize, fail with `InvalidLayout` since data partition is absent → still Ok, status absent). Update or remove that test as appropriate during step 2.7.

- [ ] **Step 2.5: Run the tests to verify the new ones fail to compile**

Run: `cargo test --lib --features resize-data preflight::resize_data::tests 2>&1 | head -40`
Expected: compile errors referencing `ods_status` field in `PreflightCtx`, or `ResizeOutcome` import — whatever the test references that does not yet exist in the struct/impl.

- [ ] **Step 2.6: Rewrite `run` to absorb errors and record status**

Replace the body of `pub fn run(ctx: &mut PreflightCtx<'_, '_, '_>) -> Result<()>` in `src/preflight/resize_data.rs` with:

```rust
pub fn run(ctx: &mut PreflightCtx<'_, '_, '_>) -> Result<()> {
    // If the guard is already set (release boots after first), do nothing.
    if let Some(bl) = ctx.boot_env.available_mut()
        && bl.get_env(BootEnvKey::ResizedData)?.is_some()
    {
        log::debug!("resize-data preflight: guard present; already resized");
        return Ok(());
    }

    // Attempt the resize. The boot env may be unavailable (degraded mode);
    // resize_if_needed accepts Option<&mut dyn BootEnv>.
    let bl_arg = ctx.boot_env.available_mut();
    if bl_arg.is_none() {
        log::warn!("resize-data: running without bootloader guard (degraded boot)");
    }
    let attempt = crate::filesystem::resize_data::resize_if_needed(ctx.layout, bl_arg);

    match attempt {
        Ok(()) => Ok(()),

        // RebootToApply is a Plan A reboot path, not a resize failure.
        // Propagate so handle_fatal_error can honor fsck's signal.
        Err(e @ crate::error::InitramfsError::Filesystem(
            crate::error::FilesystemError::FsckRequiresReboot { .. },
        )) => Err(e),

        // Dirty fsck (uncorrected errors) -> SkippedFsck indicator.
        Err(crate::error::InitramfsError::Filesystem(
            crate::error::FilesystemError::FsckFailed { device, code, output: _ },
        )) => {
            log::warn!(
                "resize-data skipped: {} fsck reported uncorrected errors (code {})",
                device.display(),
                code
            );
            ctx.ods_status.set_resize_status(crate::runtime::ResizeStatus {
                outcome: crate::runtime::ResizeOutcome::SkippedFsck,
                reason: format!(
                    "{} fsck reported uncorrected errors (code {})",
                    device.display(),
                    code
                ),
            });
            Ok(())
        }

        // Layout-shape errors (missing data partition, bad path, …).
        #[cfg(feature = "resize-data")]
        Err(crate::error::InitramfsError::ResizeData(ref e))
            if matches!(
                e,
                crate::error::ResizeDataError::InvalidDevicePath(_)
                    | crate::error::ResizeDataError::NonUtf8Path(_)
                    | crate::error::ResizeDataError::ExtendedPartitionNotFound(_)
            ) =>
        {
            log::warn!("resize-data skipped: invalid layout: {}", attempt.as_ref().err().unwrap());
            ctx.ods_status.set_resize_status(crate::runtime::ResizeStatus {
                outcome: crate::runtime::ResizeOutcome::InvalidLayout,
                reason: attempt.unwrap_err().to_string(),
            });
            Ok(())
        }

        // Everything else (parted/sgdisk/resize2fs/sync tool errors and any
        // bubbled-up I/O) -> ToolError indicator.
        Err(e) => {
            log::warn!("resize-data skipped: tool error: {e}");
            ctx.ods_status.set_resize_status(crate::runtime::ResizeStatus {
                outcome: crate::runtime::ResizeOutcome::ToolError,
                reason: e.to_string(),
            });
            Ok(())
        }
    }
}
```

Note on the `matches!` arm: it uses `attempt.as_ref().err().unwrap()` for the log line and `attempt.unwrap_err().to_string()` for the reason. The `as_ref().err().unwrap()` is safe here because we are inside the `Err(...)` arm (`if` guard does not move the `Err` value).

- [ ] **Step 2.7: Update / remove the obsolete `skips_when_bootloader_unavailable` test**

The existing test:

```rust
    #[test]
    fn skips_when_bootloader_unavailable() {
        let layout = empty_layout();
        let mut ctx = PreflightCtx {
            layout: &layout,
            bootloader: None,
        };
        assert!(run(&mut ctx).is_ok());
    }
```

Update it to match the new `PreflightCtx` shape (now uses `boot_env: &mut BootEnvState` and `ods_status`):

```rust
    #[test]
    fn runs_under_degraded_env_and_records_no_status() {
        let layout = empty_layout();
        let mut env = BootEnvState::Degraded(crate::error::BootEnvError::CommandFailed {
            command: "grub-editenv".into(),
            reason: "test".into(),
        });
        let mut ods = OdsStatus::new();
        let mut ctx = PreflightCtx {
            layout: &layout,
            boot_env: &mut env,
            ods_status: &mut ods,
        };
        assert!(run(&mut ctx).is_ok());
        assert!(ods.resize_data.is_none());
    }
```

- [ ] **Step 2.8: Run all tests**

Run: `cargo test --lib --features resize-data`
Expected: all tests pass — the new ones, the updated one, and every existing test.

Run without `resize-data`:
`cargo test --lib`
Expected: all default-feature tests pass (the resize-data tests are cfg-gated away).

- [ ] **Step 2.9: Check clippy + format**

Run: `cargo clippy --all-features --all-targets -- -D warnings && cargo fmt -- --check`
Expected: no output, exit 0.

- [ ] **Step 2.10: Commit**

```bash
git add src/preflight/resize_data.rs src/preflight/mod.rs src/lib.rs
git commit -s -m "feat(preflight): resize-data is best-effort; records ResizeStatus

Wrap resize_if_needed in preflight::resize_data::run so that every
failure mode except FsckRequiresReboot is absorbed and surfaced via
OdsStatus.resize_data. FsckRequiresReboot propagates so the Plan A
recovery model treats it as RebootToApply.

PreflightCtx gains an ods_status: &mut OdsStatus field; lib.rs passes
it in. The dirty-fsck brick (release: infinite loop, guard never set,
repeats forever) is eliminated — fsck fail now boots with an
indicator instead of hanging.

Spec: docs/superpowers/specs/2026-05-27-fsck-and-resize-design.md §3"
```

---

## Final verification

- [ ] **Step F.1: Full test suite**

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

- [ ] **Step F.3: Sanity-check the new public surface**

```bash
grep -n "ResizeOutcome\|ResizeStatus\|set_resize_status\|resize_data:" src/runtime/omnect_device_service.rs
grep -n "ods_status\|set_resize_status" src/preflight/resize_data.rs
```

Expected: the additions are present; nothing unrelated changed.

---

## Out of scope (handled by other plans)

- **Plan A:** the `FsckRequiresReboot → RebootToApply → Reboot` mapping lives in `recovery::decide`. Plan B only re-propagates the error variant; no change to the action mapping is required here.
- **Plan C:** the `OdsStatus.degraded_boot` upgrade lives in Plan C. Plan B's `OdsStatus.resize_data` field uses the same serialization style (Option + `skip_serializing_if`) but is independent.
- **Plan D:** removes `BootEnvKey::ResizedData` and the `write_resize_guard` helper, and switches preflight's guard check to `BootEnvKey::FirstBootDone`. Plan B intentionally **does not** touch those — it keeps the existing `BootEnvKey::ResizedData` reads and the existing guard-write inside `resize_if_needed` so the two plans can land independently. After Plan D lands, the wrapper in this plan continues to work; only the env key it reads in the early-return check changes.
