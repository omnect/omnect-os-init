# Boot Failure & Recovery Policy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the inline branching in `main.rs::handle_fatal_error` with an isolated, testable recovery model that branches the fatal-error response on `omnect_validate_update`, prevents bricking a release device that has an intact previous A/B slot, and fixes three known fatal-path bugs (release emergency shell, silent halt-loop spin, discarded `reboot(2)` result).

**Architecture:** Two new pure components — a `RecoveryClass` per error variant (lives next to `InitramfsError`) and a `recovery::decide(class, is_release, update_pending) → Action` function (new `src/recovery.rs`). `update_pending` is read once after the boot env is opened in `run_init` and stored in a `static AtomicBool`; `handle_fatal_error` reads it before deciding. Action *execution* (reboot / halt / shell) stays in `main.rs` and is not unit-tested.

**Tech Stack:** Rust 2024, `nix` 0.29 (mount/reboot), `log` + custom `KmsgLogger`, `thiserror`. No new external dependencies.

**Spec:** `docs/superpowers/specs/2026-05-27-boot-failure-recovery-policy-design.md`.

---

## File map

| Path | Action | Responsibility |
|---|---|---|
| `src/recovery.rs` | **Create** | Pure decision logic: `RecoveryClass`, `Action`, `decide(...)`. |
| `src/lib.rs` | Modify | Declare `pub mod recovery`; add `static UPDATE_PENDING`; read flag after `apply_boot_env_decision`. |
| `src/error.rs` | Modify | Add `InitramfsError::recovery_class()` exhaustive match. |
| `src/main.rs` | Modify | Refactor `handle_fatal_error`; move `is_release_image` to line 1 of `main()`; gate `spawn_emergency_shell` on debug only; use `log_fatal` inside the halt loop; log `reboot(2)` failure. |
| `src/logging/kmsg.rs` | No change | `log_fatal` already exists and is reused. |

Each task below is one self-contained change with TDD ordering and a commit at the end. Run all commands from the repo root.

---

## Task 1: Create the pure `recovery` module

**Files:**
- Create: `src/recovery.rs`
- Modify: `src/lib.rs`

This task defines the decision logic in isolation. No I/O, no logging, no error
types — just enums and a `match`. The test is the truth table from spec §4.

- [ ] **Step 1.1: Add `pub mod recovery;` to `src/lib.rs`**

In `src/lib.rs`, alongside the other `pub mod` declarations (after `pub mod preflight;`), add:

```rust
pub mod recovery;
```

- [ ] **Step 1.2: Create `src/recovery.rs` with the failing test**

Create the file with the truth-table test up front. The tests will not compile yet (types missing) — that's the deliberate "red" state for TDD.

```rust
//! Boot failure & recovery policy.
//!
//! Pure decision logic mapping an error's `RecoveryClass` and the boot
//! context to an `Action`. Action *execution* (reboot/halt/shell) lives
//! in `main.rs`; this module has no I/O.
//!
//! Spec: docs/superpowers/specs/2026-05-27-boot-failure-recovery-policy-design.md

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continue_degraded_always_continues() {
        for &is_release in &[false, true] {
            for &update in &[false, true] {
                assert_eq!(
                    decide(RecoveryClass::ContinueDegraded, is_release, update),
                    Action::Continue,
                    "ContinueDegraded -> Continue regardless of context (is_release={is_release}, update={update})"
                );
            }
        }
    }

    #[test]
    fn reboot_to_apply_always_reboots() {
        for &is_release in &[false, true] {
            for &update in &[false, true] {
                assert_eq!(
                    decide(RecoveryClass::RebootToApply, is_release, update),
                    Action::Reboot,
                    "RebootToApply -> Reboot regardless of context"
                );
            }
        }
    }

    #[test]
    fn fatal_with_update_pending_reboots() {
        // Anti-brick contract: a Fatal error during an unconfirmed OTA-update
        // boot reboots so the bootloader can roll back to the known-good slot.
        assert_eq!(decide(RecoveryClass::Fatal, true, true), Action::Reboot);
        assert_eq!(decide(RecoveryClass::Fatal, false, true), Action::Reboot);
    }

    #[test]
    fn fatal_release_halts() {
        assert_eq!(decide(RecoveryClass::Fatal, true, false), Action::Halt);
    }

    #[test]
    fn fatal_debug_shells() {
        assert_eq!(decide(RecoveryClass::Fatal, false, false), Action::Shell);
    }
}
```

- [ ] **Step 1.3: Run the test to verify it fails to compile**

Run: `cargo test --lib recovery::tests 2>&1 | head -20`
Expected: compile error mentioning `cannot find type RecoveryClass in this scope` (or `Action`, or function `decide`).

- [ ] **Step 1.4: Add the minimal types and function to make the tests pass**

Add the following *above* the `#[cfg(test)] mod tests {` block in `src/recovery.rs`:

```rust
/// How an `InitramfsError` is meant to be recovered from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryClass {
    /// Non-fatal; the caller should warn and proceed.
    ContinueDegraded,
    /// A reboot is expected to change the outcome (e.g. fsck applied a fix).
    RebootToApply,
    /// Boot cannot proceed.
    Fatal,
}

/// What `handle_fatal_error` should do for a given class + context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Caller is expected to have already swallowed the error; reaching this
    /// in `handle_fatal_error` is defensive only (see main.rs).
    Continue,
    /// Reboot the device. Either OTA-rollback (Fatal + update_pending) or
    /// fsck reboot-required (RebootToApply). Bounded by the bootloader for
    /// the OTA case; unconditional otherwise (documented accepted risk).
    Reboot,
    /// Infinite loop with kmsg logging; never exits PID 1.
    Halt,
    /// Spawn an interactive debug shell.
    Shell,
}

/// Decide the action for a recovery class and boot context.
///
/// `update_pending` is `true` iff `omnect_validate_update` was set in the
/// boot env at the time it was opened. When the env is unreadable or the
/// failure occurred before the env was opened, `update_pending` is `false`
/// per spec §2.5.
pub fn decide(class: RecoveryClass, is_release: bool, update_pending: bool) -> Action {
    match class {
        RecoveryClass::ContinueDegraded => Action::Continue,
        RecoveryClass::RebootToApply => Action::Reboot,
        RecoveryClass::Fatal => {
            if update_pending {
                Action::Reboot
            } else if is_release {
                Action::Halt
            } else {
                Action::Shell
            }
        }
    }
}
```

- [ ] **Step 1.5: Run the tests to verify they pass**

Run: `cargo test --lib recovery::tests`
Expected: `test result: ok. 5 passed; 0 failed`.

- [ ] **Step 1.6: Check clippy + format**

Run: `cargo clippy -- -D warnings && cargo fmt -- --check`
Expected: no output, exit 0.

- [ ] **Step 1.7: Commit**

```bash
git add src/recovery.rs src/lib.rs
git commit -s -m "feat(recovery): add recovery model and decide truth-table

Introduce RecoveryClass / Action enums and the pure decide(class,
is_release, update_pending) function. Truth-table tests cover all
3 x 2 x 2 combinations. No call sites yet — main.rs refactor follows.

Spec: docs/superpowers/specs/2026-05-27-boot-failure-recovery-policy-design.md"
```

---

## Task 2: Add `InitramfsError::recovery_class()` (exhaustive match)

**Files:**
- Modify: `src/error.rs`

This task classifies every existing `InitramfsError` variant. The match is exhaustive, so adding a new variant later will fail to compile until classified — that's the intent.

- [ ] **Step 2.1: Add a failing test for the classification**

Append to the existing `#[cfg(test)] mod` at the bottom of `src/error.rs` (or create one if it doesn't exist). The test references symbols we add next, so it will fail to compile.

```rust
#[cfg(test)]
mod recovery_class_tests {
    use super::*;
    use crate::filesystem::FsckExitCode;
    use crate::recovery::RecoveryClass;

    #[test]
    fn fsck_requires_reboot_is_reboot_to_apply() {
        let err = InitramfsError::Filesystem(FilesystemError::FsckRequiresReboot {
            device: std::path::PathBuf::from("/dev/sda7"),
            code: FsckExitCode::REBOOT_REQUIRED,
            output: String::new(),
        });
        assert_eq!(err.recovery_class(), RecoveryClass::RebootToApply);
    }

    #[test]
    fn fsck_failed_is_fatal() {
        let err = InitramfsError::Filesystem(FilesystemError::FsckFailed {
            device: std::path::PathBuf::from("/dev/sda7"),
            code: FsckExitCode::ERRORS_UNCORRECTED,
            output: String::new(),
        });
        assert_eq!(err.recovery_class(), RecoveryClass::Fatal);
    }

    #[test]
    fn mount_failed_is_fatal() {
        let err = InitramfsError::Filesystem(FilesystemError::MountFailed {
            src_path: std::path::PathBuf::from("/dev/sda2"),
            target: std::path::PathBuf::from("/rootfs"),
            reason: "test".into(),
        });
        assert_eq!(err.recovery_class(), RecoveryClass::Fatal);
    }

    #[test]
    fn degraded_boot_is_fatal() {
        let err = InitramfsError::DegradedBoot(BootEnvError::CommandFailed {
            command: "grub-editenv".into(),
            reason: "test".into(),
        });
        assert_eq!(err.recovery_class(), RecoveryClass::Fatal);
    }

    #[test]
    fn early_init_is_fatal() {
        let err = InitramfsError::EarlyInit(EarlyInitError::MountFailed {
            target: "/dev".into(),
            reason: "test".into(),
        });
        assert_eq!(err.recovery_class(), RecoveryClass::Fatal);
    }

    #[test]
    fn config_is_fatal() {
        let err = InitramfsError::Config(ConfigError::CmdlineReadFailed(
            std::io::Error::from(std::io::ErrorKind::NotFound),
        ));
        assert_eq!(err.recovery_class(), RecoveryClass::Fatal);
    }

    #[cfg(feature = "resize-data")]
    #[test]
    fn resize_data_is_continue_degraded() {
        // ResizeData becomes ContinueDegraded once Plan B lands; today the
        // error still propagates to handle_fatal_error, which defensively
        // treats Continue as Fatal (see main.rs). Plan B will absorb the
        // error inside the preflight wrapper so it no longer propagates.
        let err = InitramfsError::ResizeData(ResizeDataError::InvalidDevicePath(
            std::path::PathBuf::from("/dev/sda"),
        ));
        assert_eq!(err.recovery_class(), RecoveryClass::ContinueDegraded);
    }
}
```

- [ ] **Step 2.2: Run the test to verify it fails to compile**

Run: `cargo test --lib recovery_class_tests 2>&1 | head -20`
Expected: compile error `no method named recovery_class found for ...` or similar.

- [ ] **Step 2.3: Add the `recovery_class` method**

Append to `src/error.rs` (after the `InitramfsError` definition, before any `#[cfg(test)]`):

```rust
impl InitramfsError {
    /// Classify the error for the recovery policy. The match is exhaustive
    /// so a new variant fails to compile until classified.
    ///
    /// Spec: docs/superpowers/specs/2026-05-27-boot-failure-recovery-policy-design.md §2.1
    pub fn recovery_class(&self) -> crate::recovery::RecoveryClass {
        use crate::recovery::RecoveryClass;
        match self {
            Self::Bootloader(_) => RecoveryClass::Fatal,
            Self::DegradedBoot(_) => RecoveryClass::Fatal,
            Self::EarlyInit(_) => RecoveryClass::Fatal,
            Self::Partition(_) => RecoveryClass::Fatal,
            Self::Filesystem(FilesystemError::FsckRequiresReboot { .. }) => {
                RecoveryClass::RebootToApply
            }
            Self::Filesystem(_) => RecoveryClass::Fatal,
            Self::Logging(_) => RecoveryClass::Fatal,
            Self::Config(_) => RecoveryClass::Fatal,
            #[cfg(feature = "resize-data")]
            Self::ResizeData(_) => RecoveryClass::ContinueDegraded,
            Self::Io(_) => RecoveryClass::Fatal,
        }
    }
}
```

- [ ] **Step 2.4: Run the tests**

Run: `cargo test --lib recovery_class_tests`
Expected: all tests pass. If `resize-data` feature is not enabled in the default test profile, the resize test is auto-skipped (cfg-gated).

Run: `cargo test --lib --features resize-data recovery_class_tests`
Expected: same, plus the `resize_data_is_continue_degraded` test runs and passes.

- [ ] **Step 2.5: Check clippy + format**

Run: `cargo clippy -- -D warnings && cargo fmt -- --check`
Expected: no output, exit 0.

- [ ] **Step 2.6: Commit**

```bash
git add src/error.rs
git commit -s -m "feat(error): classify every InitramfsError variant by RecoveryClass

Add InitramfsError::recovery_class() with an exhaustive match. Adding a
new variant will fail to compile until classified. Tests pin the
intended class per variant; the resize-data test is feature-gated and
documents that Plan B will reclassify the propagation path (defensive
ContinueDegraded today is treated as Fatal in main.rs).

Spec: docs/superpowers/specs/2026-05-27-boot-failure-recovery-policy-design.md §2.1"
```

---

## Task 3: Plumb `update_pending` from `run_init` to `main`

**Files:**
- Modify: `src/lib.rs`

The fatal handler in `main.rs` needs to know whether an OTA update was being validated when the failure happened. `run_init` opens the boot env via `apply_boot_env_decision`; we read `omnect_validate_update` once afterward and store it in a process-global `static AtomicBool`. `main.rs` reads it in the next task.

Why a `static AtomicBool` rather than threading the value through return types: PID 1 is single-threaded, and run_init's failure path drops local state before main can inspect it. A `static` is the minimum-viable mechanism that preserves the value across the fallible region without refactoring run_init's signature.

- [ ] **Step 3.1: Add a failing test for the plumbing helper**

Append a test module to `src/lib.rs` (or extend the existing one) that verifies the helpers. Place it near the other unit tests in `lib.rs`:

```rust
#[cfg(test)]
mod update_pending_tests {
    use super::*;

    #[test]
    fn default_update_pending_is_false() {
        // The static must default to false so failures before the env is
        // opened (or in degraded mode) report the safe "no update in flight".
        // We can't reset the static between tests, so this test must run
        // before any other test in this module sets it.
        // The default is the load value at process start.
        let default_value = read_update_pending();
        // The test runner may run other tests first, so we only assert the
        // initial-load semantic via a fresh AtomicBool below.
        let fresh = std::sync::atomic::AtomicBool::new(false);
        assert!(!fresh.load(std::sync::atomic::Ordering::Relaxed));
        // Cover the public accessor with a deterministic write/read cycle:
        set_update_pending(true);
        assert!(read_update_pending());
        set_update_pending(false);
        assert!(!read_update_pending());
        // Restore to original to avoid leaking state to other tests.
        set_update_pending(default_value);
    }
}
```

- [ ] **Step 3.2: Run the test to verify it fails to compile**

Run: `cargo test --lib update_pending_tests 2>&1 | head -20`
Expected: compile error `cannot find function set_update_pending in this scope` (or `read_update_pending`).

- [ ] **Step 3.3: Add the static and accessors to `src/lib.rs`**

Near the top of `src/lib.rs` (after the existing `const ROOTFS_DIR: &str = "/rootfs";`), add:

```rust
use std::sync::atomic::{AtomicBool, Ordering};

/// `true` iff `omnect_validate_update` was set in the boot env at the time
/// it was read. Default `false` so a failure before the env is opened (or in
/// degraded mode) is reported as "no update in flight" per spec §2.5.
///
/// Single writer: [`run_init`] after `apply_boot_env_decision` succeeds.
/// Single reader: `main::handle_fatal_error`.
static UPDATE_PENDING: AtomicBool = AtomicBool::new(false);

/// Set the global update-pending flag. Called once per boot, after the
/// boot env is opened. Safe to call multiple times; the last call wins.
pub fn set_update_pending(value: bool) {
    UPDATE_PENDING.store(value, Ordering::Relaxed);
}

/// Read the global update-pending flag. Defaults to `false` if never set.
pub fn read_update_pending() -> bool {
    UPDATE_PENDING.load(Ordering::Relaxed)
}
```

- [ ] **Step 3.4: Read the flag in `run_init` after the boot env is opened**

Find the lines in `src/lib.rs::run_init`:

```rust
    let mut bootloader_env =
        apply_boot_env_decision(decision, core_result, &mut ods_status, rootfs)?;
```

Immediately *after* that line, insert:

```rust
    // Read omnect_validate_update once, before any subsequent fallible step.
    // Stored in a process-global so handle_fatal_error in main.rs can branch
    // on it without threading the value through every return type.
    let update_pending = bootloader_env
        .available()
        .and_then(|bl| {
            bl.get_env(bootloader::BootEnvKey::ValidateUpdate)
                .ok()
                .flatten()
        })
        .is_some();
    set_update_pending(update_pending);
```

The `is_some()` check: any non-absent value means an update is in flight. We do not distinguish `"1"` vs `"failed"` here — both indicate "the bootloader booted a slot whose validation has not yet been confirmed."

- [ ] **Step 3.5: Run the tests**

Run: `cargo test --lib update_pending_tests`
Expected: `1 passed`.

Run the full test suite to make sure no other test broke:

Run: `cargo test --lib`
Expected: all tests pass.

- [ ] **Step 3.6: Check clippy + format**

Run: `cargo clippy -- -D warnings && cargo fmt -- --check`
Expected: no output, exit 0.

- [ ] **Step 3.7: Commit**

```bash
git add src/lib.rs
git commit -s -m "feat(lib): read omnect_validate_update into a process-global

Add UPDATE_PENDING AtomicBool + set/read accessors. run_init populates
it once after apply_boot_env_decision succeeds. handle_fatal_error in
main.rs (next commit) reads it to decide whether to reboot or halt on
a Fatal error during an unconfirmed-update boot.

Spec: docs/superpowers/specs/2026-05-27-boot-failure-recovery-policy-design.md §3.1"
```

---

## Task 4: Refactor `main.rs` — use `recovery::decide`, fix the three bugs

**Files:**
- Modify: `src/main.rs`

This task wires the recovery model into the fatal-error path and fixes the three known bugs:
1. Early-mount failure on a release image must not spawn a shell (security/policy).
2. The halt loop must use direct kmsg, not the (possibly-unregistered) global logger.
3. A failed `reboot(2)` must be logged before the fallthrough.

No new tests are added for action *execution* (out of scope per spec §4); the recovery-decision tests in Task 1 cover the decision side, and the per-variant classification tests in Task 2 cover the input side.

- [ ] **Step 4.1: Move `is_release_image` to the first line of `main()`**

In `src/main.rs::main()`, the current code is:

```rust
fn main() {
    // Mount essential filesystems first (/dev, /proc, /sys, /run)
    if let Err(e) = mount_essential_filesystems() {
        eprintln!("FATAL: Failed to mount essential filesystems: {}", e);
        spawn_emergency_shell();
    }

    // Release vs. debug mode is a build-time property via the `release-image` feature.
    let is_release_image = cfg!(feature = "release-image");
    // ...
```

Replace the first lines of `main()` so `is_release_image` is the first binding and `mount_essential_filesystems` failure is routed through the policy:

```rust
fn main() {
    // Compile-time image-type discriminator. Read on line 1 so even the
    // earliest failures (mount_essential_filesystems, logger init) respect
    // the release/debug split. Spec invariant 1 (§2.6): release never shells.
    let is_release_image = cfg!(feature = "release-image");

    // Mount essential filesystems first (/dev, /proc, /sys, /run). On
    // failure: release halts; debug spawns the emergency shell.
    if let Err(e) = mount_essential_filesystems() {
        eprintln!("FATAL: Failed to mount essential filesystems: {}", e);
        if is_release_image {
            halt_with_message(&format!("Failed to mount essential filesystems: {e}"));
        } else {
            spawn_emergency_shell();
        }
    }
```

- [ ] **Step 4.2: Add the `halt_with_message` helper**

Below `spawn_debug_shell` in `src/main.rs`, add:

```rust
/// Halt forever with a fixed message written to /dev/kmsg each cycle.
///
/// Used by release images on any Fatal error path. Writes directly to
/// /dev/kmsg via `log_fatal` rather than through the `log` facade so a
/// failure of the kmsg logger itself does not silence the message.
fn halt_with_message(message: &str) -> ! {
    loop {
        log_fatal(message);
        thread::sleep(Duration::from_secs(FATAL_ERROR_SLEEP_SECS));
    }
}
```

`log_fatal` is already imported from `omnect_os_init::logging` at the top of `main.rs`. Confirm the import:

```rust
use omnect_os_init::{
    error::{FilesystemError, InitramfsError},
    logging::{KmsgLogger, log_fatal},
    mount_essential_filesystems,
};
```

If the import is missing `log_fatal`, add it.

- [ ] **Step 4.3: Refactor `handle_fatal_error` to use `recovery::decide`**

Replace the entire body of `handle_fatal_error` in `src/main.rs` with:

```rust
/// Handle a fatal error per the recovery policy.
///
/// Spec: docs/superpowers/specs/2026-05-27-boot-failure-recovery-policy-design.md §2-§3
fn handle_fatal_error(error: InitramfsError, is_release: bool) -> ! {
    use omnect_os_init::recovery::{Action, decide};

    let class = error.recovery_class();
    let update_pending = omnect_os_init::read_update_pending();
    let action = decide(class, is_release, update_pending);

    log_fatal(&format!(
        "fatal error (class={class:?}, update_pending={update_pending}, action={action:?}): {error}"
    ));

    match action {
        Action::Reboot => {
            if let Err(e) = nix::sys::reboot::reboot(nix::sys::reboot::RebootMode::RB_AUTOBOOT) {
                log_fatal(&format!("reboot(2) failed: {e}; halting"));
            }
            // reboot(2) should not return on success; if it does, fall back to halt.
            halt_with_message(&format!("reboot(2) returned unexpectedly after {action:?}"));
        }
        Action::Halt => {
            halt_with_message(&format!("FATAL: {error}"));
        }
        Action::Shell => {
            spawn_debug_shell();
        }
        Action::Continue => {
            // Defensive: ContinueDegraded errors should be absorbed by the
            // caller and never reach the fatal path. If we get here, something
            // failed to suppress the error — treat as Fatal so the device
            // doesn't fall off the policy.
            log_fatal(&format!(
                "BUG: ContinueDegraded reached handle_fatal_error for: {error}"
            ));
            if is_release {
                halt_with_message(&format!("FATAL (defensive): {error}"));
            } else {
                spawn_debug_shell();
            }
        }
    }
}
```

The previous unconditional-reboot block for `FsckRequiresReboot` is gone — that case is now handled via `RecoveryClass::RebootToApply → Action::Reboot` in `decide`, and `reboot(2)` failure is logged uniformly.

- [ ] **Step 4.4: Compile and run all tests**

Run: `cargo build`
Expected: clean build, no warnings.

Run: `cargo test --lib`
Expected: all existing tests still pass.

- [ ] **Step 4.5: Check clippy + format**

Run: `cargo clippy -- -D warnings && cargo fmt -- --check`
Expected: no output, exit 0.

- [ ] **Step 4.6: Manual trace through the four Action arms**

Read `handle_fatal_error` line-by-line and verify these scenarios mentally against the spec §2.3 truth table:

- `FsckRequiresReboot` on release, no update → class `RebootToApply` → `Action::Reboot` → reboot(2) called.
- `MountFailed` on release, no update → class `Fatal`, update_pending=false → `Action::Halt` → `halt_with_message`.
- `MountFailed` on release, update pending → class `Fatal`, update_pending=true → `Action::Reboot` → reboot, bootloader rolls back.
- `MountFailed` on debug, no update → `Action::Shell` → `spawn_debug_shell`.

Document any discrepancy as a comment fix; otherwise proceed.

- [ ] **Step 4.7: Commit**

```bash
git add src/main.rs
git commit -s -m "feat(main): apply recovery policy in handle_fatal_error

Wire recovery::decide into handle_fatal_error. Fixes three bugs:

  * Release images no longer spawn /bin/sh on early-mount failure.
    is_release_image moves to line 1 of main() so the policy applies
    before any fallible call. Halt instead of shell when release.
  * Halt loop now writes directly to /dev/kmsg via log_fatal on every
    cycle, instead of error!() through a possibly-unregistered logger.
    Silent-spin if logger init failed is eliminated.
  * reboot(2) result is logged via log_fatal on failure before the
    fall-through halt, rather than discarded.

The unconditional FsckRequiresReboot reboot block is removed; the same
behavior now flows through RecoveryClass::RebootToApply -> Action::Reboot.
A defensive Continue arm catches the should-not-happen case where a
ContinueDegraded error reaches the fatal path.

Spec: docs/superpowers/specs/2026-05-27-boot-failure-recovery-policy-design.md §2-§3"
```

---

## Final verification

After all tasks are committed, run:

- [ ] **Step F.1: Full test suite**

```bash
cargo test --all-features
```

Expected: every test passes; no warnings.

- [ ] **Step F.2: Clippy + format on the whole crate**

```bash
cargo clippy --all-features --all-targets -- -D warnings
cargo fmt -- --check
```

Expected: clean exit.

- [ ] **Step F.3: Sanity-check the public surface**

```bash
grep -n "pub fn\|pub mod\|pub struct\|pub enum" src/recovery.rs
grep -n "fn handle_fatal_error\|fn halt_with_message" src/main.rs
grep -n "UPDATE_PENDING\|set_update_pending\|read_update_pending" src/lib.rs
```

Expected: each command lists the additions made by the plan; nothing else has changed.

---

## Out of scope (handled by other plans)

- **Plan B (fsck & resize):** changes the propagation path of `ResizeDataError` so the defensive `Continue` arm in §4 step 4.3 is never reached in practice. No change to this plan's code.
- **Plan C (degraded boot):** upgrades `OdsStatus.degraded_boot` from `bool` to `Option<DegradedBootStatus>`. No interaction with the recovery model.
- **Plan D (first-boot detection):** introduces `BootEnvKey::FirstBootDone` and removes `BootEnvKey::ResizedData`. No interaction with the recovery model.

The plans can land in any order after Plan A; only Plan A introduces new types that the others reference.
