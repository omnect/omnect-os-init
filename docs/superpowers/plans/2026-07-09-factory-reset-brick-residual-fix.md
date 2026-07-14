# Factory Reset mkfs-Corruption Brick Residual Fix — Implementation Plan

> **Status: executed, then partly superseded by PR review.** This is the original build plan. Two
> later review-driven changes replaced parts of Tasks 3–6, so their code blocks below no longer
> match the shipped code. The authoritative current design is the spec
> (`docs/superpowers/specs/2026-07-08-factory-reset-brick-residual-fix-design.md`). The changes:
> - **Signal carrier (was Task 3/5/6):** the failure signal is no longer a `#[serde(skip)]`
>   `exhausted_signal` field on `FactoryResetStatus`. It is a named `ResetFailureSignal` returned
>   alongside the status (`run_reset`/`run_destructive_phase` return
>   `(FactoryResetStatus, Option<ResetFailureSignal>)`). See spec §3.5.
> - **Retry model (was Task 4/5):** the retry covers the whole `{mkfs → mount}` step, not just the
>   mount. A `mkfs` failure is retried and signalled too. The function is `reformat_and_mount_with_retry`
>   (two phases: reformat, then mount), not `mount_reformatted_with_retry`. See spec §0/§3.2/§4.
>
> Read the spec, not the Task 3–6 code blocks, for what the code does now.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop a factory reset from silently bricking the device when a freshly reformatted `data`/`etc` partition is unmountable, by self-healing with a bounded reformat retry and persisting a diagnosable failure signal to the bootloader environment.

**Architecture:** After `run_destructive_phase` reformats `data`/`etc`, mounting is driven through a small injectable seam (`ReformatRetryOps`) so a mount/overlay failure triggers exactly one more `mkfs` + retry per partition. If a partition is still unmountable after that, the typed `(partition, reason)` is carried out on the returned `FactoryResetStatus` (non-serialized field) and written to a new bootloader-env key in `run()` before the (still unchanged) handoff to `mode::normal::run`. Bundled hardening items 1/4/5/6 from issue #17 are applied in the same change.

**Tech Stack:** Rust (edition 2021/2024 as configured), `thiserror`, `serde`/`serde_json`, feature-gated compilation (`grub`/`uboot` × `gpt`/`dos` × `factory-reset` × `test-utils`). PID-1 init, synchronous, no async.

**Source spec:** `docs/superpowers/specs/2026-07-08-factory-reset-brick-residual-fix-design.md` (Status: Approved). Read §3.1–§3.6 and §5 before starting.

## Global Constraints

- **Feature gating:** all new factory-reset code is behind `#[cfg(feature = "factory-reset")]`. `MockBootEnv` and the retry unit tests require `test-utils`.
- **Bootloader backend:** exactly one of `grub`/`uboot` is required to build (enforced by `build.rs`); code must not assume a specific backend. The new `save_factory_reset_failure` uses the generic `set_env`, which both backends implement — no backend override.
- **No magic path strings, no magic numbers:** every path and numeric threshold is a named `const` (e.g. `MAX_FACTORY_RESET_FAILURE_REASON_LEN`). Reuse existing `mount_points::*` and the existing `DATA_PARTITION_LABEL`/`ETC_PARTITION_LABEL` constants (`src/mode/factory_reset/mod.rs:32-33`).
- **File organization:** `use`/`const`/`static`/`type` at the top of the file before any `fn`/`impl`/`struct`/`enum` (exception: items inside `#[cfg(test)]`/`#[cfg(feature=…)]` blocks). Absolute paths use `crate::…`; `super::` only for sibling modules (`use super::*` in test modules is the standard exception).
- **Comments explain why, not what.** No references to PRs, "legacy bash", or previous implementations in source comments.
- **Commits:** Conventional Commits (`type(scope): subject`); every commit ends with a `Signed-off-by: Joerg Zeidler <joerg.zeidler@conplement.de>` trailer. **Never add an AI co-author trailer.**
- **Verification per task:** each task ends with `cargo fmt`, the task's tests, and `cargo clippy --tests --features grub,gpt,factory-reset,test-utils -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module` passing. The primary test feature combo is `grub,gpt,factory-reset,test-utils`; the full change must also compile+test under `uboot,dos,factory-reset,test-utils` (run once at the end, Task 10).

---

## File Structure

- `src/bootloader/mod.rs` — add `BootEnvKey::FactoryResetLastError`, the `save_factory_reset_failure` default trait method, the `truncate_on_char_boundary` helper, and `MAX_FACTORY_RESET_FAILURE_REASON_LEN`. (Tasks 1, 2)
- `src/runtime/omnect_device_service.rs` — add the non-serialized `exhausted_signal` field + accessor to `FactoryResetStatus`. (Task 3)
- `src/mode/factory_reset/mod.rs` — `RetryReport`, `ReformatRetryOps`, `mount_reformatted_with_retry`, real ops impl, wiring into `run_destructive_phase`, `run()` persistence. (Tasks 4, 5, 6)
- `src/error.rs` — enumerate `FactoryResetError` in `recovery_class()` (item 5). (Task 7)
- `src/mode/factory_reset/backup_restore.rs` + `src/mode/factory_reset/mod.rs` — backup manifest so `restore_all` reports a lost backup as `PartialFailure` (item 1). (Task 8)
- `src/mode/factory_reset/config.rs` — map config read I/O errors to `FactoryResetError::Io` (item 6); `ResetMode` newtype (item 4). (Tasks 9, 10)

---

## Task 1: `truncate_on_char_boundary` helper + length constant

**Files:**
- Modify: `src/bootloader/mod.rs` (add near the top, after the existing `use` block and before the `BootEnvKey` enum)
- Test: `src/bootloader/mod.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `const MAX_FACTORY_RESET_FAILURE_REASON_LEN: usize = 128;` and `fn truncate_on_char_boundary(s: &str, max_bytes: usize) -> &str` (both `#[cfg(feature = "factory-reset")]`, module-private).

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` in `src/bootloader/mod.rs`:

```rust
#[cfg(feature = "factory-reset")]
mod truncate_tests {
    use super::super::truncate_on_char_boundary;

    #[test]
    fn returns_input_when_within_limit() {
        assert_eq!(truncate_on_char_boundary("short", 128), "short");
    }

    #[test]
    fn truncates_ascii_at_limit() {
        assert_eq!(truncate_on_char_boundary("abcdef", 3), "abc");
    }

    #[test]
    fn never_splits_a_multibyte_char() {
        // "é" is 2 bytes (0xC3 0xA9). Truncating to 1 byte must not split it.
        let s = "aé";
        let out = truncate_on_char_boundary(s, 2);
        assert!(s.is_char_boundary(out.len()));
        assert_eq!(out, "a");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features grub,gpt,factory-reset,test-utils truncate_tests -- --nocapture`
Expected: FAIL — `cannot find function truncate_on_char_boundary`.

- [ ] **Step 3: Add the constant and helper**

In `src/bootloader/mod.rs`, after the `use` block and before `pub enum BootEnvKey`:

```rust
/// Upper bound (bytes) on the failure reason stored in the bootloader env by
/// `save_factory_reset_failure`. grubenv is a fixed ~1024-byte shared block, so
/// the diagnostic is kept short and human-readable rather than exhaustive.
#[cfg(feature = "factory-reset")]
const MAX_FACTORY_RESET_FAILURE_REASON_LEN: usize = 128;

/// Truncate `s` to at most `max_bytes`, never splitting a multi-byte UTF-8
/// character (a naive byte slice would panic on a non-ASCII boundary).
#[cfg(feature = "factory-reset")]
fn truncate_on_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features grub,gpt,factory-reset,test-utils truncate_tests -- --nocapture`
Expected: PASS (3 tests).

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt
cargo clippy --tests --features grub,gpt,factory-reset,test-utils -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module
git add src/bootloader/mod.rs
git commit -s -m "feat(factory-reset): add bounded UTF-8-safe reason truncation helper"
```

---

## Task 2: `BootEnvKey::FactoryResetLastError` + `save_factory_reset_failure`

**Files:**
- Modify: `src/bootloader/mod.rs` (`BootEnvKey` enum ~line 39-55, `as_str` ~line 59-68, `BootEnv` trait after `save_fsck_status`/`clear_fsck_status` ~line 118-121)
- Test: `src/bootloader/mod.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `truncate_on_char_boundary`, `MAX_FACTORY_RESET_FAILURE_REASON_LEN` (Task 1).
- Produces:
  - `BootEnvKey::FactoryResetLastError` (feature-gated) → `as_str()` = `"omnect_factory_reset_last_error"`.
  - `fn save_factory_reset_failure(&mut self, partition: PartitionName, reason: &str) -> Result<()>` — default `BootEnv` trait method; stores `"<partition>:<reason>"` via `set_env`.

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `src/bootloader/mod.rs`:

```rust
#[cfg(feature = "factory-reset")]
mod factory_reset_failure_tests {
    use super::super::{BootEnv, BootEnvKey, MockBootEnv};
    use crate::partition::PartitionName;

    #[test]
    fn key_as_str_is_stable() {
        assert_eq!(
            BootEnvKey::FactoryResetLastError.as_str().as_ref(),
            "omnect_factory_reset_last_error"
        );
    }

    #[test]
    fn save_factory_reset_failure_round_trips() {
        let mut mock = MockBootEnv::new();
        mock.save_factory_reset_failure(PartitionName::Etc, "mkfs retry exhausted")
            .unwrap();
        assert_eq!(
            mock.get_env(BootEnvKey::FactoryResetLastError).unwrap(),
            Some("etc:mkfs retry exhausted".to_string())
        );
    }

    #[test]
    fn save_factory_reset_failure_truncates_long_reason() {
        let mut mock = MockBootEnv::new();
        let long = "x".repeat(500);
        mock.save_factory_reset_failure(PartitionName::Data, &long)
            .unwrap();
        let stored = mock
            .get_env(BootEnvKey::FactoryResetLastError)
            .unwrap()
            .unwrap();
        // "data:" prefix + at most MAX reason bytes.
        assert!(stored.len() <= "data:".len() + MAX_FACTORY_RESET_FAILURE_REASON_LEN);
        assert!(stored.starts_with("data:x"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features grub,gpt,factory-reset,test-utils factory_reset_failure_tests -- --nocapture`
Expected: FAIL — no variant `FactoryResetLastError`, no method `save_factory_reset_failure`.

- [ ] **Step 3: Add the enum variant + `as_str` arm**

In `pub enum BootEnvKey` (after the existing `#[cfg(feature = "factory-reset")] FactoryReset,` variant):

```rust
    #[cfg(feature = "factory-reset")]
    /// `omnect_factory_reset_last_error` — records a partition that stayed
    /// unmountable after a reformat-and-retry during the factory-reset
    /// destructive phase, so the failure survives even if the boot that
    /// follows halts before switch_root. Plain text `"<partition>:<reason>"`.
    FactoryResetLastError,
```

In `as_str()` (after the `#[cfg(feature = "factory-reset")] Self::FactoryReset => …` arm):

```rust
            #[cfg(feature = "factory-reset")]
            Self::FactoryResetLastError => Cow::Borrowed("omnect_factory_reset_last_error"),
```

- [ ] **Step 4: Add the default trait method**

In `pub trait BootEnv`, after `clear_fsck_status`:

```rust
    /// Persist an unrecoverable factory-reset reformat/mount failure.
    ///
    /// Plain text (`"<partition>:<reason>"`), not gzip+base64 like
    /// `save_fsck_status`: the payload is small and bounded, and an operator
    /// should be able to read it directly with `fw_printenv`/`grub-editenv list`
    /// without decoding.
    #[cfg(feature = "factory-reset")]
    fn save_factory_reset_failure(
        &mut self,
        partition: PartitionName,
        reason: &str,
    ) -> Result<()> {
        let reason = truncate_on_char_boundary(reason, MAX_FACTORY_RESET_FAILURE_REASON_LEN);
        self.set_env(
            BootEnvKey::FactoryResetLastError,
            Some(&format!("{partition}:{reason}")),
        )
    }
```

`MockBootEnv` already implements `set_env`, so it inherits this default method — no mock change needed. `PartitionName`'s `Display` yields `"data"`/`"etc"` (verified: `src/partition/layout.rs:45-46,53-57`).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --features grub,gpt,factory-reset,test-utils factory_reset_failure_tests -- --nocapture`
Expected: PASS (3 tests).

- [ ] **Step 6: Verify `BootEnvKey` `as_str` exhaustiveness under uboot too**

Run: `cargo build --features uboot,dos,factory-reset`
Expected: compiles (the new `as_str` arm is feature-gated, not backend-gated).

- [ ] **Step 7: Format, lint, commit**

```bash
cargo fmt
cargo clippy --tests --features grub,gpt,factory-reset,test-utils -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module
git add src/bootloader/mod.rs
git commit -s -m "feat(factory-reset): add bootloader-env signal for unrecoverable reset failure"
```

---

## Task 3: `FactoryResetStatus.exhausted_signal` carrier field + accessor

**Files:**
- Modify: `src/runtime/omnect_device_service.rs` (`FactoryResetStatus` struct ~line 202-220; add accessor in the same `impl` region)
- Modify: `src/mode/factory_reset/mod.rs` (every `FactoryResetStatus { … }` literal: `run()` error path ~line 65-71, `run_destructive_phase` success/partial ~line 174-187, `destructive_phase_failure_status` ~line 201-207) to set `exhausted_signal: None`
- Test: `src/runtime/omnect_device_service.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  - `FactoryResetStatus.exhausted_signal: Option<(PartitionName, String)>` — `#[serde(skip)]`, `#[cfg(feature = "factory-reset")]`.
  - `fn exhausted_signal(&self) -> Option<(PartitionName, &str)>` on `FactoryResetStatus` (feature-gated).
- Consumed by: Task 5 (sets it), Task 6 (reads it).

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `src/runtime/omnect_device_service.rs` (import `PartitionName` as needed):

```rust
#[cfg(feature = "factory-reset")]
#[test]
fn exhausted_signal_is_not_serialized_and_is_readable() {
    use crate::partition::PartitionName;
    let status = FactoryResetStatus {
        status: FactoryResetStatusCode::Error,
        error: Some("boom".into()),
        context: None,
        paths: vec![],
        data_wiped: true,
        exhausted_signal: Some((PartitionName::Etc, "mkfs retry exhausted".into())),
    };
    // Accessor exposes the typed signal.
    assert_eq!(
        status.exhausted_signal(),
        Some((PartitionName::Etc, "mkfs retry exhausted"))
    );
    // #[serde(skip)] keeps it out of the ODS JSON.
    let json = serde_json::to_string(&status).unwrap();
    assert!(!json.contains("exhausted_signal"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features grub,gpt,factory-reset,test-utils exhausted_signal_is_not_serialized -- --nocapture`
Expected: FAIL — struct has no field `exhausted_signal`.

- [ ] **Step 3: Add the field and accessor**

In `pub struct FactoryResetStatus` (after `pub data_wiped: bool,`):

```rust
    /// Set when the destructive phase exhausted its reformat retry on a
    /// partition. Not user-facing status — an internal carrier for the typed
    /// `(partition, reason)`, read by `run()` for the bootloader-env write.
    /// Never serialized to the ODS JSON.
    #[cfg(feature = "factory-reset")]
    #[serde(skip)]
    pub exhausted_signal: Option<(PartitionName, String)>,
```

Add an accessor in the `impl FactoryResetStatus` block (create the `impl` if none exists):

```rust
#[cfg(feature = "factory-reset")]
impl FactoryResetStatus {
    /// The typed exhausted signal, if the destructive phase gave up on a
    /// partition. Consumed by `run()` to write the bootloader-env failure key.
    pub fn exhausted_signal(&self) -> Option<(PartitionName, &str)> {
        self.exhausted_signal
            .as_ref()
            .map(|(p, r)| (*p, r.as_str()))
    }
}
```

(`PartitionName` derives `Copy` — verified `src/partition/layout.rs:18`.)

- [ ] **Step 4: Update every existing `FactoryResetStatus` literal**

In `src/mode/factory_reset/mod.rs`, add `exhausted_signal: None,` to each of the three literals: the `run()` error-path status (~line 65), the `RestoreResult::Success` and `RestoreResult::PartialFailure` arms in `run_destructive_phase` (~line 174, ~line 181), and `destructive_phase_failure_status` (~line 201). Each addition looks like:

```rust
                data_wiped: false,
                exhausted_signal: None,
            }
```

(Match the surrounding `data_wiped` value; only the new line is added.)

- [ ] **Step 5: Run test + full factory-reset suite to verify pass**

Run: `cargo test --features grub,gpt,factory-reset,test-utils factory_reset -- --nocapture`
Expected: PASS, including the new test and all existing factory-reset tests (they now compile with the added field).

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt
cargo clippy --tests --features grub,gpt,factory-reset,test-utils -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module
git add src/runtime/omnect_device_service.rs src/mode/factory_reset/mod.rs
git commit -s -m "feat(factory-reset): carry unrecoverable-failure signal on FactoryResetStatus"
```

---

## Task 4: `RetryReport` + `ReformatRetryOps` + `mount_reformatted_with_retry`

**Files:**
- Modify: `src/mode/factory_reset/mod.rs` (add struct, trait, function, and a partition-resolution helper; add unit tests in the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `ReformatTargets` (existing, `mod.rs:137-142`: `data_dev`, `etc_dev`, `preserve_list`, `backup_dir`), `mount_points::{ETC_PARTITION, DATA_PARTITION}`, `FilesystemError`, `PartitionName`, `DATA_PARTITION_LABEL`/`ETC_PARTITION_LABEL`.
- Produces:
  - `struct RetryReport { retried: Vec<PartitionName>, exhausted: Option<(PartitionName, String)> }`
  - `trait ReformatRetryOps { fn reformat(&mut self, device: &Path, label: &str) -> Result<()>; fn mount_all(&mut self) -> Result<()>; }`
  - `fn mount_reformatted_with_retry(rootfs: &Path, targets: &ReformatTargets, ops: &mut dyn ReformatRetryOps) -> Result<RetryReport>`

**Design note (concretizes the spec's illustrative §3.2 signature):** `mount_reformatted_with_retry` takes only what the retry logic needs — `rootfs` (to build `etc`/`data` mount points for `OverlayFailed.target` matching), `targets` (device paths for `MountFailed.src_path` matching and for the reformat device+label), and the `ops` seam. `layout`/`ods_status`/`mounts` live inside the real `ReformatRetryOps` implementor (Task 5), so they are not parameters here. This keeps the unit tests free of real block devices, which is the entire point of the seam (spec §3.2/§6).

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `src/mode/factory_reset/mod.rs`:

```rust
#[cfg(feature = "factory-reset")]
mod retry_tests {
    use super::*;
    use crate::error::FilesystemError;

    // Programmable ops: each mount_all() call pops the next scripted result.
    struct ScriptedOps {
        mount_results: std::collections::VecDeque<Result<()>>,
        reformatted: Vec<PathBuf>,
    }
    impl ScriptedOps {
        fn new(results: Vec<Result<()>>) -> Self {
            Self {
                mount_results: results.into_iter().collect(),
                reformatted: vec![],
            }
        }
    }
    impl ReformatRetryOps for ScriptedOps {
        fn reformat(&mut self, device: &Path, _label: &str) -> Result<()> {
            self.reformatted.push(device.to_path_buf());
            Ok(())
        }
        fn mount_all(&mut self) -> Result<()> {
            self.mount_results
                .pop_front()
                .expect("mount_all called more times than scripted")
        }
    }

    fn targets<'a>(data: &'a Path, etc: &'a Path, preserve: &'a [String]) -> ReformatTargets<'a> {
        ReformatTargets {
            data_dev: data,
            etc_dev: etc,
            preserve_list: preserve,
            backup_dir: Path::new("/tmp/does-not-matter"),
        }
    }

    fn mount_failed(src: &Path) -> InitramfsError {
        FilesystemError::MountFailed {
            src_path: src.to_path_buf(),
            target: PathBuf::from("/rootfs/mnt/etc"),
            reason: "bad superblock".into(),
        }
        .into()
    }

    fn overlay_failed(target: &Path) -> InitramfsError {
        FilesystemError::OverlayFailed {
            target: target.to_path_buf(),
            reason: "cannot create upperdir".into(),
        }
        .into()
    }

    #[test]
    fn clean_mount_reports_no_retry() {
        let (data, etc) = (Path::new("/dev/sda7"), Path::new("/dev/sda6"));
        let mut ops = ScriptedOps::new(vec![Ok(())]);
        let report =
            mount_reformatted_with_retry(Path::new("/rootfs"), &targets(data, etc, &[]), &mut ops)
                .unwrap();
        assert!(report.retried.is_empty());
        assert!(report.exhausted.is_none());
        assert!(ops.reformatted.is_empty());
    }

    #[test]
    fn etc_recovers_after_one_reformat() {
        let (data, etc) = (Path::new("/dev/sda7"), Path::new("/dev/sda6"));
        // First mount fails on etc, second succeeds.
        let mut ops = ScriptedOps::new(vec![Err(mount_failed(etc)), Ok(())]);
        let report =
            mount_reformatted_with_retry(Path::new("/rootfs"), &targets(data, etc, &[]), &mut ops)
                .unwrap();
        assert_eq!(report.retried, vec![PartitionName::Etc]);
        assert!(report.exhausted.is_none());
        assert_eq!(ops.reformatted, vec![etc.to_path_buf()]);
    }

    #[test]
    fn etc_exhausts_after_second_failure() {
        let (data, etc) = (Path::new("/dev/sda7"), Path::new("/dev/sda6"));
        let mut ops = ScriptedOps::new(vec![Err(mount_failed(etc)), Err(mount_failed(etc))]);
        let report =
            mount_reformatted_with_retry(Path::new("/rootfs"), &targets(data, etc, &[]), &mut ops)
                .unwrap();
        assert_eq!(report.retried, vec![PartitionName::Etc]);
        let (part, _reason) = report.exhausted.expect("must record exhausted");
        assert_eq!(part, PartitionName::Etc);
    }

    #[test]
    fn overlay_failure_on_etc_triggers_reformat() {
        let (data, etc) = (Path::new("/dev/sda7"), Path::new("/dev/sda6"));
        // Real OverlayFailed target is the overlay upper dir UNDER the partition
        // mount point, not the mount point itself.
        let etc_overlay_dir = Path::new("/rootfs").join(mount_points::ETC_PARTITION).join("upper");
        let mut ops = ScriptedOps::new(vec![Err(overlay_failed(&etc_overlay_dir)), Ok(())]);
        let report =
            mount_reformatted_with_retry(Path::new("/rootfs"), &targets(data, etc, &[]), &mut ops)
                .unwrap();
        assert_eq!(report.retried, vec![PartitionName::Etc]);
        assert!(report.exhausted.is_none());
        assert_eq!(ops.reformatted, vec![etc.to_path_buf()]);
    }

    #[test]
    fn unresolvable_failure_propagates_err() {
        let (data, etc) = (Path::new("/dev/sda7"), Path::new("/dev/sda6"));
        // A failure on the factory partition device — matches neither data nor etc.
        let mut ops = ScriptedOps::new(vec![Err(mount_failed(Path::new("/dev/sda4")))]);
        let result =
            mount_reformatted_with_retry(Path::new("/rootfs"), &targets(data, etc, &[]), &mut ops);
        assert!(result.is_err());
        assert!(ops.reformatted.is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features grub,gpt,factory-reset,test-utils retry_tests -- --nocapture`
Expected: FAIL — `RetryReport`, `ReformatRetryOps`, `mount_reformatted_with_retry` not defined.

- [ ] **Step 3: Add the resolution helper, struct, trait, and function**

In `src/mode/factory_reset/mod.rs` (place the `struct`/`trait`/`fn` after `run_destructive_phase`, keeping declarations before test modules). Add the `FilesystemError` import to the existing `crate::error::{…}` use if not already present:

```rust
/// Outcome of the reformat-retry loop, returned so the caller can set the
/// success `context` note and, on exhaustion, the bootloader-env signal.
struct RetryReport {
    /// Partitions that needed one reformat-and-retry (empty on a clean mount).
    retried: Vec<PartitionName>,
    /// Set when the retry was exhausted: the partition and reason to persist.
    exhausted: Option<(PartitionName, String)>,
}

/// Injectable seam over the destructive-phase mount side effects, so the
/// bounded-retry control flow is unit-testable without real block devices.
/// Narrowly scoped to this loop — not a general refactor of the module.
trait ReformatRetryOps {
    fn reformat(&mut self, device: &Path, label: &str) -> Result<()>;
    fn mount_all(&mut self) -> Result<()>;
}

/// Resolve a mount/overlay failure back to the reformatted partition it
/// concerns. `MountFailed` carries the source device (match against the target
/// device paths); `OverlayFailed` carries the mount point (match against the
/// etc/data mount points). Any other error, or a path matching neither
/// reformatted partition (e.g. the read-only `factory` partition), yields None
/// so the caller propagates the error without retrying.
///
/// COUPLING: `src_path` equals `targets.data_dev`/`etc_dev` only because both
/// come from the same `layout.partitions` lookup. If a future change resolves
/// one side to a different path form (e.g. `/dev/omnect/data`), this match
/// silently stops firing — the mocked tests cannot catch that.
fn resolve_failed_partition(
    err: &InitramfsError,
    rootfs: &Path,
    targets: &ReformatTargets,
) -> Option<PartitionName> {
    match err {
        InitramfsError::Filesystem(FilesystemError::MountFailed { src_path, .. }) => {
            if src_path == targets.data_dev {
                Some(PartitionName::Data)
            } else if src_path == targets.etc_dev {
                Some(PartitionName::Etc)
            } else {
                None
            }
        }
        InitramfsError::Filesystem(FilesystemError::OverlayFailed { target, .. }) => {
            // OverlayFailed carries the overlay upper/work dir, which lives under
            // the partition mount point (e.g. mnt/etc/upper, mnt/data/home/upper).
            // Match by prefix, not exact equality against the mount point.
            if target.starts_with(rootfs.join(mount_points::DATA_PARTITION)) {
                Some(PartitionName::Data)
            } else if target.starts_with(rootfs.join(mount_points::ETC_PARTITION)) {
                Some(PartitionName::Etc)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Mount the reformatted partitions, self-healing a single bad reformat per
/// partition before giving up.
///
/// At most one reformat-and-retry per `data`/`etc`: on a mount/overlay failure
/// resolving to a reformatted partition, that partition is re-`mkfs`'d once and
/// the mount retried. A second failure on the same partition abandons the retry
/// and is returned in `RetryReport.exhausted` — NOT propagated as `Err`, so the
/// typed `(partition, reason)` survives out to `run()` (see spec §3.2/§3.5). A
/// failure resolving to neither reformatted partition propagates immediately.
fn mount_reformatted_with_retry(
    rootfs: &Path,
    targets: &ReformatTargets,
    ops: &mut dyn ReformatRetryOps,
) -> Result<RetryReport> {
    let mut retried: Vec<PartitionName> = Vec::new();
    loop {
        match ops.mount_all() {
            Ok(()) => {
                return Ok(RetryReport {
                    retried,
                    exhausted: None,
                });
            }
            Err(e) => {
                let Some(part) = resolve_failed_partition(&e, rootfs, targets) else {
                    return Err(e);
                };
                if retried.contains(&part) {
                    return Ok(RetryReport {
                        retried,
                        exhausted: Some((part, e.to_string())),
                    });
                }
                let (device, label) = match part {
                    PartitionName::Data => (targets.data_dev, DATA_PARTITION_LABEL),
                    PartitionName::Etc => (targets.etc_dev, ETC_PARTITION_LABEL),
                    _ => return Err(e),
                };
                retried.push(part);
                ops.reformat(device, label)?;
            }
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features grub,gpt,factory-reset,test-utils retry_tests -- --nocapture`
Expected: PASS (5 tests).

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt
cargo clippy --tests --features grub,gpt,factory-reset,test-utils -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module
git add src/mode/factory_reset/mod.rs
git commit -s -m "feat(factory-reset): add bounded reformat-retry loop with injectable seam"
```

---

## Task 5: Wire the retry loop into `run_destructive_phase`

**Files:**
- Modify: `src/mode/factory_reset/mod.rs` (`run_destructive_phase` ~line 147-191; add the real `ReformatRetryOps` impl; add a `context`-join helper + test)

**Interfaces:**
- Consumes: `mount_reformatted_with_retry`, `RetryReport`, `ReformatRetryOps` (Task 4); `FactoryResetStatus.exhausted_signal` (Task 3); existing `factory_reset_mount`, `unmount_tracked`, `reformat_ext4`, `restore_all`, `RestoreResult`.
- Produces: `run_destructive_phase` now returns a status whose `exhausted_signal` is set when a partition was unrecoverable, and whose `context` carries a retry note (joined with any restore partial-failure context using `";"`).

**Behavior:** `run_destructive_phase` still calls `reformat_ext4(data)` + `reformat_ext4(etc)` first (the initial reformat). It then drives mounting through `mount_reformatted_with_retry` with a real ops impl. On `report.exhausted == Some`, the reformatted partition is still unmountable, so `restore_all` cannot run: build the failure status (`Error`, `data_wiped: true`, `paths` = preserve list, `exhausted_signal` set) and return it. On `report.exhausted == None`, proceed to `restore_all` as today, then build the success/partial status; if `report.retried` is non-empty, prepend a retry note to `context`.

- [ ] **Step 1: Write the failing test for the context-join helper**

Add to `#[cfg(test)] mod tests` in `src/mode/factory_reset/mod.rs`:

```rust
#[cfg(feature = "factory-reset")]
mod context_join_tests {
    use super::*;

    #[test]
    fn retry_note_only() {
        let out = join_context(Some("etc reformatted twice: initial remount failed".into()), None);
        assert_eq!(
            out.as_deref(),
            Some("etc reformatted twice: initial remount failed")
        );
    }

    #[test]
    fn restore_context_only() {
        let out = join_context(None, Some("etc/hostname:restore".into()));
        assert_eq!(out.as_deref(), Some("etc/hostname:restore"));
    }

    #[test]
    fn both_joined_with_bare_semicolon() {
        let out = join_context(
            Some("etc reformatted twice: initial remount failed".into()),
            Some("etc/hostname:restore".into()),
        );
        assert_eq!(
            out.as_deref(),
            Some("etc reformatted twice: initial remount failed;etc/hostname:restore")
        );
    }

    #[test]
    fn neither_is_none() {
        assert_eq!(join_context(None, None), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features grub,gpt,factory-reset,test-utils context_join_tests -- --nocapture`
Expected: FAIL — `join_context` not defined.

- [ ] **Step 3: Add the `join_context` helper + retry-note builder**

In `src/mode/factory_reset/mod.rs` (declarations region, before test modules):

```rust
/// Combine the optional retry note and the optional restore-partial-failure
/// context into a single `context` string, joined with `";"` — the same
/// separator `restore_all` uses internally (`backup_restore.rs`). Returns the
/// lone value when only one is present, or None when neither is.
fn join_context(retry_note: Option<String>, restore_context: Option<String>) -> Option<String> {
    match (retry_note, restore_context) {
        (Some(a), Some(b)) => Some(format!("{a};{b}")),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Human-readable note for a partition that needed a reformat retry, for the
/// `context` field. Empty `retried` → None.
fn retry_note(retried: &[PartitionName]) -> Option<String> {
    if retried.is_empty() {
        return None;
    }
    let names: Vec<String> = retried.iter().map(|p| p.to_string()).collect();
    Some(format!(
        "{} reformatted twice: initial remount failed",
        names.join(",")
    ))
}
```

- [ ] **Step 4: Add the real `ReformatRetryOps` impl and rewrite `run_destructive_phase`**

Replace the body of `run_destructive_phase` (currently `mod.rs:147-191`). The initial `reformat_ext4(data)`/`reformat_ext4(etc)` stay; the single `factory_reset_mount` + `restore_all` block is replaced by the retry-driven flow:

```rust
/// Real `ReformatRetryOps`: `mount_all` re-runs the full factory mount and, on
/// failure, unmounts what it managed so the next attempt starts clean.
struct RealReformatOps<'a> {
    layout: &'a PartitionLayout,
    rootfs: &'a Path,
    ods_status: &'a mut OdsStatus,
    mounts: &'a mut Vec<PathBuf>,
}

impl ReformatRetryOps for RealReformatOps<'_> {
    fn reformat(&mut self, device: &Path, label: &str) -> Result<()> {
        reformat_ext4(device, label)
    }

    fn mount_all(&mut self) -> Result<()> {
        factory_reset_mount(self.layout, self.rootfs, self.ods_status, self.mounts).inspect_err(
            |_| {
                let _ = unmount_tracked(self.mounts);
            },
        )
    }
}

fn run_destructive_phase(
    layout: &PartitionLayout,
    rootfs: &Path,
    mounts: &mut Vec<PathBuf>,
    ods_status: &mut OdsStatus,
    targets: ReformatTargets,
) -> Result<FactoryResetStatus> {
    reformat_ext4(targets.data_dev, DATA_PARTITION_LABEL)?;
    reformat_ext4(targets.etc_dev, ETC_PARTITION_LABEL)?;

    let report = {
        let mut ops = RealReformatOps {
            layout,
            rootfs,
            ods_status,
            mounts,
        };
        mount_reformatted_with_retry(rootfs, &targets, &mut ops)?
    };

    if let Some((part, reason)) = report.exhausted {
        // The reformatted partition is still unmountable — restore cannot run.
        // Report data-loss and carry the typed signal out for run() (§3.5).
        warn!(
            "factory reset: {part} still unmountable after reformat retry; \
             preserved data lost: {reason}"
        );
        let _ = unmount_tracked(mounts);
        return Ok(FactoryResetStatus {
            status: FactoryResetStatusCode::Error,
            error: Some(reason.clone()),
            context: retry_note(&report.retried),
            paths: targets.preserve_list.to_vec(),
            data_wiped: true,
            exhausted_signal: Some((part, reason)),
        });
    }

    let restore_result = restore_all(rootfs, targets.preserve_list, targets.backup_dir)
        .inspect_err(|_| {
            let _ = unmount_tracked(mounts);
        })?;

    unmount_tracked(mounts)?;

    log::info!("factory-reset complete");

    let note = retry_note(&report.retried);
    let status = match restore_result {
        RestoreResult::Success => FactoryResetStatus {
            status: FactoryResetStatusCode::Success,
            error: None,
            context: note,
            paths: targets.preserve_list.to_vec(),
            data_wiped: true,
            exhausted_signal: None,
        },
        RestoreResult::PartialFailure { context, error } => FactoryResetStatus {
            status: FactoryResetStatusCode::Error,
            error: Some(error),
            context: join_context(note, Some(context)),
            paths: targets.preserve_list.to_vec(),
            data_wiped: true,
            exhausted_signal: None,
        },
    };

    Ok(status)
}
```

(Note: `report` is scoped so the mutable borrow of `mounts`/`ods_status` inside `RealReformatOps` ends before `mounts` is used again by `unmount_tracked`.)

- [ ] **Step 5: Run the factory-reset suite to verify pass**

Run: `cargo test --features grub,gpt,factory-reset,test-utils factory_reset -- --nocapture`
Expected: PASS — `context_join_tests`, `retry_tests`, and all existing factory-reset tests.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt
cargo clippy --tests --features grub,gpt,factory-reset,test-utils -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module
git add src/mode/factory_reset/mod.rs
git commit -s -m "feat(factory-reset): self-heal unmountable reformat via bounded retry"
```

---

## Task 6: Persist the exhausted signal in `run()`

**Files:**
- Modify: `src/mode/factory_reset/mod.rs` (`run()` ~line 39-77; add a test that drives `run` far enough to observe the bootloader write — or, if `run` needs a real layout, test the persistence step in isolation)

**Interfaces:**
- Consumes: `FactoryResetStatus::exhausted_signal()` (Task 3), `BootEnv::save_factory_reset_failure` (Task 2).
- Produces: `run()` writes the bootloader-env failure key (best-effort) whenever the status carries an exhausted signal, before `set_factory_reset` and the `normal::run` handoff.

**Note:** `run()` takes a full `BootContext` and ends by calling `mode::normal::run(ctx)`, which performs real mounts/`switch_root` — not unit-testable directly. Extract the persistence into a small, pure, testable helper `persist_exhausted_signal(status, boot_env)` and call it from `run()`.

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `src/mode/factory_reset/mod.rs`:

```rust
#[cfg(feature = "factory-reset")]
mod persist_signal_tests {
    use super::*;
    use crate::bootloader::{BootEnvKey, BootEnvState, MockBootEnv};

    fn status_with_signal(part: PartitionName) -> FactoryResetStatus {
        FactoryResetStatus {
            status: FactoryResetStatusCode::Error,
            error: Some("exhausted".into()),
            context: None,
            paths: vec![],
            data_wiped: true,
            exhausted_signal: Some((part, "mkfs retry exhausted".into())),
        }
    }

    #[test]
    fn writes_bootloader_key_when_signal_present() {
        let mut env = BootEnvState::Available(Box::new(MockBootEnv::new()));
        persist_exhausted_signal(&status_with_signal(PartitionName::Etc), &mut env);
        let bl = env.available().unwrap();
        assert_eq!(
            bl.get_env(BootEnvKey::FactoryResetLastError).unwrap(),
            Some("etc:mkfs retry exhausted".to_string())
        );
    }

    #[test]
    fn no_write_when_no_signal() {
        let mut status = status_with_signal(PartitionName::Etc);
        status.exhausted_signal = None;
        let mut env = BootEnvState::Available(Box::new(MockBootEnv::new()));
        persist_exhausted_signal(&status, &mut env);
        let bl = env.available().unwrap();
        assert_eq!(bl.get_env(BootEnvKey::FactoryResetLastError).unwrap(), None);
    }

    #[test]
    fn no_panic_on_degraded_env() {
        use crate::error::BootEnvError;
        let mut env = BootEnvState::Degraded(BootEnvError::CommandFailed {
            command: "boot-env-tool".into(),
            reason: "test".into(),
        });
        // Best-effort: degraded env is a no-op, must not panic.
        persist_exhausted_signal(&status_with_signal(PartitionName::Data), &mut env);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features grub,gpt,factory-reset,test-utils persist_signal_tests -- --nocapture`
Expected: FAIL — `persist_exhausted_signal` not defined.

- [ ] **Step 3: Add the helper and call it from `run()`**

In `src/mode/factory_reset/mod.rs`, add the helper (declarations region):

```rust
/// Best-effort write of the unrecoverable-failure signal to the bootloader env,
/// so the outcome survives even if the ensuing Normal boot halts before
/// `create_ods_runtime_files`. A degraded env is a no-op.
fn persist_exhausted_signal(
    status: &FactoryResetStatus,
    boot_env: &mut crate::bootloader::BootEnvState,
) {
    if let Some((part, reason)) = status.exhausted_signal()
        && let Some(bl) = boot_env.available_mut()
        && let Err(e) = bl.save_factory_reset_failure(part, reason)
    {
        warn!("failed to persist factory-reset failure signal: {e}");
    }
}
```

In `run()`, between building `status` and `ctx.ods_status.set_factory_reset(status)`:

```rust
    persist_exhausted_signal(&status, &mut ctx.boot_env);
    ctx.ods_status.set_factory_reset(status);

    crate::mode::normal::run(ctx)
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features grub,gpt,factory-reset,test-utils persist_signal_tests -- --nocapture`
Expected: PASS (3 tests).

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt
cargo clippy --tests --features grub,gpt,factory-reset,test-utils -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module
git add src/mode/factory_reset/mod.rs
git commit -s -m "feat(factory-reset): persist unrecoverable failure signal before Normal handoff"
```

---

## Task 7: Enumerate `FactoryResetError` in `recovery_class()` (item 5)

**Files:**
- Modify: `src/error.rs` (`recovery_class()` ~line 79-80; the `recovery_class_tests` module ~line 397+)

**Interfaces:**
- Produces: `recovery_class()` matches each `FactoryResetError` variant explicitly (all → `RecoveryClass::ContinueDegraded`) instead of the `Self::FactoryReset(_)` wildcard, so a future variant is a compile error until classified.

- [ ] **Step 1: Write the failing test**

Add to `mod recovery_class_tests` in `src/error.rs`:

```rust
    #[cfg(feature = "factory-reset")]
    #[test]
    fn factory_reset_reformat_error_is_continue_degraded() {
        let err = InitramfsError::FactoryReset(FactoryResetError::ReformatFailed {
            device: std::path::PathBuf::from("/dev/sda6"),
            reason: "mkfs failed".into(),
        });
        assert_eq!(err.recovery_class(), RecoveryClass::ContinueDegraded);
    }

    #[cfg(feature = "factory-reset")]
    #[test]
    fn factory_reset_mount_error_is_continue_degraded() {
        let err =
            InitramfsError::FactoryReset(FactoryResetError::MountError("no data".into()));
        assert_eq!(err.recovery_class(), RecoveryClass::ContinueDegraded);
    }
```

- [ ] **Step 2: Run tests to verify they pass against current code (wildcard already covers them)**

Run: `cargo test --features grub,gpt,factory-reset,test-utils recovery_class_tests -- --nocapture`
Expected: PASS — the existing `_` wildcard already returns `ContinueDegraded`. (These tests lock the behavior in before the refactor.)

- [ ] **Step 3: Replace the wildcard with an explicit enumeration**

In `recovery_class()`, replace:

```rust
            #[cfg(feature = "factory-reset")]
            Self::FactoryReset(_) => RecoveryClass::ContinueDegraded,
```

with an explicit match over every `FactoryResetError` variant:

```rust
            #[cfg(feature = "factory-reset")]
            Self::FactoryReset(
                FactoryResetError::InvalidConfig(_)
                | FactoryResetError::MissingField(_)
                | FactoryResetError::BackupFailed { .. }
                | FactoryResetError::RestoreFailed { .. }
                | FactoryResetError::ReformatFailed { .. }
                | FactoryResetError::MountError(_)
                | FactoryResetError::Io(_),
            ) => RecoveryClass::ContinueDegraded,
```

- [ ] **Step 4: Run tests to verify they still pass**

Run: `cargo test --features grub,gpt,factory-reset,test-utils recovery_class_tests -- --nocapture`
Expected: PASS (existing + 2 new).

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt
cargo clippy --tests --features grub,gpt,factory-reset,test-utils -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module
git add src/error.rs
git commit -s -m "refactor(error): enumerate FactoryResetError variants in recovery_class"
```

---

## Task 8: Report a lost backup as `PartialFailure` (item 1)

**Files:**
- Modify: `src/mode/factory_reset/backup_restore.rs` (`backup_all` ~line 16-22, `restore_all` ~line 29-53; tests)
- Modify: `src/mode/factory_reset/mod.rs` (`ReformatTargets` ~line 137-142; `run_reset` ~line 100-128 to thread the manifest)

**Interfaces:**
- Produces:
  - `backup_all(rootfs, preserve_list, backup_dir) -> Result<Vec<String>>` — returns the subset of paths actually backed up (source existed at backup time).
  - `restore_all(rootfs, backed_up, backup_dir) -> Result<RestoreResult>` — iterates the manifest; a manifest entry whose backup is missing at restore time is a `PartialFailure` (not a silent skip).
  - `ReformatTargets.backed_up: &'a [String]` — the manifest, added alongside `preserve_list`.

**Why:** Today `restore_path` silently skips a missing backup (`backup_restore.rs:97-100`), so if the tmpfs backup is lost between backup and restore (the accepted item-2 gap), `restore_all` returns `Success` while nothing was restored. Iterating the *manifest of what was actually backed up* turns that into a visible `PartialFailure` without false-positiving on paths that legitimately never existed on the device. `paths` on the status stays the full `preserve_list` (intent); the manifest only drives restore + detection.

- [ ] **Step 1: Write the failing tests**

Replace/extend the tests in `src/mode/factory_reset/backup_restore.rs`. Add:

```rust
    #[test]
    fn backup_all_returns_only_paths_that_existed() {
        let temp = TempDir::new().unwrap();
        let rootfs = temp.path().join("rootfs");
        let backup = temp.path().join("backup");
        fs::create_dir_all(rootfs.join("etc")).unwrap();
        fs::write(rootfs.join("etc/hostname"), "host").unwrap();

        let preserve = vec!["/etc/hostname".to_string(), "/etc/absent".to_string()];
        let manifest = backup_all(&rootfs, &preserve, &backup).unwrap();
        assert_eq!(manifest, vec!["/etc/hostname".to_string()]);
    }

    #[test]
    fn restore_all_reports_partial_failure_when_backed_up_file_vanished() {
        let temp = TempDir::new().unwrap();
        let rootfs = temp.path().join("rootfs");
        let backup = temp.path().join("backup");
        fs::create_dir_all(&rootfs).unwrap();
        fs::create_dir_all(&backup).unwrap();

        // Manifest claims /etc/hostname was backed up, but the backup dir is empty
        // (simulates the tmpfs backup lost between backup and restore).
        let manifest = vec!["/etc/hostname".to_string()];
        let result = restore_all(&rootfs, &manifest, &backup).unwrap();
        assert!(matches!(result, RestoreResult::PartialFailure { .. }));
        if let RestoreResult::PartialFailure { context, .. } = result {
            assert!(context.contains("etc/hostname:missing-backup"));
        }
    }

    #[test]
    fn restore_all_success_when_manifest_empty() {
        let temp = TempDir::new().unwrap();
        let rootfs = temp.path().join("rootfs");
        let backup = temp.path().join("backup");
        fs::create_dir_all(&rootfs).unwrap();
        fs::create_dir_all(&backup).unwrap();
        let result = restore_all(&rootfs, &[], &backup).unwrap();
        assert!(matches!(result, RestoreResult::Success));
    }
```

Update the existing `restore_path_restores_file_from_backup` and `restore_all_partial_failure_accumulates_context` tests to pass a manifest containing `"/etc/hostname"` where they previously passed the preserve list (same value; the signature/semantics are unchanged for present backups).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features grub,gpt,factory-reset,test-utils backup_restore -- --nocapture`
Expected: FAIL — `backup_all` returns `()` not `Vec<String>`; `restore_all` still skips missing backups.

- [ ] **Step 3: Make `backup_all` return the manifest**

In `backup_restore.rs`, change `backup_all` to collect the paths it actually copied. `backup_path` currently returns `Ok(())` and skips a nonexistent source — change it to return `Ok(bool)` (true = copied, false = skipped), or keep `backup_path` and check existence in `backup_all`. Minimal version:

```rust
/// Backup all preserve-list paths from rootfs into backup_dir. Returns the
/// subset of paths actually backed up (those whose source existed) — the
/// manifest `restore_all` uses to detect a backup lost before restore.
pub fn backup_all(
    rootfs: &Path,
    preserve_list: &[String],
    backup_dir: &Path,
) -> Result<Vec<String>> {
    std::fs::create_dir_all(backup_dir)?;
    let mut backed_up = Vec::new();
    for path in preserve_list {
        if backup_path(rootfs, path, backup_dir)? {
            backed_up.push(path.clone());
        }
    }
    Ok(backed_up)
}
```

Change `backup_path` to return `Result<bool>`:

```rust
fn backup_path(rootfs: &Path, path: &str, backup_dir: &Path) -> Result<bool> {
    let src = rootfs.join(path.trim_start_matches('/'));

    if !src.exists() {
        log::info!("backup: {path} does not exist; skipping");
        return Ok(false);
    }
    // ... existing cp + status check unchanged ...
    run_sync()?;
    Ok(true)
}
```

- [ ] **Step 4: Make `restore_all` iterate the manifest and flag missing backups**

```rust
/// Restore all backed-up paths from backup_dir back into rootfs.
///
/// `backed_up` is the manifest returned by `backup_all`. A manifest entry whose
/// backup is missing at restore time is reported as `PartialFailure` (the
/// backup was lost between backup and restore), not silently skipped.
pub fn restore_all(
    rootfs: &Path,
    backed_up: &[String],
    backup_dir: &Path,
) -> Result<RestoreResult> {
    let mut error_context: Vec<String> = Vec::new();
    let mut last_error: Option<String> = None;

    for path in backed_up {
        let backup_src = backup_dir
            .join(rootfs.strip_prefix("/").unwrap_or(rootfs))
            .join(path.trim_start_matches('/'));
        if !backup_src.exists() {
            log::warn!("restore: backup for {path} is missing; preserved data lost");
            error_context.push(format!("{}:missing-backup", path.trim_start_matches('/')));
            last_error = Some(format!("backup missing for {path}"));
            continue;
        }
        if let Err(e) = restore_path(rootfs, path, backup_dir) {
            log::warn!("restore failed for {path}: {e}");
            error_context.push(format!("{}:restore", path.trim_start_matches('/')));
            last_error = Some(e.to_string());
        }
    }

    if let Some(error) = last_error {
        Ok(RestoreResult::PartialFailure {
            context: error_context.join(";"),
            error,
        })
    } else {
        Ok(RestoreResult::Success)
    }
}
```

(`restore_path` keeps its existing internal `!backup_src.exists()` guard as a harmless extra check; the manifest loop is what produces the `PartialFailure`.)

- [ ] **Step 5: Thread the manifest through `ReformatTargets` and `run_reset`**

In `src/mode/factory_reset/mod.rs`, add to `ReformatTargets`:

```rust
struct ReformatTargets<'a> {
    data_dev: &'a Path,
    etc_dev: &'a Path,
    preserve_list: &'a [String],
    backed_up: &'a [String],
    backup_dir: &'a Path,
}
```

In `run_reset`, capture the manifest and pass it in:

```rust
    let backup_dir = PathBuf::from(FACTORY_RESET_BACKUP_DIR);
    let backed_up = backup_all(rootfs, &preserve_list, &backup_dir).inspect_err(|_| {
        let _ = unmount_tracked(&mut mounts);
    })?;

    unmount_tracked(&mut mounts)?;
    // ... data_dev / etc_dev lookups unchanged ...

    match run_destructive_phase(
        layout,
        rootfs,
        &mut mounts,
        ods_status,
        ReformatTargets {
            data_dev,
            etc_dev,
            preserve_list: &preserve_list,
            backed_up: &backed_up,
            backup_dir: &backup_dir,
        },
    ) {
```

In `run_destructive_phase`, change the `restore_all` call to pass the manifest:

```rust
    let restore_result = restore_all(rootfs, targets.backed_up, targets.backup_dir)
```

(`status.paths` stays `targets.preserve_list.to_vec()` — intent, not manifest.)

- [ ] **Step 6: Run the factory-reset suite to verify pass**

Run: `cargo test --features grub,gpt,factory-reset,test-utils factory_reset -- --nocapture`
Expected: PASS — updated `backup_restore` tests plus all mod.rs tests compile with the new `ReformatTargets` field and `backup_all` return type.

- [ ] **Step 7: Format, lint, commit**

```bash
cargo fmt
cargo clippy --tests --features grub,gpt,factory-reset,test-utils -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module
git add src/mode/factory_reset/backup_restore.rs src/mode/factory_reset/mod.rs
git commit -s -m "feat(factory-reset): report a lost backup as PartialFailure instead of silent success"
```

---

## Task 9: Map config read I/O errors to `FactoryResetError::Io` (item 6)

**Files:**
- Modify: `src/mode/factory_reset/config.rs` (the three `std::fs::read_to_string`/`read_dir` sites that currently map to `InvalidConfig`: `build_preserve_list` ~line 58-63, `collect_application_paths` ~line 117-119 and ~line 136-138; tests)

**Interfaces:**
- Produces: a filesystem *read* failure (file unreadable, path is a directory, read_dir error) in config loading now yields `FactoryResetError::Io` (classified by `run()` as `FactoryResetStatusCode::Error`), while *JSON parse* failures stay `FactoryResetError::InvalidConfig` (`Invalid`). This stops a config I/O error from being mislabeled `Invalid` in ODS.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/mode/factory_reset/config.rs`:

```rust
    #[test]
    fn build_preserve_list_read_error_maps_to_io_not_invalid_config() {
        // A directory where a readable file is expected makes read_to_string fail
        // with an I/O error (not a JSON parse error).
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("etc/omnect/factory-reset.d");
        fs::create_dir_all(&dir).unwrap();
        // app.json is itself a directory → read_to_string returns Err(io).
        fs::create_dir_all(dir.join("app.json")).unwrap();

        let cfg = FactoryResetConfig {
            mode: ResetMode::MODE_1, // updated in Task 10; use `1.try_into().unwrap()` until then
            preserve: vec!["applications".into()],
        };
        let error = build_preserve_list(&cfg, temp.path()).unwrap_err();
        assert!(matches!(
            error,
            crate::error::InitramfsError::FactoryReset(FactoryResetError::Io(_))
        ));
    }
```

**Note:** Task 9 runs before Task 10, so at this point `FactoryResetConfig.mode` is still `u32` — write `mode: 1` in this test, and Task 10 updates it along with the others.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features grub,gpt,factory-reset,test-utils build_preserve_list_read_error -- --nocapture`
Expected: FAIL — the read error is currently mapped to `InvalidConfig`.

- [ ] **Step 3: Map read failures to `Io`, keep parse failures as `InvalidConfig`**

In `collect_application_paths`, the `read_to_string(&path)` mapping (~line 136-138) becomes:

```rust
        let content = std::fs::read_to_string(&path).map_err(|e| {
            FactoryResetError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to read {}: {e}", path.display()),
            ))
        })?;
```

Apply the same transformation to:
- `read_dir(&dir)` (~line 117-119) and the per-entry `entry.map_err(...)` (~line 125-130) in `collect_application_paths`,
- `read_to_string(&config_file)` in `build_preserve_list` (~line 58-63).

Leave every `serde_json::from_str(...).map_err(|e| … InvalidConfig …)` unchanged — parse failures remain `InvalidConfig`.

- [ ] **Step 4: Run test to verify it passes; confirm the JSON-parse test still expects InvalidConfig**

Run: `cargo test --features grub,gpt,factory-reset,test-utils config -- --nocapture`
Expected: PASS — the new Io test passes, and `build_preserve_list_applications_invalid_json_returns_invalid_config` still passes (parse errors unchanged).

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt
cargo clippy --tests --features grub,gpt,factory-reset,test-utils -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module
git add src/mode/factory_reset/config.rs
git commit -s -m "fix(factory-reset): classify config read I/O errors as Io, not InvalidConfig"
```

---

## Task 10: `ResetMode` newtype rejecting unsupported modes at parse time (item 4)

**Files:**
- Modify: `src/mode/factory_reset/config.rs` (`FactoryResetConfig` ~line 14-19; add `ResetMode`; tests ~line 168-200)
- Modify: `src/mode/factory_reset/mod.rs` (`run_reset` mode check ~line 89-93; remove `SUPPORTED_RESET_MODE` ~line 28; remove the now-impossible `run_reset_rejects_unsupported_mode_before_touching_layout` test ~line 284-300)
- Modify: `src/mode/mod.rs` (detect test `detect_factory_reset_when_key_present_valid_json` ~line 112-114)
- Modify: any test literal that constructed `FactoryResetConfig { mode: 1, .. }` (config.rs tests, `mod.rs::tests::empty_layout`-adjacent test, the Task 9 test)

**Interfaces:**
- Produces: `ResetMode` (a validated newtype over `u32`) with `#[serde(try_from = "u32")]`; `FactoryResetConfig.mode: ResetMode`. Only mode `1` is representable — deserializing any other value fails, so an unsupported-mode trigger fails `FactoryResetConfig::parse` and `BootMode::detect` falls back to `Normal` (consistent with the existing "invalid JSON → Normal" behavior).

**Behavior change (intended):** an unsupported `mode` value no longer produces a `factory_reset: {status: Invalid}` ODS record — it is treated like any other unrecognized trigger and boots Normal. This matches the existing precedent for invalid-JSON triggers and is the point of parse-time rejection.

- [ ] **Step 1: Write the failing tests**

In `src/mode/factory_reset/config.rs` tests, replace `parse_with_preserve_keys` (which used `mode:2`) and add ResetMode tests:

```rust
    #[test]
    fn parse_rejects_unsupported_mode() {
        assert!(FactoryResetConfig::parse(r#"{"mode":2,"preserve":[]}"#).is_err());
        assert!(FactoryResetConfig::parse(r#"{"mode":0,"preserve":[]}"#).is_err());
    }

    #[test]
    fn parse_accepts_mode_1() {
        let cfg = FactoryResetConfig::parse(r#"{"mode":1,"preserve":["applications"]}"#).unwrap();
        assert_eq!(cfg.mode, ResetMode::MODE_1);
        assert_eq!(cfg.preserve, vec!["applications"]);
    }

    #[test]
    fn reset_mode_try_from_rejects_non_one() {
        assert!(ResetMode::try_from(2u32).is_err());
        assert!(ResetMode::try_from(1u32).is_ok());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features grub,gpt,factory-reset,test-utils config -- --nocapture`
Expected: FAIL — `ResetMode` not defined; `mode: 2` currently parses fine.

- [ ] **Step 3: Add the `ResetMode` newtype and change the field**

In `src/mode/factory_reset/config.rs` (top-of-file `const` / type region):

```rust
/// The only factory-reset mode currently supported (backup / reformat / restore).
const SUPPORTED_RESET_MODE: u32 = 1;

/// Validated factory-reset mode. Only `SUPPORTED_RESET_MODE` is representable;
/// any other value is rejected at deserialize time, so an unsupported trigger
/// never reaches the reset sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[serde(try_from = "u32")]
pub struct ResetMode(u32);

impl ResetMode {
    pub const MODE_1: ResetMode = ResetMode(SUPPORTED_RESET_MODE);
}

impl TryFrom<u32> for ResetMode {
    type Error = String;
    fn try_from(value: u32) -> std::result::Result<Self, Self::Error> {
        if value == SUPPORTED_RESET_MODE {
            Ok(ResetMode(value))
        } else {
            Err(format!("factory reset mode {value} is not supported"))
        }
    }
}
```

Add `Deserialize` to the derive on `ResetMode` (it needs `#[derive(Deserialize)]` too — combine: `#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]`). Change the config struct:

```rust
#[derive(Debug, Deserialize)]
pub struct FactoryResetConfig {
    pub mode: ResetMode,
    #[serde(default)]
    pub preserve: Vec<String>,
}
```

- [ ] **Step 4: Remove the now-dead runtime mode check + its test**

In `src/mode/factory_reset/mod.rs`:
- Delete the `const SUPPORTED_RESET_MODE: u32 = 1;` (~line 28) — it now lives in `config.rs`.
- Delete the mode guard at the top of `run_reset` (~line 89-93: the `if config.mode != SUPPORTED_RESET_MODE { … return Err(InvalidConfig) }` block). The type guarantees `mode == MODE_1`.
- Delete the `run_reset_rejects_unsupported_mode_before_touching_layout` test (~line 284-300) — an unsupported mode can no longer reach `run_reset`.
- In `run()`'s error-classification match (~line 54-64), the `InvalidConfig` arm remains valid (still produced by `build_preserve_list`), so leave it.

- [ ] **Step 5: Update remaining `FactoryResetConfig` literals and the detect test**

- In `src/mode/mod.rs` `detect_factory_reset_when_key_present_valid_json` (~line 113): change `assert_eq!(config.mode, 1);` to `assert_eq!(config.mode, crate::mode::factory_reset::config::ResetMode::MODE_1);`.
- In every `FactoryResetConfig { mode: 1, .. }` / `{ mode: 2, .. }` test literal across `config.rs` and the Task 9 test: change to `mode: ResetMode::MODE_1`. (There is no `mode: 2` literal left after removing `parse_with_preserve_keys`; any test that needs an unsupported mode now goes through `parse` and asserts `is_err`.)

- [ ] **Step 6: Run the full factory-reset suite**

Run: `cargo test --features grub,gpt,factory-reset,test-utils -- --nocapture`
Expected: PASS across `config`, `mode`, `factory_reset`, `bootloader`, `error` modules.

- [ ] **Step 7: Verify the other feature combinations build and test**

Run:
```bash
cargo test --features uboot,dos,factory-reset,test-utils
cargo test --features grub,gpt,test-utils            # factory-reset OFF still compiles
```
Expected: both PASS (the feature-off build must still compile — all new code is `#[cfg(feature = "factory-reset")]`).

- [ ] **Step 8: Format, lint, commit**

```bash
cargo fmt
cargo clippy --tests --features grub,gpt,factory-reset,test-utils -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module
cargo clippy --tests --features uboot,dos,factory-reset,test-utils -- -D warnings
git add src/mode/factory_reset/config.rs src/mode/factory_reset/mod.rs src/mode/mod.rs
git commit -s -m "feat(factory-reset): reject unsupported reset modes at parse time via ResetMode newtype"
```

---

## Final verification

- [ ] **Run every CI feature combination that touches factory-reset:**

```bash
cargo test --features grub,gpt,factory-reset,test-utils
cargo test --features grub,dos,factory-reset,test-utils
cargo test --features uboot,gpt,factory-reset,test-utils
cargo test --features uboot,dos,factory-reset,test-utils
cargo test --features grub,gpt,test-utils
cargo fmt -- --check
cargo clippy --tests --features grub,gpt,factory-reset,test-utils -- -D warnings -W clippy::items_after_statements -W clippy::items_after_test_module
```
Expected: all PASS / clean.

- [ ] **Confirm the end-to-end behavior against the spec §4 data flow** (manual/review): a partition that recovers after one reformat boots normally with a `context` retry note; a partition still unmountable after retry yields `data_wiped: true` + `exhausted_signal`, writes `omnect_factory_reset_last_error` to the bootloader env, then hands off to Normal boot (which may still halt on the genuinely dead partition — accepted residual §2.2).

---

## Self-Review (completed during authoring)

- **Spec coverage:** §3.1 → Tasks 1-2; §3.2 → Task 4; §3.3 → Task 5 (`join_context`/`retry_note`); §3.4/item 5 → Task 7; §3.5 → Tasks 3 + 6; §3.6 (`OverlayFailed`) → Task 4 (`resolve_failed_partition`); §5 item 1 → Task 8; item 4 → Task 10; item 6 → Task 9. Items 2/3/7 are won't-do (no task, by design).
- **Type consistency:** `RetryReport { retried, exhausted }`, `ReformatRetryOps { reformat, mount_all }`, `exhausted_signal()` accessor, `ResetMode::MODE_1`, `backup_all -> Vec<String>` / `restore_all(rootfs, backed_up, backup_dir)`, `ReformatTargets.backed_up` are used identically across the tasks that define and consume them.
- **Signature refinement flagged:** `mount_reformatted_with_retry(rootfs, targets, ops)` trims the spec's illustrative parameter list (spec body was `/* ... */`); the removed params live in `RealReformatOps`. Behavior (RetryReport, Ok-on-exhausted, OverlayFailed coverage) matches the spec exactly.
- **Behavior change flagged (Task 10):** unsupported `mode` → Normal boot with no `Invalid` ODS record; documented as intended and consistent with existing invalid-trigger handling.
