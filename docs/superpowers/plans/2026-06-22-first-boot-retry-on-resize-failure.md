# First-Boot Retry on Resize Failure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Suppress the `FirstBootDone` marker write when a resize-data failure was absorbed, so the next boot retries resize.

**Architecture:** Single conditional added to `mode::normal::run` before the `write_first_boot_marker` call. `ods_status.resize_data.is_none()` is the signal: `None` = no failure (write marker); `Some(...)` = failure absorbed (suppress marker). The condition is `#[cfg(feature = "resize-data")]` gated so non-resize builds are unaffected.

**Tech Stack:** Rust, `cfg(feature = "resize-data")`, existing `OdsStatus.resize_data: Option<ResizeStatus>`.

**Worktree:** `.worktrees/feat/plan-d-first-boot-detection`

---

## Task 1: Add resize-ok guard to marker write in `mode::normal::run`

**Files:**
- Modify: `src/mode/normal.rs`

- [ ] **Step 1.1: Write the failing test — skips marker when resize failed**

Add to `mod marker_writer_tests` in `src/mode/normal.rs`. The test calls `run`-level logic
indirectly by checking the new `resize_ok` gate through `write_first_boot_marker`. Because
`write_first_boot_marker` itself only takes a `bool`, the gate lives one level up in `run`.
Test it at the level of the helper by verifying the combined flag.

The cleanest approach is a helper `should_write_first_boot_marker` that encodes the
condition, tested directly. Add it as a private fn:

```rust
#[cfg(feature = "resize-data")]
fn resize_succeeded(ods_status: &crate::runtime::OdsStatus) -> bool {
    ods_status.resize_data.is_none()
}

#[cfg(not(feature = "resize-data"))]
fn resize_succeeded(_ods_status: &crate::runtime::OdsStatus) -> bool {
    true
}
```

First write the two new tests (they will fail to compile until the function exists):

```rust
#[cfg(feature = "resize-data")]
#[test]
fn skips_marker_when_resize_failed() {
    use crate::runtime::{OdsStatus, ResizeOutcome, ResizeStatus};
    let mock = MockBootEnv::new();
    let mut env = BootEnvState::Available(Box::new(mock));
    let mut ods = OdsStatus::new();
    ods.first_boot = true;
    ods.set_resize_status(ResizeStatus {
        outcome: ResizeOutcome::ToolError,
        reason: "test failure".into(),
    });
    // resize failed → resize_succeeded == false → marker must NOT be written
    write_first_boot_marker(ods.first_boot && super::resize_succeeded(&ods), &mut env);
    let bl = env.available().unwrap();
    assert_eq!(
        bl.get_env(BootEnvKey::FirstBootDone).unwrap(),
        None,
        "marker must not be written when resize failed"
    );
}

#[cfg(feature = "resize-data")]
#[test]
fn writes_marker_when_resize_succeeded() {
    use crate::runtime::OdsStatus;
    let mock = MockBootEnv::new();
    let mut env = BootEnvState::Available(Box::new(mock));
    let mut ods = OdsStatus::new();
    ods.first_boot = true;
    // resize_data is None → resize_succeeded == true → marker must be written
    write_first_boot_marker(ods.first_boot && super::resize_succeeded(&ods), &mut env);
    let bl = env.available().unwrap();
    assert_eq!(
        bl.get_env(BootEnvKey::FirstBootDone).unwrap(),
        Some("1".to_string()),
        "marker must be written when resize succeeded"
    );
}
```

- [ ] **Step 1.2: Run tests to verify they fail**

```bash
cd .worktrees/feat/plan-d-first-boot-detection
cargo test --features grub,gpt,resize-data,test-utils skips_marker_when_resize_failed 2>&1 | tail -10
```

Expected: compile error — `super::resize_succeeded` not found.

- [ ] **Step 1.3: Add `resize_succeeded` helper and wire into `run`**

In `src/mode/normal.rs`, add before `fn write_first_boot_marker`:

```rust
#[cfg(feature = "resize-data")]
fn resize_succeeded(ods_status: &crate::runtime::OdsStatus) -> bool {
    ods_status.resize_data.is_none()
}

#[cfg(not(feature = "resize-data"))]
fn resize_succeeded(_ods_status: &crate::runtime::OdsStatus) -> bool {
    true
}
```

Then update the call in `run` from:

```rust
    // Single write point for the unified first-boot sentinel. Best-effort:
    // failure is logged and does not abort the boot — the next boot retries.
    write_first_boot_marker(ods_status.first_boot, &mut boot_env);
```

to:

```rust
    // Single write point for the unified first-boot sentinel. Best-effort:
    // failure is logged and does not abort the boot — the next boot retries.
    // Suppress the write when resize failed so the next boot retries resize.
    write_first_boot_marker(ods_status.first_boot && resize_succeeded(&ods_status), &mut boot_env);
```

- [ ] **Step 1.4: Run new tests to verify they pass**

```bash
cd .worktrees/feat/plan-d-first-boot-detection
cargo test --features grub,gpt,resize-data,test-utils skips_marker_when_resize_failed writes_marker_when_resize_succeeded 2>&1 | tail -10
```

Expected: both tests pass.

- [ ] **Step 1.5: Run fmt + clippy + full test matrix**

```bash
cd .worktrees/feat/plan-d-first-boot-detection
cargo fmt -- --check && \
cargo clippy --tests --features grub,gpt -- -D warnings && \
cargo clippy --tests --features uboot,dos -- -D warnings && \
cargo clippy --tests --features grub,gpt,resize-data -- -D warnings && \
cargo clippy --tests --features uboot,dos,resize-data -- -D warnings && \
cargo test --features grub,gpt,resize-data,test-utils 2>&1 | grep -E "test result|FAILED"
```

Expected: fmt ok, all clippy clean, all tests pass.

- [ ] **Step 1.6: Commit**

```bash
cd .worktrees/feat/plan-d-first-boot-detection
SIGNOFF="$(git config user.name) <$(git config user.email)>"
git add src/mode/normal.rs
git commit -m "fix(normal-mode): suppress FirstBootDone marker when resize fails

Add resize_succeeded() helper (cfg-gated on resize-data feature) that
returns false when ods_status.resize_data is Some — i.e. a resize
failure was absorbed. The marker write is conditioned on both
first_boot and resize_succeeded, so a failed resize leaves
omnect_first_boot_done unset and the next boot retries the resize.

When resize-data is not compiled in, resize_succeeded() returns true
unconditionally, preserving prior behaviour for those builds.

Signed-off-by: $SIGNOFF"
```

---

## Verification

After the commit, push and confirm all four feature combos are clean:

```bash
cd .worktrees/feat/plan-d-first-boot-detection
cargo test --features grub,gpt,test-utils 2>&1 | grep "test result"
cargo test --features uboot,dos,test-utils 2>&1 | grep "test result"
cargo test --features grub,gpt,resize-data,test-utils 2>&1 | grep "test result"
cargo test --features uboot,dos,resize-data,test-utils 2>&1 | grep "test result"
```

All should show `0 failed`.
