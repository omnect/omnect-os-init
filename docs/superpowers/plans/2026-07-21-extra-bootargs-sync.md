# Extra-Bootargs Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Sync `omnect_extra_bootargs` from the boot-partition files to the bootloader env on the fresh-flash boot only, then reboot to apply — without breaking OTA rollback or risking a reboot loop.

**Architecture:** A new `init_setup::extra_bootargs` step (created from scratch) runs first among the init-setup steps. It gates on `first_boot && !update_pending`, writes the env, verifies the write by reading it back, flushes it with `sync()`, records an ODS status entry, and signals a reboot by returning a new `InitramfsError::ExtraBootArgsUpdated` (classified `RebootToApply`). All other outcomes are best-effort: logged, recorded in ODS, boot continues.

**Tech Stack:** Rust, `nix` (0.29, `fs` feature already enabled for `sync()`), `thiserror`, `serde`. Bootloader env via the `BootEnv` trait; tests via `MockBootEnv` (`test-utils`).

**Baseline:** This plan targets the current `main`. On `main` there is **no** `src/init_setup/extra_bootargs.rs`, **no** `BootEnvKey::ExtraBootArgs`, and `InitSetupCtx` has no `rootfs`/`update_pending` fields — all are created here. Present on `main` already: the first-boot marker (`compute_first_boot`), the ODS `resize_data`/`ResizeStatus` pattern, the `RebootToApply` recovery class, the `update_pending` infra (`read_update_pending`), and a read-write boot mount.

**Spec:** `docs/superpowers/specs/2026-07-21-extra-bootargs-sync-design.md`

## Global Constraints

- **No magic path strings.** Use `mount_points::BOOT` (value `"boot"`), never a `"boot"` literal.
- **No magic numbers / inline literals** for keys — use `BootEnvKey::ExtraBootArgs`.
- **Imports/consts at top of file**, before `fn`/`impl` (test-scoped imports excepted).
- **Comments explain why, not what.** No PR/issue/commit references.
- **`recovery_class()` stays an exhaustive match** — a new `InitramfsError` variant must get its own arm.
- **Commits:** Conventional Commits; every commit ends with `Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>` (matches the repo-local git identity). No AI co-author trailer.
- **Test feature combo for this work:** `--features grub,gpt,test-utils` (the step is not bootloader-specific; U-Boot behaves the same). Full CI matrix still applies (Task 5).

---

### Task 1: ODS status entry for the bootargs sync

**Files:**
- Modify: `src/runtime/omnect_device_service.rs`
- Modify: `src/runtime/mod.rs` (re-export the new public types)

**Interfaces:**
- Produces: `ExtraBootArgsOutcome` enum (failure kinds: `ReadFailed`, `SetEnvFailed`, `ReadBackFailed`, `ReadBackMismatch`); `ExtraBootArgsStatus { outcome: ExtraBootArgsOutcome, reason: String }`; `OdsStatus.extra_bootargs: Option<ExtraBootArgsStatus>` (set only on failure, `None` otherwise); `OdsStatus::set_extra_bootargs_status(...)`. All re-exported from `crate::runtime`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `src/runtime/omnect_device_service.rs` (`use super::*;` is already present):

```rust
#[test]
fn extra_bootargs_failure_serializes_kind_and_reason() {
    let mut ods = OdsStatus::new();
    ods.set_extra_bootargs_status(ExtraBootArgsStatus {
        outcome: ExtraBootArgsOutcome::SetEnvFailed,
        reason: "boom".to_string(),
    });
    let json = serde_json::to_string(&ods).unwrap();
    assert!(json.contains(r#""extra_bootargs""#));
    assert!(json.contains(r#""outcome":"set_env_failed""#));
    assert!(json.contains(r#""reason":"boom""#));
}

#[test]
fn extra_bootargs_absent_is_not_serialized() {
    let ods = OdsStatus::new();
    let json = serde_json::to_string(&ods).unwrap();
    assert!(!json.contains("extra_bootargs"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features grub,gpt,test-utils extra_bootargs_failure_serializes_kind_and_reason -- --nocapture`
Expected: FAIL to compile — `ExtraBootArgsOutcome`, `ExtraBootArgsStatus`, `set_extra_bootargs_status` not found.

- [ ] **Step 3: Add the types, field, setter, and re-export**

Add the enum and struct next to `ResizeOutcome` / `ResizeStatus`:

```rust
/// Why the extra-bootargs sync failed on this boot.
///
/// Serialized as snake_case so ODS and cloud consumers can match exact strings.
/// Failure kinds only — the status exists only on failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtraBootArgsOutcome {
    /// Reading the current value failed (before any write).
    ReadFailed,
    /// Writing the env value failed.
    SetEnvFailed,
    /// The read-back after a successful write failed — the value is persisted
    /// but unverified.
    ReadBackFailed,
    /// The stored value read back different from what was written.
    ReadBackMismatch,
}

/// Extra-bootargs sync failure, for ODS diagnosis.
///
/// Recorded only when the sync failed on this boot (`None` otherwise, like
/// `resize_data`). `normal::run` withholds the first-boot marker while this is
/// `Some`, so the sync retries on the next boot.
#[derive(Debug, Clone, Serialize)]
pub struct ExtraBootArgsStatus {
    /// The failure kind, for exact matching by consumers.
    pub outcome: ExtraBootArgsOutcome,
    /// One-line detail for operator diagnosis.
    pub reason: String,
}
```

Add the field to `OdsStatus` (after `resize_data`):

```rust
    /// Extra-bootargs sync failure on this boot. `None` on success, no-op, or
    /// when the step did not run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_bootargs: Option<ExtraBootArgsStatus>,
```

Add the setter inside `impl OdsStatus` (after `set_resize_status`):

```rust
    /// Record the extra-bootargs sync outcome for ODS.
    pub fn set_extra_bootargs_status(&mut self, status: ExtraBootArgsStatus) {
        self.extra_bootargs = Some(status);
    }
```

Extend the re-export in `src/runtime/mod.rs`:

```rust
pub use self::omnect_device_service::{
    ExtraBootArgsOutcome, ExtraBootArgsStatus, FactoryResetStatus, FactoryResetStatusCode,
    ODS_RUNTIME_DIR, OdsStatus, ResizeOutcome, ResizeStatus, create_ods_runtime_files,
};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features grub,gpt,test-utils extra_bootargs -- --nocapture`
Expected: PASS (both new tests).

- [ ] **Step 5: Commit**

```bash
git add src/runtime/omnect_device_service.rs src/runtime/mod.rs
git commit -m "feat(runtime): add extra_bootargs ODS status entry

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

### Task 2: `ExtraBootArgsUpdated` error + recovery class

**Files:**
- Modify: `src/error.rs` (enum `InitramfsError`, `recovery_class`, tests)
- Modify: `src/recovery.rs` (doc comment on `Action::Reboot`)

**Interfaces:**
- Consumes: `RecoveryClass::RebootToApply` (existing on `main`).
- Produces: `InitramfsError::ExtraBootArgsUpdated` (unit variant) classified as `RecoveryClass::RebootToApply`.

- [ ] **Step 1: Write the failing test**

Add to `mod recovery_class_tests` in `src/error.rs` (it already asserts `RebootToApply` for `FsckRequiresReboot`):

```rust
#[test]
fn extra_bootargs_updated_reboots_to_apply() {
    let err = InitramfsError::ExtraBootArgsUpdated;
    assert_eq!(err.recovery_class(), RecoveryClass::RebootToApply);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features grub,gpt,test-utils extra_bootargs_updated_reboots_to_apply -- --nocapture`
Expected: FAIL to compile — no variant `ExtraBootArgsUpdated`.

- [ ] **Step 3: Add the variant and its recovery arm**

In `enum InitramfsError`, add before the `Io` variant:

```rust
    #[error("extra bootargs updated; reboot required to apply")]
    ExtraBootArgsUpdated,
```

In `recovery_class`, add an arm before `Self::Io(_)`:

```rust
            Self::ExtraBootArgsUpdated => RecoveryClass::RebootToApply,
```

- [ ] **Step 4: Update the `Action::Reboot` doc comment**

In `src/recovery.rs`, replace the `Action::Reboot` doc comment so it names all reboot reasons and is honest about the loop bounds:

```rust
    /// Reboot the device. Reasons: OTA-rollback (Fatal + update_pending),
    /// fsck reboot-required, or extra-bootargs applied on first boot. The OTA
    /// case is bounded by the bootloader; fsck and extra-bootargs are
    /// accepted-risk loops — extra-bootargs mitigates the loop with read-back
    /// verify + sync in the step but does not bound it.
    Reboot,
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --features grub,gpt,test-utils recovery_class -- --nocapture`
Expected: PASS. Confirm the exhaustive match still builds: `cargo build --features grub`.

- [ ] **Step 6: Commit**

```bash
git add src/error.rs src/recovery.rs
git commit -m "feat(error): add ExtraBootArgsUpdated reboot-to-apply variant

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

### Task 3: Add `BootEnvKey::ExtraBootArgs`

**Files:**
- Modify: `src/bootloader/mod.rs` (`BootEnvKey` enum + `as_str` + test)

**Interfaces:**
- Produces: `BootEnvKey::ExtraBootArgs`, mapping to the env-var name `omnect_extra_bootargs`.

- [ ] **Step 1: Write the failing test**

Add to the bootloader tests module (near `first_boot_done_key_string`):

```rust
#[test]
fn extra_bootargs_key_string() {
    assert_eq!(
        BootEnvKey::ExtraBootArgs.as_str().as_ref(),
        "omnect_extra_bootargs"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features grub,gpt,test-utils extra_bootargs_key_string -- --nocapture`
Expected: FAIL to compile — no variant `ExtraBootArgs`.

- [ ] **Step 3: Add the variant and mapping**

In `enum BootEnvKey`, add after `FirstBootDone` (not feature-gated):

```rust
    /// `omnect_extra_bootargs` — extra kernel cmdline arguments the bootloader
    /// appends. Synced from the boot-partition files by the extra-bootargs
    /// init-setup step.
    ExtraBootArgs,
```

In `as_str`, add the arm after `FirstBootDone`:

```rust
            Self::ExtraBootArgs => Cow::Borrowed("omnect_extra_bootargs"),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --features grub,gpt,test-utils extra_bootargs_key_string -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bootloader/mod.rs
git commit -m "feat(bootloader): add BootEnvKey::ExtraBootArgs

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

### Task 4: Create the extra-bootargs init-setup step and wire it

**Files:**
- Create: `src/init_setup/extra_bootargs.rs`
- Modify: `src/init_setup/mod.rs` (add `pub mod extra_bootargs;`; add `rootfs` + `update_pending` to `InitSetupCtx`; call the step first)
- Modify: `src/lib.rs` (populate `rootfs` + `update_pending` when building `InitSetupCtx`)
- Modify: `src/bootloader/mod.rs` (add a `MockBootEnv` knob for the read-back mismatch test)

**Interfaces:**
- Consumes: `ExtraBootArgsStatus`, `OdsStatus::set_extra_bootargs_status` (Task 1); `InitramfsError::ExtraBootArgsUpdated` (Task 2); `BootEnvKey::ExtraBootArgs` (Task 3); `crate::read_update_pending()`; `mount_points::BOOT`.
- Produces: `extra_bootargs::run(ctx: &mut InitSetupCtx) -> crate::Result<()>`; `should_sync(first_boot: bool, update_pending: bool) -> bool`; `InitSetupCtx` gains `rootfs: &'r Path` and `update_pending: bool` (now `InitSetupCtx<'l, 'b, 's, 'r>`); `MockBootEnv::with_set_env_normalize(&str)`.

- [ ] **Step 1: Add a `MockBootEnv` knob to simulate a normalizing tool**

The read-back verify only triggers when the stored value differs from what was written. `MockBootEnv` stores exactly what it is given, so add a knob that forces `set_env` to store a fixed value instead.

In `src/bootloader/mod.rs`, add a field to `MockBootEnv`:

```rust
    /// When set, `set_env` stores this fixed value instead of the given one,
    /// simulating a bootloader tool that normalizes the written value.
    set_env_normalize: Option<String>,
```

Add the builder in `impl MockBootEnv`:

```rust
    pub fn with_set_env_normalize(mut self, stored: &str) -> Self {
        self.set_env_normalize = Some(stored.to_string());
        self
    }
```

In `set_env`, after `self.set_env_calls.push(key);`, replace the `match value` block so the normalized value wins when present:

```rust
        let to_store = match &self.set_env_normalize {
            Some(forced) => Some(forced.clone()),
            None => value.map(|v| v.to_string()),
        };
        match to_store {
            Some(v) => {
                self.env.insert(key.as_str().to_string(), v);
            }
            None => {
                self.env.remove(key.as_str().as_ref());
            }
        }
        Ok(())
```

Confirm it builds: `cargo build --features grub,gpt,test-utils`.

- [ ] **Step 2: Extend `InitSetupCtx` and wire lib.rs**

In `src/init_setup/mod.rs`: fix the stale module doc, add the module declaration, the import, the two fields (and the `'r` lifetime), and call the step first.

Fix the module doc — `extra_bootargs` is unconditional, so "each step is independently feature-gated" is no longer true:

```rust
//! Init setup: conditional one-time prep steps that run after core mount
//! and the bootloader env is open, but before mode dispatch.
//!
//! Steps are idempotent — guarded by bootloader env or filesystem state so each
//! runs at most once per trigger. Some steps are feature-gated (`resize_data`),
//! others always run (`extra_bootargs`).
```

Add near the top, after the doc comment:

```rust
pub mod extra_bootargs;
#[cfg(feature = "resize-data")]
pub mod resize_data;

use std::path::Path;

use crate::{Result, bootloader::BootEnvState, partition::PartitionLayout, runtime::OdsStatus};
```

Replace the struct:

```rust
/// Context passed to each init setup step.
#[non_exhaustive]
pub struct InitSetupCtx<'l, 'b, 's, 'r> {
    pub layout: &'l PartitionLayout,
    pub boot_env: &'b mut BootEnvState,
    pub ods_status: &'s mut OdsStatus,
    pub rootfs: &'r Path,
    /// OTA update in flight (`omnect_validate_update` set). The extra-bootargs
    /// step must not persist bootargs during an update validation boot.
    pub update_pending: bool,
}
```

Replace `run` (the `extra_bootargs` step always runs, so the old `#[cfg_attr(...)]` allow is no longer needed):

```rust
/// Run all enabled init setup steps in order.
///
/// Steps are independent and idempotent. Order is intentional: extra-bootargs
/// may reboot before resize-data touches any partition read-write.
pub fn run(mut ctx: InitSetupCtx<'_, '_, '_, '_>) -> Result<()> {
    extra_bootargs::run(&mut ctx)?;
    #[cfg(feature = "resize-data")]
    resize_data::run(&mut ctx)?;
    Ok(())
}
```

Update the `resize_data::run` signature in `src/init_setup/resize_data.rs` to the 4-lifetime form:

```rust
pub fn run(ctx: &mut InitSetupCtx<'_, '_, '_, '_>) -> crate::Result<()> {
```

In `src/lib.rs`, set the two new fields where `InitSetupCtx` is built (the `rootfs` binding already exists in `run_init`; `read_update_pending` is defined in this module):

```rust
        let ctx = init_setup::InitSetupCtx {
            layout: &layout,
            boot_env: &mut bootloader_env,
            ods_status: &mut ods_status,
            rootfs,
            update_pending: read_update_pending(),
        };
        init_setup::run(ctx)?;
```

- [ ] **Step 3: Write the failing tests (the new step file)**

Create `src/init_setup/extra_bootargs.rs` containing only the `use` block, the two consts, and the full `#[cfg(test)] mod tests` below — no `run`/helpers yet, so the tests fail to compile first.

```rust
use std::io::ErrorKind;
use std::path::Path;

use crate::bootloader::BootEnvKey;
use crate::error::InitramfsError;
use crate::filesystem::mount_points;
use crate::init_setup::InitSetupCtx;
use crate::runtime::{ExtraBootArgsOutcome, ExtraBootArgsStatus};

/// Boot-partition file for distro-managed extra boot arguments.
const BOOTARGS_OMNECT_FILE: &str = "omnect_extra_bootargs_omnect";
/// Boot-partition file for user-managed extra boot arguments.
const BOOTARGS_CUSTOM_FILE: &str = "omnect_extra_bootargs_custom";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootloader::{BootEnvKey, BootEnvState, MockBootEnv};
    use crate::partition::{PartitionLayout, RootDevice};
    use crate::runtime::OdsStatus;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn empty_layout() -> PartitionLayout {
        PartitionLayout {
            partitions: HashMap::new(),
            device: RootDevice {
                base: PathBuf::from("/dev/sda"),
                partition_sep: "",
                root_partition: PathBuf::from("/dev/sda2"),
            },
        }
    }

    fn make_ctx<'l, 'b, 's, 'r>(
        layout: &'l PartitionLayout,
        env: &'b mut BootEnvState,
        ods: &'s mut OdsStatus,
        rootfs: &'r Path,
        first_boot: bool,
        update_pending: bool,
    ) -> InitSetupCtx<'l, 'b, 's, 'r> {
        ods.first_boot = first_boot;
        InitSetupCtx {
            layout,
            boot_env: env,
            ods_status: ods,
            rootfs,
            update_pending,
        }
    }

    fn write_file(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    // ---- read_extra_bootargs -------------------------------------------

    #[test]
    fn no_files_yields_empty_string() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(read_extra_bootargs(tmp.path()), "");
    }

    #[test]
    fn omnect_only_returns_its_content() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), BOOTARGS_OMNECT_FILE, "quiet loglevel=3\n");
        assert_eq!(read_extra_bootargs(tmp.path()), "quiet loglevel=3");
    }

    #[test]
    fn both_files_are_joined_with_space() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), BOOTARGS_OMNECT_FILE, "quiet loglevel=3");
        write_file(tmp.path(), BOOTARGS_CUSTOM_FILE, "myarg=1");
        assert_eq!(read_extra_bootargs(tmp.path()), "quiet loglevel=3 myarg=1");
    }

    #[test]
    fn empty_files_yield_empty_string() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), BOOTARGS_OMNECT_FILE, "   \n");
        write_file(tmp.path(), BOOTARGS_CUSTOM_FILE, "\n");
        assert_eq!(read_extra_bootargs(tmp.path()), "");
    }

    #[test]
    fn internal_whitespace_is_squeezed() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), BOOTARGS_OMNECT_FILE, "quiet    loglevel=3");
        write_file(tmp.path(), BOOTARGS_CUSTOM_FILE, "a\tb");
        assert_eq!(read_extra_bootargs(tmp.path()), "quiet loglevel=3 a b");
    }

    // ---- should_sync ---------------------------------------------------

    #[test]
    fn should_sync_only_on_first_boot_without_update() {
        assert!(should_sync(true, false));
        assert!(!should_sync(false, false));
        assert!(!should_sync(true, true));
        assert!(!should_sync(false, true));
    }

    // ---- run() ---------------------------------------------------------

    #[test]
    fn skips_when_not_first_boot() {
        let tmp = TempDir::new().unwrap();
        let boot_dir = tmp.path().join("boot");
        std::fs::create_dir_all(&boot_dir).unwrap();
        write_file(&boot_dir, BOOTARGS_OMNECT_FILE, "quiet loglevel=3");

        let layout = empty_layout();
        let mock = MockBootEnv::new();
        let mut env = BootEnvState::Available(Box::new(mock));
        let mut ods = OdsStatus::new();
        let mut ctx = make_ctx(&layout, &mut env, &mut ods, tmp.path(), false, false);
        assert!(run(&mut ctx).is_ok());
        assert!(ctx.ods_status.extra_bootargs.is_none());
        let bl = ctx.boot_env.available_mut().unwrap();
        assert_eq!(bl.get_env(BootEnvKey::ExtraBootArgs).unwrap(), None);
    }

    #[test]
    fn skips_when_update_pending() {
        let tmp = TempDir::new().unwrap();
        let boot_dir = tmp.path().join("boot");
        std::fs::create_dir_all(&boot_dir).unwrap();
        write_file(&boot_dir, BOOTARGS_OMNECT_FILE, "quiet");

        let layout = empty_layout();
        let mock = MockBootEnv::new();
        let mut env = BootEnvState::Available(Box::new(mock));
        let mut ods = OdsStatus::new();
        let mut ctx = make_ctx(&layout, &mut env, &mut ods, tmp.path(), true, true);
        assert!(run(&mut ctx).is_ok());
        assert!(ctx.ods_status.extra_bootargs.is_none());
    }

    #[test]
    fn already_current_is_noop() {
        let tmp = TempDir::new().unwrap();
        let boot_dir = tmp.path().join("boot");
        std::fs::create_dir_all(&boot_dir).unwrap();
        write_file(&boot_dir, BOOTARGS_OMNECT_FILE, "quiet loglevel=3");

        let layout = empty_layout();
        let mock = MockBootEnv::new().with_env(BootEnvKey::ExtraBootArgs, "quiet loglevel=3");
        let mut env = BootEnvState::Available(Box::new(mock));
        let mut ods = OdsStatus::new();
        let mut ctx = make_ctx(&layout, &mut env, &mut ods, tmp.path(), true, false);
        assert!(run(&mut ctx).is_ok());
        assert!(
            ctx.ods_status.extra_bootargs.is_none(),
            "no-op must not record an ODS entry"
        );
    }

    #[test]
    fn changed_value_applies_and_requests_reboot() {
        let tmp = TempDir::new().unwrap();
        let boot_dir = tmp.path().join("boot");
        std::fs::create_dir_all(&boot_dir).unwrap();
        write_file(&boot_dir, BOOTARGS_OMNECT_FILE, "quiet loglevel=3");

        let layout = empty_layout();
        let mock = MockBootEnv::new();
        let mut env = BootEnvState::Available(Box::new(mock));
        let mut ods = OdsStatus::new();
        let mut ctx = make_ctx(&layout, &mut env, &mut ods, tmp.path(), true, false);
        let result = run(&mut ctx);
        assert!(matches!(result, Err(InitramfsError::ExtraBootArgsUpdated)));
        assert!(
            ctx.ods_status.extra_bootargs.is_none(),
            "success is not recorded in ODS (reboot before JSON write)"
        );
        let bl = ctx.boot_env.available_mut().unwrap();
        assert_eq!(
            bl.get_env(BootEnvKey::ExtraBootArgs).unwrap().as_deref(),
            Some("quiet loglevel=3")
        );
    }

    #[test]
    fn read_back_mismatch_records_failed_without_reboot() {
        let tmp = TempDir::new().unwrap();
        let boot_dir = tmp.path().join("boot");
        std::fs::create_dir_all(&boot_dir).unwrap();
        write_file(&boot_dir, BOOTARGS_OMNECT_FILE, "quiet loglevel=3");

        let layout = empty_layout();
        let mock = MockBootEnv::new().with_set_env_normalize("mangled");
        let mut env = BootEnvState::Available(Box::new(mock));
        let mut ods = OdsStatus::new();
        let mut ctx = make_ctx(&layout, &mut env, &mut ods, tmp.path(), true, false);
        let result = run(&mut ctx);
        assert!(result.is_ok(), "must not request reboot on read-back mismatch");
        assert_eq!(
            ctx.ods_status.extra_bootargs.as_ref().unwrap().outcome,
            ExtraBootArgsOutcome::ReadBackMismatch
        );
    }
}
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test --features grub,gpt,test-utils extra_bootargs -- --nocapture`
Expected: FAIL to compile — `read_extra_bootargs`, `should_sync`, `run` not defined.

- [ ] **Step 5: Add the implementation**

In `src/init_setup/extra_bootargs.rs`, above the test module, add the module doc, helpers, `should_sync`, and `run`:

```rust
//! Init setup step: sync `omnect_extra_bootargs` to the bootloader env.
//!
//! On the fresh-flash boot only, this reads the boot-partition argument files
//! and, if they differ from the stored value, writes the value, verifies it,
//! flushes it to disk, and requests a reboot so the bootloader applies the
//! arguments from the next boot. OTA argument changes are handled by the
//! swupdate handler and the bootloader validate mechanism, not here.
```

(Place the doc at the very top of the file, before the `use` block.)

```rust
/// Gate: the sync runs only on the fresh-flash boot and never during an OTA
/// validation boot. `first_boot` alone already excludes OTA (the marker
/// survives updates); `update_pending` is defence-in-depth.
fn should_sync(first_boot: bool, update_pending: bool) -> bool {
    first_boot && !update_pending
}

/// Build the combined bootargs value from the two boot-partition files: the
/// distro file plus the optional custom file. Whitespace runs are squeezed to
/// single spaces (matching the legacy `awk '{$1=$1};1'`) to normalize irregular
/// whitespace in hand-edited files, so the built value is stable across boots.
fn read_extra_bootargs(boot_dir: &Path) -> String {
    let omnect = read_bootargs_file(&boot_dir.join(BOOTARGS_OMNECT_FILE));
    let custom = read_bootargs_file(&boot_dir.join(BOOTARGS_CUSTOM_FILE));
    let combined = match (omnect.as_deref(), custom.as_deref()) {
        (Some(a), Some(b)) => format!("{a} {b}"),
        (Some(a), None) => a.to_string(),
        (None, Some(b)) => b.to_string(),
        (None, None) => String::new(),
    };
    combined.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Read one bootargs file, trimmed. `None` if empty or absent.
fn read_bootargs_file(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let trimmed = s.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => {
            log::warn!("extra-bootargs: failed to read {}: {e}", path.display());
            None
        }
    }
}

/// Sync `omnect_extra_bootargs` on the fresh-flash boot, then request a reboot.
///
/// Returns `Err(InitramfsError::ExtraBootArgsUpdated)` when the value changed
/// and was written, verified and flushed — the caller reboots. Every other
/// path returns `Ok(())`: the step is best-effort and never blocks boot. ODS
/// status is set only on failure (`None` otherwise), matching `resize_data`.
pub fn run(ctx: &mut InitSetupCtx<'_, '_, '_, '_>) -> crate::Result<()> {
    if !should_sync(ctx.ods_status.first_boot, ctx.update_pending) {
        log::debug!("extra-bootargs: skipping (not first boot or update pending)");
        return Ok(());
    }

    let boot_dir = ctx.rootfs.join(mount_points::BOOT);
    let new_args = read_extra_bootargs(&boot_dir);

    // `first_boot == true` implies the env is available (`compute_first_boot`
    // returns false on a degraded env), so this None arm cannot occur in
    // production. Kept as a defensive no-op rather than an unwrap.
    let bl = match ctx.boot_env.available_mut() {
        Some(bl) => bl,
        None => {
            log::warn!("extra-bootargs: unexpected degraded env despite first_boot; skipping");
            return Ok(());
        }
    };

    let current = match bl.get_env(BootEnvKey::ExtraBootArgs) {
        Ok(v) => v.unwrap_or_default(),
        Err(e) => {
            log::warn!("extra-bootargs: read current value failed: {e}");
            ctx.ods_status.set_extra_bootargs_status(ExtraBootArgsStatus {
                outcome: ExtraBootArgsOutcome::ReadFailed,
                reason: format!("read current value failed: {e}"),
            });
            return Ok(());
        }
    };

    if current == new_args {
        log::debug!("extra-bootargs: already up to date");
        return Ok(());
    }

    let value = if new_args.is_empty() {
        None
    } else {
        Some(new_args.as_str())
    };
    if let Err(e) = bl.set_env(BootEnvKey::ExtraBootArgs, value) {
        log::warn!("extra-bootargs: set_env failed: {e}");
        ctx.ods_status.set_extra_bootargs_status(ExtraBootArgsStatus {
            outcome: ExtraBootArgsOutcome::SetEnvFailed,
            reason: format!("set_env failed: {e}"),
        });
        return Ok(());
    }

    // Read-back verify: if the stored value reads back different from what was
    // written, rebooting could never converge (current never equals new_args).
    // The value is not rolled back — the tools store verbatim, so a mismatch is
    // a genuine write fault and the old value is no better (see spec §7).
    let readback = match bl.get_env(BootEnvKey::ExtraBootArgs) {
        Ok(v) => v.unwrap_or_default(),
        Err(e) => {
            log::warn!("extra-bootargs: read-back failed: {e}; not rebooting");
            ctx.ods_status.set_extra_bootargs_status(ExtraBootArgsStatus {
                outcome: ExtraBootArgsOutcome::ReadBackFailed,
                reason: format!("read-back failed: {e}"),
            });
            return Ok(());
        }
    };
    if readback != new_args {
        log::warn!("extra-bootargs: read-back mismatch; not rebooting");
        ctx.ods_status.set_extra_bootargs_status(ExtraBootArgsStatus {
            outcome: ExtraBootArgsOutcome::ReadBackMismatch,
            reason: "read-back verify mismatch".to_string(),
        });
        return Ok(());
    }

    // Flush the env write to disk. reboot(2) with RB_AUTOBOOT does not sync,
    // so without this the write can be lost across the reboot and loop forever.
    nix::unistd::sync();

    // Success is not recorded in ODS: we reboot before normal::run writes the
    // JSON, and the next boot is a no-op. The reboot is visible in kmsg.
    log::info!("extra-bootargs: applied {new_args:?}; rebooting to apply");
    Err(InitramfsError::ExtraBootArgsUpdated)
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --features grub,gpt,test-utils extra_bootargs -- --nocapture`
Expected: PASS (all `read_extra_bootargs`, `should_sync`, and `run` tests).

- [ ] **Step 7: Verify lints and the wider build**

Run:
```bash
cargo fmt -- --check
cargo clippy --tests --features grub,gpt,test-utils -- -D warnings -W clippy::items_after_statements
cargo build --features grub
```
Expected: no warnings, clean build.

- [ ] **Step 8: Commit**

```bash
git add src/init_setup/extra_bootargs.rs src/init_setup/mod.rs src/init_setup/resize_data.rs src/lib.rs src/bootloader/mod.rs
git commit -m "feat(init-setup): sync extra-bootargs on first boot and reboot to apply

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

### Task 5: Skip the first-boot marker on a failed sync

Security-first: a `Failed` sync must not close the first-boot gate, so it retries
next boot instead of leaving the device running without the security args forever.

**Files:**
- Modify: `src/mode/normal.rs`

**Interfaces:**
- Consumes: `OdsStatus.extra_bootargs` (Task 1) — `Some` means the sync failed on this boot.
- Produces: `extra_bootargs_applied_ok(&OdsStatus) -> bool`, folded into the `write_first_boot_marker` condition.

- [ ] **Step 1: Write the failing test**

Add to `mod marker_writer_tests` in `src/mode/normal.rs`:

```rust
#[test]
fn extra_bootargs_ok_unless_failed() {
    use crate::runtime::{ExtraBootArgsOutcome, ExtraBootArgsStatus, OdsStatus};
    let mut ods = OdsStatus::new();
    assert!(extra_bootargs_applied_ok(&ods)); // absent → ok

    ods.set_extra_bootargs_status(ExtraBootArgsStatus {
        outcome: ExtraBootArgsOutcome::SetEnvFailed,
        reason: "boom".into(),
    });
    assert!(!extra_bootargs_applied_ok(&ods)); // failure recorded → not ok
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features grub,gpt,test-utils extra_bootargs_ok_unless_failed -- --nocapture`
Expected: FAIL to compile — `extra_bootargs_applied_ok` not defined.

- [ ] **Step 3: Add the helper and fold it into the marker condition**

Add the helper near `resize_succeeded` in `src/mode/normal.rs`:

```rust
/// A failed extra-bootargs sync must not close the first-boot gate: withhold
/// the marker so `first_boot` stays true and the sync retries on the next boot.
/// `extra_bootargs` is `Some` only on failure (set only in that case).
fn extra_bootargs_applied_ok(ods_status: &crate::runtime::OdsStatus) -> bool {
    ods_status.extra_bootargs.is_none()
}
```

Update the `write_first_boot_marker` call in `run`:

```rust
    write_first_boot_marker(
        ods_status.first_boot
            && resize_succeeded(&ods_status)
            && extra_bootargs_applied_ok(&ods_status),
        &mut boot_env,
    );
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features grub,gpt,test-utils -- --nocapture normal`
Expected: PASS (new test plus the existing `marker_writer_tests`).

- [ ] **Step 5: Commit**

```bash
git add src/mode/normal.rs
git commit -m "feat(mode): retry extra-bootargs sync by withholding first-boot marker on failure

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

### Task 6: Verify across the feature matrix

**Files:** none (verification only).

- [ ] **Step 1: Run the relevant CI combinations**

Run each and confirm PASS:
```bash
cargo test --features grub,gpt,test-utils
cargo test --features uboot,dos,test-utils
cargo test --features grub,gpt,resize-data,test-utils
cargo test --features grub,gpt,release-image,test-utils
cargo test --features grub,gpt,resize-data,release-image,test-utils
```
Expected: PASS in all combos. The step is not feature-gated, so `resize-data` off/on and `release-image` off/on must all build and pass. `release-image` matters because `ExtraBootArgsUpdated → RebootToApply → Action::Reboot` must hold there too.

- [ ] **Step 2: Confirm ordering**

Read `src/init_setup/mod.rs` and confirm `extra_bootargs::run` runs before `resize_data::run`, so a reboot returns before resize touches the disk read-write.

---

## Notes for the implementer

- `first_boot` is read from `ctx.ods_status.first_boot`, set in `lib.rs` before `init_setup` runs. It stays `true` across the sync reboot (the marker is written later, in `normal::run`), so convergence relies on `current == new_args`, not on the marker. Do not add a marker write here.
- The boot partition is mounted read-write before `init_setup` (`mount_core_partitions`), and `grub-editenv` runs as root, so it can write the grubenv file. Do not mount it here.
- Do not call `reboot(2)` from this step — return the error and let `main::handle_fatal_error` reboot. That keeps the single reboot path.
- Adding `rootfs`/`update_pending` to `InitSetupCtx` is additive; `resize_data` ignores them. Only its `run` signature needs the extra elided lifetime.
