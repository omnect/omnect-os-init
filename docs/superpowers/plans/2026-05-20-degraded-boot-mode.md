# Degraded Boot Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When the bootloader environment is unavailable, release-images boot in a degraded state (resize-data runs without guard, `degraded_boot` is set in the ODS runtime JSON), while debug-images immediately enter a debug shell.

**Architecture:** A new `BootloaderEnv` enum replaces `Option<Box<dyn Bootloader>>` across the codebase. A pure `classify_bootloader()` function in `src/bootloader/mod.rs` makes the release-vs-debug image decision at the single point of detection in `lib.rs`. The `preflight::resize_data` step calls `filesystem::resize_data::resize_if_needed` with `Option<&mut dyn Bootloader>`, skipping the guard write when `None`.

**Tech Stack:** Rust, thiserror, serde_json, cargo test with feature flags.

**Spec:** `docs/superpowers/specs/2026-05-19-degraded-boot-mode-design.md`

---

## File Map

| File | Change |
|---|---|
| `src/error.rs` | Add `InitramfsError::DegradedBoot(#[source] BootloaderError)` |
| `src/bootloader/mod.rs` | Add `BootloaderEnv`, `BootloaderDecision`, `classify_bootloader()` |
| `src/lib.rs` | Re-export new types; replace inline match with `classify_bootloader`; update `PreflightCtx` and `BootContext` construction |
| `src/runtime/omnect_device_service.rs` | Add `degraded_boot: bool` to `OdsStatus`; add `set_degraded_boot()` |
| `src/preflight/mod.rs` | Change `PreflightCtx.bootloader` from `Option<&mut Box<dyn Bootloader>>` to `&mut BootloaderEnv` |
| `src/preflight/resize_data.rs` | Match on `BootloaderEnv`; call `resize_if_needed` with `None` on `Degraded` |
| `src/filesystem/resize_data.rs` | Change `bootloader` param to `Option<&mut dyn Bootloader>`; skip `set_env` when `None` |
| `src/mode/mod.rs` | Change `BootContext.bootloader` to `BootloaderEnv`; update `BootMode::detect` |
| `src/mode/normal.rs` | Use `bootloader.available_mut()` instead of `if let Some(ref mut bl)` |
| `tests/degraded_boot.rs` | Integration tests for `classify_bootloader` and `degraded_boot` JSON serialization |

---

## Task 1: Add `InitramfsError::DegradedBoot` variant

**Files:**
- Modify: `src/error.rs`

- [ ] **Step 1: Add the variant**

  In `src/error.rs`, add after the `Bootloader` variant (line 18):

  ```rust
  #[error("degraded boot: {0}")]
  DegradedBoot(#[source] BootloaderError),
  ```

  The full enum block becomes:

  ```rust
  pub enum InitramfsError {
      #[error("Bootloader error: {0}")]
      Bootloader(#[from] BootloaderError),

      #[error("degraded boot: {0}")]
      DegradedBoot(#[source] BootloaderError),

      #[error("Early init error: {0}")]
      EarlyInit(#[from] EarlyInitError),
      // ... rest unchanged
  }
  ```

  Note: `#[source]` (not `#[from]`) — `DegradedBoot` is not auto-converted from `BootloaderError`; the caller constructs it explicitly.

- [ ] **Step 2: Check it compiles**

  ```bash
  cargo check --features grub,gpt
  ```

  Expected: no errors.

- [ ] **Step 3: Commit**

  ```bash
  git add src/error.rs
  git commit -m "feat(error): add DegradedBoot variant to InitramfsError

  Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
  ```

---

## Task 2: Add `BootloaderEnv`, `BootloaderDecision`, `classify_bootloader` to `src/bootloader/mod.rs`

**Files:**
- Modify: `src/bootloader/mod.rs`

- [ ] **Step 1: Write the failing tests first**

  Add to the `#[cfg(test)] mod tests` block at the bottom of `src/bootloader/mod.rs`:

  ```rust
  mod classify_tests {
      use super::*;
      use crate::error::{BootloaderError, InitramfsError};

      fn ok_bootloader() -> std::result::Result<Box<dyn Bootloader>, BootloaderError> {
          Ok(Box::new(MockBootloader::new()))
      }

      fn err_bootloader() -> std::result::Result<Box<dyn Bootloader>, BootloaderError> {
          Err(BootloaderError::CommandFailed {
              command: "grub-editenv".into(),
              reason: "not found".into(),
          })
      }

      #[test]
      fn ok_release_image_returns_available_not_degraded() {
          let decision = classify_bootloader(ok_bootloader(), true);
          assert!(
              matches!(decision, BootloaderDecision::Continue(BootloaderEnv::Available(_), false))
          );
      }

      #[test]
      fn ok_debug_image_returns_available_not_degraded() {
          let decision = classify_bootloader(ok_bootloader(), false);
          assert!(
              matches!(decision, BootloaderDecision::Continue(BootloaderEnv::Available(_), false))
          );
      }

      #[test]
      fn err_release_image_returns_degraded_continue() {
          let decision = classify_bootloader(err_bootloader(), true);
          assert!(
              matches!(decision, BootloaderDecision::Continue(BootloaderEnv::Degraded(_), true))
          );
      }

      #[test]
      fn err_debug_image_returns_abort_with_degraded_boot_error() {
          let decision = classify_bootloader(err_bootloader(), false);
          assert!(matches!(
              decision,
              BootloaderDecision::Abort(InitramfsError::DegradedBoot(_))
          ));
      }
  }
  ```

- [ ] **Step 2: Run tests to verify they fail**

  ```bash
  cargo test --features grub,gpt 2>&1 | grep -E "error\[|classify_bootloader|BootloaderEnv|BootloaderDecision"
  ```

  Expected: compile errors — `classify_bootloader`, `BootloaderEnv`, `BootloaderDecision` not defined.

- [ ] **Step 3: Add `BootloaderEnv`, `BootloaderDecision`, and `classify_bootloader`**

  Add the following after the `open_bootloader_env` function (after line 130 of the original file), before the `#[cfg(test)]` block:

  ```rust
  /// The result of a bootloader availability check.
  pub enum BootloaderEnv {
      /// Bootloader environment opened successfully.
      Available(Box<dyn Bootloader>),
      /// Bootloader environment could not be opened.
      Degraded(BootloaderError),
  }

  impl BootloaderEnv {
      /// Returns `true` if the bootloader environment is unavailable.
      pub fn is_degraded(&self) -> bool {
          matches!(self, Self::Degraded(_))
      }

      /// Returns a mutable reference to the bootloader if available.
      pub fn available_mut(&mut self) -> Option<&mut dyn Bootloader> {
          match self {
              Self::Available(b) => Some(b.as_mut()),
              Self::Degraded(_) => None,
          }
      }

      /// Returns a shared reference to the bootloader if available.
      pub fn available(&self) -> Option<&dyn Bootloader> {
          match self {
              Self::Available(b) => Some(b.as_ref()),
              Self::Degraded(_) => None,
          }
      }
  }

  /// The outcome of `classify_bootloader`.
  pub enum BootloaderDecision {
      /// Continue init with this bootloader env. The bool is `true` iff degraded.
      Continue(BootloaderEnv, bool),
      /// Abort init with this error — caller passes it to `handle_fatal_error`.
      Abort(crate::error::InitramfsError),
  }

  /// Decide how to proceed based on the bootloader open result and the image type.
  ///
  /// - `Ok(bl)` → `Continue(Available(bl), false)` — normal boot, both image types.
  /// - `Err(e)` + release-image → `Continue(Degraded(e), true)` — degraded boot continues.
  /// - `Err(e)` + debug-image → `Abort(DegradedBoot(e))` — enter debug shell immediately.
  pub fn classify_bootloader(
      open_result: std::result::Result<Box<dyn Bootloader>, BootloaderError>,
      is_release_image: bool,
  ) -> BootloaderDecision {
      match open_result {
          Ok(bl) => BootloaderDecision::Continue(BootloaderEnv::Available(bl), false),
          Err(e) if is_release_image => {
              BootloaderDecision::Continue(BootloaderEnv::Degraded(e), true)
          }
          Err(e) => BootloaderDecision::Abort(crate::error::InitramfsError::DegradedBoot(e)),
      }
  }
  ```

- [ ] **Step 4: Run the new tests**

  ```bash
  cargo test --features grub,gpt classify_tests 2>&1
  ```

  Expected: all four `classify_tests::*` tests pass.

- [ ] **Step 5: Run all tests to verify no regression**

  ```bash
  cargo test --features grub,gpt && cargo test --features uboot,dos
  ```

  Expected: all pass.

- [ ] **Step 6: Commit**

  ```bash
  git add src/bootloader/mod.rs
  git commit -m "feat(bootloader): add BootloaderEnv enum and classify_bootloader function

  Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
  ```

---

## Task 3: Add `degraded_boot` to `OdsStatus`

**Files:**
- Modify: `src/runtime/omnect_device_service.rs`

- [ ] **Step 1: Write the failing test**

  Add a new test to the `#[cfg(test)] mod tests` block in `src/runtime/omnect_device_service.rs`. Find the block with `use super::*;` at the bottom and add:

  ```rust
  #[test]
  fn degraded_boot_serializes_only_when_true() {
      let status_normal = OdsStatus::new();
      let json_normal = serde_json::to_string(&status_normal).unwrap();
      assert!(
          !json_normal.contains("degraded_boot"),
          "degraded_boot must be absent when false; got: {json_normal}"
      );

      let mut status_degraded = OdsStatus::new();
      status_degraded.set_degraded_boot();
      let json_degraded = serde_json::to_string(&status_degraded).unwrap();
      assert!(
          json_degraded.contains("\"degraded_boot\":true"),
          "degraded_boot must be present and true; got: {json_degraded}"
      );
  }
  ```

- [ ] **Step 2: Run test to verify it fails**

  ```bash
  cargo test --features grub,gpt degraded_boot_serializes_only_when_true 2>&1
  ```

  Expected: compile error — `set_degraded_boot` not defined.

- [ ] **Step 3: Add the field and setter**

  In `src/runtime/omnect_device_service.rs`, update `OdsStatus` struct:

  ```rust
  #[derive(Debug, Clone, Default, Serialize)]
  pub struct OdsStatus {
      #[serde(skip_serializing_if = "HashMap::is_empty")]
      pub fsck: HashMap<PartitionName, FsckStatus>,

      #[serde(skip_serializing_if = "Option::is_none")]
      pub factory_reset: Option<FactoryResetStatus>,

      #[serde(skip_serializing_if = "std::ops::Not::not")]
      pub degraded_boot: bool,
  }
  ```

  Add the setter in the `impl OdsStatus` block (after `set_factory_reset`):

  ```rust
  /// Mark this boot as degraded (bootloader environment unavailable).
  pub fn set_degraded_boot(&mut self) {
      self.degraded_boot = true;
  }
  ```

- [ ] **Step 4: Run the test**

  ```bash
  cargo test --features grub,gpt degraded_boot_serializes_only_when_true 2>&1
  ```

  Expected: PASS.

- [ ] **Step 5: Run all tests**

  ```bash
  cargo test --features grub,gpt && cargo test --features uboot,dos
  ```

  Expected: all pass.

- [ ] **Step 6: Commit**

  ```bash
  git add src/runtime/omnect_device_service.rs
  git commit -m "feat(runtime): add degraded_boot field to OdsStatus

  Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
  ```

---

## Task 4: Add integration test `tests/degraded_boot.rs`

**Files:**
- Create: `tests/degraded_boot.rs`

- [ ] **Step 1: Create the test file**

  ```rust
  //! Integration tests for degraded boot classification and OdsStatus JSON output.

  use omnect_os_init::bootloader::{BootloaderDecision, BootloaderEnv, classify_bootloader};
  use omnect_os_init::error::{BootloaderError, InitramfsError};
  use omnect_os_init::runtime::OdsStatus;

  fn make_ok() -> Result<Box<dyn omnect_os_init::bootloader::Bootloader>, BootloaderError> {
      // Use the public mock from the library
      Ok(Box::new(omnect_os_init::bootloader::MockBootloader::new()))
  }

  fn make_err() -> Result<Box<dyn omnect_os_init::bootloader::Bootloader>, BootloaderError> {
      Err(BootloaderError::CommandFailed {
          command: "grub-editenv".into(),
          reason: "test error".into(),
      })
  }

  #[test]
  fn ok_result_is_not_degraded_regardless_of_image_type() {
      for is_release in [true, false] {
          let decision = classify_bootloader(make_ok(), is_release);
          assert!(
              matches!(decision, BootloaderDecision::Continue(BootloaderEnv::Available(_), false)),
              "is_release={is_release}: expected Available/not-degraded"
          );
      }
  }

  #[test]
  fn err_release_image_is_degraded_continue() {
      let decision = classify_bootloader(make_err(), true);
      assert!(
          matches!(decision, BootloaderDecision::Continue(BootloaderEnv::Degraded(_), true)),
          "release-image: expected Degraded/true"
      );
  }

  #[test]
  fn err_debug_image_is_abort_with_cause() {
      let decision = classify_bootloader(make_err(), false);
      assert!(
          matches!(decision, BootloaderDecision::Abort(InitramfsError::DegradedBoot(_))),
          "debug-image: expected Abort(DegradedBoot)"
      );
  }

  #[test]
  fn degraded_ods_status_json_contains_flag() {
      let mut status = OdsStatus::new();
      assert!(!serde_json::to_string(&status).unwrap().contains("degraded_boot"));
      status.set_degraded_boot();
      let json = serde_json::to_string(&status).unwrap();
      assert!(
          json.contains("\"degraded_boot\":true"),
          "expected degraded_boot:true in JSON, got: {json}"
      );
  }
  ```

  Note: `MockBootloader` needs to be `pub` in `src/bootloader/mod.rs` for integration tests. Change the `#[cfg(test)]` guard on `MockBootloader` and its `impl` to `#[cfg(any(test, feature = "test-utils"))]` **only** if the integration test cannot access it otherwise. If the compiler complains about `MockBootloader` being unavailable in `tests/`, add the following to `Cargo.toml` under `[features]`:

  ```toml
  test-utils = []
  ```

  And change the guards in `src/bootloader/mod.rs` from `#[cfg(test)]` to `#[cfg(any(test, feature = "test-utils"))]`.

  Then run the integration test with `--features grub,gpt,test-utils`.

- [ ] **Step 2: Re-export `classify_bootloader`, `BootloaderEnv`, `BootloaderDecision`, `MockBootloader` from `src/lib.rs`**

  In `src/lib.rs`, update the bootloader re-exports:

  ```rust
  pub use crate::bootloader::{
      Bootloader, BootloaderDecision, BootloaderEnv, classify_bootloader, open_bootloader_env,
  };
  #[cfg(any(test, feature = "test-utils"))]
  pub use crate::bootloader::MockBootloader;
  ```

  Also add `OdsStatus` re-export for the integration test:

  ```rust
  pub use crate::runtime::OdsStatus;
  ```

- [ ] **Step 3: Run the integration tests**

  ```bash
  cargo test --features grub,gpt,test-utils --test degraded_boot 2>&1
  ```

  Expected: all four tests pass.

- [ ] **Step 4: Run all tests**

  ```bash
  cargo test --features grub,gpt,test-utils && cargo test --features uboot,dos,test-utils
  ```

  Expected: all pass.

- [ ] **Step 5: Commit**

  ```bash
  git add tests/degraded_boot.rs src/lib.rs Cargo.toml src/bootloader/mod.rs
  git commit -m "test(degraded-boot): add integration tests for classify_bootloader and OdsStatus

  Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
  ```

---

## Task 5: Change `resize_if_needed` signature to `Option<&mut dyn Bootloader>`

**Files:**
- Modify: `src/filesystem/resize_data.rs`

- [ ] **Step 1: Write the new test for the `None` path**

  In the `#[cfg(test)] mod tests` block of `src/filesystem/resize_data.rs`, add:

  ```rust
  #[test]
  fn resize_with_none_bootloader_skips_guard_set() {
      // Data partition absent → resize returns Ok without calling set_env.
      // Verifies that the None path does not panic or error when no bootloader
      // is provided — the guard simply can't be written.
      let layout = PartitionLayout {
          partitions: std::collections::HashMap::new(),
          device: crate::partition::RootDevice {
              base: std::path::PathBuf::from("/dev/sda"),
              partition_sep: "",
              root_partition: std::path::PathBuf::from("/dev/sda2"),
          },
      };
      assert!(resize_if_needed(&layout, None).is_ok());
  }
  ```

  Also update the existing `test_resize_skips_when_data_partition_absent` test to pass `Some(bl.as_mut())` instead of `bl.as_mut()`:

  ```rust
  #[test]
  fn test_resize_skips_when_data_partition_absent() {
      use crate::bootloader::MockBootloader;
      use crate::partition::{PartitionLayout, RootDevice};
      use std::collections::HashMap;

      let layout = PartitionLayout {
          partitions: HashMap::new(),
          device: RootDevice {
              base: std::path::PathBuf::from("/dev/sda"),
              partition_sep: "",
              root_partition: std::path::PathBuf::from("/dev/sda2"),
          },
      };
      let mut bl: Box<dyn crate::bootloader::Bootloader> = Box::new(MockBootloader::new());

      assert!(resize_if_needed(&layout, Some(bl.as_mut())).is_ok());
      assert!(bl.get_env(BootloaderEnvKey::ResizedData).unwrap().is_none());
  }
  ```

- [ ] **Step 2: Run tests to verify they fail**

  ```bash
  cargo test --features grub,gpt,resize-data resize 2>&1 | grep -E "error|FAILED"
  ```

  Expected: compile errors from signature mismatch.

- [ ] **Step 3: Update `resize_if_needed` signature**

  In `src/filesystem/resize_data.rs`, change the function signature from:

  ```rust
  pub fn resize_if_needed(
      layout: &crate::partition::PartitionLayout,
      bootloader: &mut (dyn Bootloader + '_),
  ) -> Result<()> {
  ```

  to:

  ```rust
  pub fn resize_if_needed(
      layout: &crate::partition::PartitionLayout,
      bootloader: Option<&mut dyn Bootloader>,
  ) -> Result<()> {
  ```

  And change the terminal `set_env` call (line 103) from:

  ```rust
  bootloader.set_env(BootloaderEnvKey::ResizedData, Some("1"))?;
  ```

  to:

  ```rust
  if let Some(bl) = bootloader {
      bl.set_env(BootloaderEnvKey::ResizedData, Some("1"))?;
  }
  ```

- [ ] **Step 4: Run the tests**

  ```bash
  cargo test --features grub,gpt,resize-data resize 2>&1
  ```

  Expected: all resize tests pass.

- [ ] **Step 5: Run all tests**

  ```bash
  cargo test --features grub,gpt,resize-data && cargo test --features uboot,dos,resize-data
  ```

  Expected: all pass.

- [ ] **Step 6: Commit**

  ```bash
  git add src/filesystem/resize_data.rs
  git commit -m "refactor(resize-data): accept Option<&mut dyn Bootloader> in resize_if_needed

  Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
  ```

---

## Task 6: Update `preflight::mod.rs` and `preflight::resize_data.rs`

**Files:**
- Modify: `src/preflight/mod.rs`
- Modify: `src/preflight/resize_data.rs`

- [ ] **Step 1: Update `PreflightCtx` in `src/preflight/mod.rs`**

  Replace the entire file content with:

  ```rust
  //! Preflight: conditional one-time prep steps that run after core mount
  //! and the bootloader env is open, but before mode dispatch.
  //!
  //! Each step is independently feature-gated and idempotent — guarded by
  //! bootloader env or filesystem state so it runs at most once per trigger.

  #[cfg(feature = "resize-data")]
  pub mod resize_data;

  use crate::{Result, bootloader::BootloaderEnv, partition::PartitionLayout};

  /// Context passed to each preflight step.
  #[non_exhaustive]
  pub struct PreflightCtx<'l, 'b> {
      pub layout: &'l PartitionLayout,
      pub bootloader: &'b mut BootloaderEnv,
  }

  /// Run all enabled preflight steps in order.
  ///
  /// Steps are independent and idempotent. Order is intentional: resize-data
  /// must run before any partition is mounted read-write.
  #[cfg_attr(not(feature = "resize-data"), allow(unused_variables))]
  pub fn run(mut ctx: PreflightCtx<'_, '_>) -> Result<()> {
      #[cfg(feature = "resize-data")]
      resize_data::run(&mut ctx)?;
      Ok(())
  }
  ```

- [ ] **Step 2: Update `preflight::resize_data::run`**

  Replace the entire `run` function and its tests in `src/preflight/resize_data.rs`:

  ```rust
  //! Preflight step: data partition auto-resize
  //!
  //! On a live bootloader: checks the `omnect_resized_data` guard and, if
  //! absent, expands the data partition via `filesystem::resize_data`.
  //!
  //! On a degraded boot (bootloader unavailable): runs resize without the
  //! guard. Only reached on release-images; debug-images abort in lib.rs
  //! before preflight executes.

  use crate::bootloader::{BootloaderEnv, BootloaderEnvKey};
  use crate::error::Result;
  use crate::preflight::PreflightCtx;

  pub fn run(ctx: &mut PreflightCtx<'_, '_>) -> Result<()> {
      match ctx.bootloader.available_mut() {
          Some(bl) => {
              if bl.get_env(BootloaderEnvKey::ResizedData)?.is_some() {
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

  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::bootloader::{BootloaderEnv, BootloaderEnvKey, MockBootloader};
      use crate::error::BootloaderError;
      use crate::partition::{PartitionLayout, RootDevice};
      use crate::preflight::PreflightCtx;
      use std::collections::HashMap;
      use std::path::PathBuf;

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

      fn layout_with_data() -> PartitionLayout {
          let mut partitions = HashMap::new();
          partitions.insert(
              crate::partition::PartitionName::Data,
              PathBuf::from("/dev/sda8"),
          );
          PartitionLayout {
              partitions,
              device: RootDevice {
                  base: PathBuf::from("/dev/sda"),
                  partition_sep: "",
                  root_partition: PathBuf::from("/dev/sda2"),
              },
          }
      }

      #[test]
      fn skips_when_guard_present() {
          // layout_with_data: if the guard check is bypassed, resize_if_needed
          // will attempt to spawn sgdisk/parted (not in test env) and return Err.
          let layout = layout_with_data();
          let mut bl: Box<dyn crate::bootloader::Bootloader> =
              Box::new(MockBootloader::new().with_env(BootloaderEnvKey::ResizedData, "1"));
          let mut env = BootloaderEnv::Available(bl);
          let mut ctx = PreflightCtx {
              layout: &layout,
              bootloader: &mut env,
          };
          assert!(run(&mut ctx).is_ok());
      }

      #[test]
      fn degraded_env_with_empty_layout_returns_ok() {
          // Data partition absent in layout → resize_if_needed returns Ok immediately.
          // Verifies the Degraded arm is reached and does not panic.
          let layout = empty_layout();
          let mut env = BootloaderEnv::Degraded(BootloaderError::CommandFailed {
              command: "grub-editenv".into(),
              reason: "test".into(),
          });
          let mut ctx = PreflightCtx {
              layout: &layout,
              bootloader: &mut env,
          };
          assert!(run(&mut ctx).is_ok());
      }
  }
  ```

- [ ] **Step 3: Run the preflight tests**

  ```bash
  cargo test --features grub,gpt,resize-data preflight 2>&1
  ```

  Expected: both `skips_when_guard_present` and `degraded_env_with_empty_layout_returns_ok` pass.

- [ ] **Step 4: Run all tests**

  ```bash
  cargo test --features grub,gpt,resize-data && cargo test --features uboot,dos,resize-data
  ```

  Expected: all pass.

- [ ] **Step 5: Commit**

  ```bash
  git add src/preflight/mod.rs src/preflight/resize_data.rs
  git commit -m "refactor(preflight): use BootloaderEnv in PreflightCtx; handle degraded resize

  Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
  ```

---

## Task 7: Update `BootContext` and `BootMode` to use `BootloaderEnv`

**Files:**
- Modify: `src/mode/mod.rs`

- [ ] **Step 1: Update `BootContext` and `BootMode::detect` in `src/mode/mod.rs`**

  Replace the full file content with:

  ```rust
  use std::path::Path;

  use crate::{
      Bootloader, BootloaderEnv, Result,
      config::Config,
      partition::PartitionLayout,
      runtime::OdsStatus,
  };

  pub mod normal;

  /// Runtime context passed to the active boot-mode handler.
  pub struct BootContext<'a> {
      pub(crate) config: &'a Config,
      pub(crate) layout: &'a PartitionLayout,
      pub(crate) rootfs: &'a Path,
      pub(crate) bootloader: BootloaderEnv,
      pub(crate) ods_status: OdsStatus,
  }

  impl<'a> BootContext<'a> {
      pub(crate) fn new(
          config: &'a Config,
          layout: &'a PartitionLayout,
          rootfs: &'a Path,
          bootloader: BootloaderEnv,
          ods_status: OdsStatus,
      ) -> Self {
          Self {
              config,
              layout,
              rootfs,
              bootloader,
              ods_status,
          }
      }
  }

  /// The detected boot mode to execute.
  pub enum BootMode {
      Normal,
  }

  impl BootMode {
      /// Detect the boot mode from the bootloader environment.
      pub fn detect(_bl: Option<&dyn Bootloader>) -> Result<Self> {
          Ok(Self::Normal)
      }
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::bootloader::create_mock_bootloader;

      #[test]
      fn detect_normal_with_live_bootloader() {
          let mock = create_mock_bootloader();
          let mode = BootMode::detect(Some(&mock)).unwrap();
          assert!(matches!(mode, BootMode::Normal));
      }

      #[test]
      fn detect_normal_degraded_boot_no_bootloader() {
          let mode = BootMode::detect(None).unwrap();
          assert!(matches!(mode, BootMode::Normal));
      }
  }
  ```

- [ ] **Step 2: Check it compiles**

  ```bash
  cargo check --features grub,gpt 2>&1
  ```

  Expected: no errors (or errors only from the not-yet-updated callers in `lib.rs` and `normal.rs`).

- [ ] **Step 3: Update `src/mode/normal.rs`**

  Change the `if let Some(ref mut bl) = bootloader` blocks to use `bootloader.available_mut()`:

  ```rust
  use std::path::Path;

  use log::info;

  use crate::{
      Result,
      filesystem::{
          mount_remaining_partitions, persist_fsck_results, setup_data_overlay, setup_etc_overlay,
          setup_raw_rootfs_mount,
      },
      mode::BootContext,
      runtime::{ODS_RUNTIME_DIR, create_fs_links, create_ods_runtime_files, switch_root},
  };

  /// Run the normal boot path.
  ///
  /// # Mode obligation: persist fsck results
  ///
  /// This handler (and any future mode handler) is responsible for calling
  /// `persist_fsck_results` after `mount_remaining_partitions`, capturing the
  /// result before propagating any mount error. Skipping this call means fsck
  /// diagnostics for data/factory/cert/etc are silently lost on a failed boot.
  pub fn run(ctx: BootContext<'_>) -> Result<()> {
      let BootContext {
          config,
          layout,
          rootfs,
          mut bootloader,
          mut ods_status,
      } = ctx;

      let mount_result = mount_remaining_partitions(layout, rootfs, &mut ods_status);

      if let Some(bl) = bootloader.available_mut() {
          persist_fsck_results(&ods_status, bl, rootfs);
      }

      mount_result?;

      setup_raw_rootfs_mount(rootfs)?;
      setup_etc_overlay(rootfs)?;
      setup_data_overlay(rootfs)?;
      create_fs_links(rootfs)?;

      create_ods_runtime_files(
          &ods_status,
          bootloader.available(),
          rootfs,
          Path::new(ODS_RUNTIME_DIR),
      )?;

      info!("omnect-os-initramfs completed successfully");

      switch_root(rootfs, &config.cmdline)
  }
  ```

- [ ] **Step 4: Run all tests**

  ```bash
  cargo test --features grub,gpt && cargo test --features uboot,dos
  ```

  Expected: all pass (lib.rs still uses `Option<Box<dyn Bootloader>>` so there may be compile errors; address them in the next task).

- [ ] **Step 5: Commit**

  ```bash
  git add src/mode/mod.rs src/mode/normal.rs
  git commit -m "refactor(mode): use BootloaderEnv in BootContext and normal boot handler

  Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
  ```

---

## Task 8: Wire `classify_bootloader` into `lib.rs`

**Files:**
- Modify: `src/lib.rs`

This is the final wiring task that replaces the existing `match open_bootloader_env()` block and updates all downstream call sites to the new `BootloaderEnv` type.

- [ ] **Step 1: Update imports in `src/lib.rs`**

  Replace the existing `pub use crate::bootloader::{Bootloader, open_bootloader_env};` re-export with:

  ```rust
  pub use crate::bootloader::{
      Bootloader, BootloaderDecision, BootloaderEnv, classify_bootloader, open_bootloader_env,
  };
  ```

  Add `OdsStatus` re-export if not already present:

  ```rust
  pub use crate::runtime::OdsStatus;
  ```

  Update the `use` at the top of `run_init()` — add `BootloaderDecision` and `BootloaderEnv` to the imports:

  ```rust
  use crate::{
      bootloader::{BootloaderDecision, BootloaderEnv, classify_bootloader, open_bootloader_env},
      config::Config,
      filesystem::{mount_core_partitions, persist_fsck_results},
      mode::{BootContext, BootMode},
      partition::{PartitionLayout, create_omnect_symlinks, detect_root_device},
      runtime::OdsStatus,
  };
  ```

- [ ] **Step 2: Replace the `match open_bootloader_env()` block**

  Replace lines 70–84 (the entire `let mut bootloader_opt: Option<...> = match ...` block):

  ```rust
  // Best-effort: open the bootloader environment. The image type determines how
  // to proceed when it is unavailable — see classify_bootloader.
  //
  // Note: if mount_core_partitions returned FsckRequiresReboot, the boot partition
  // may not be mounted (GRUB), causing open_bootloader_env() to fail. core_result?
  // runs inside both branches below, so FsckRequiresReboot is always propagated
  // before DegradedBoot — the reboot invariant is preserved.
  let is_release = cfg!(feature = "release-image");
  let mut bootloader_env: BootloaderEnv =
      match classify_bootloader(open_bootloader_env(), is_release) {
          BootloaderDecision::Continue(mut env, degraded) => {
              if let Some(bl) = env.available_mut() {
                  persist_fsck_results(&ods_status, bl, rootfs);
                  ods_status.fsck.clear();
              }
              core_result?;
              if degraded {
                  warn!("Bootloader environment unavailable; booting in degraded mode");
                  ods_status.set_degraded_boot();
              }
              env
          }
          BootloaderDecision::Abort(err) => {
              core_result?;
              return Err(err);
          }
      };
  ```

- [ ] **Step 3: Update `PreflightCtx` construction**

  Replace:

  ```rust
  {
      let ctx = preflight::PreflightCtx {
          layout: &layout,
          bootloader: bootloader_opt.as_mut(),
      };
      preflight::run(ctx)?;
  }
  ```

  With:

  ```rust
  {
      let ctx = preflight::PreflightCtx {
          layout: &layout,
          bootloader: &mut bootloader_env,
      };
      preflight::run(ctx)?;
  }
  ```

- [ ] **Step 4: Update `BootContext` construction**

  Replace:

  ```rust
  let ctx = BootContext::new(&config, &layout, rootfs, bootloader_opt, ods_status);

  match BootMode::detect(ctx.bootloader.as_deref())? {
      BootMode::Normal => mode::normal::run(ctx),
  }
  ```

  With:

  ```rust
  let ctx = BootContext::new(&config, &layout, rootfs, bootloader_env, ods_status);

  match BootMode::detect(ctx.bootloader.available())? {
      BootMode::Normal => mode::normal::run(ctx),
  }
  ```

- [ ] **Step 5: Build and fix any remaining compile errors**

  ```bash
  cargo build --features grub,gpt 2>&1
  ```

  Address any errors. Common ones:
  - `bootloader_opt` references that need updating to `bootloader_env`
  - Lifetime issues with `BootloaderEnv` in the match expression

- [ ] **Step 6: Run the full test matrix**

  ```bash
  cargo test --features grub,gpt
  cargo test --features grub,dos
  cargo test --features uboot,gpt
  cargo test --features uboot,dos
  cargo test --features grub,gpt,resize-data
  cargo test --features grub,dos,resize-data
  cargo test --features uboot,gpt,resize-data
  cargo test --features uboot,dos,resize-data
  cargo test --features grub,gpt,release-image,test-utils
  cargo test --features grub,dos,release-image,test-utils
  cargo test --features uboot,gpt,release-image,test-utils
  cargo test --features uboot,dos,release-image,test-utils
  cargo test --features grub,gpt,resize-data,release-image,test-utils
  cargo test --features uboot,dos,resize-data,release-image,test-utils
  ```

  Expected: all pass.

- [ ] **Step 7: Run clippy and fmt**

  ```bash
  cargo fmt -- --check
  cargo clippy --tests --features grub,gpt -- -D warnings
  cargo clippy --tests --features grub,gpt,resize-data,release-image -- -D warnings
  ```

  Expected: no warnings or errors.

- [ ] **Step 8: Commit**

  ```bash
  git add src/lib.rs
  git commit -m "feat(init): wire classify_bootloader into run_init; enter debug shell on degraded debug-image

  Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
  ```

---

## Task 9: Final verification

- [ ] **Step 1: Run cargo audit**

  ```bash
  cargo audit
  ```

  Expected: no new vulnerabilities.

- [ ] **Step 2: Run the complete feature matrix one final time**

  ```bash
  for features in \
    "grub,gpt" "grub,dos" "uboot,gpt" "uboot,dos" \
    "grub,gpt,resize-data" "grub,dos,resize-data" "uboot,gpt,resize-data" "uboot,dos,resize-data" \
    "grub,gpt,release-image,test-utils" "grub,dos,release-image,test-utils" \
    "uboot,gpt,release-image,test-utils" "uboot,dos,release-image,test-utils" \
    "grub,gpt,resize-data,release-image,test-utils" "uboot,dos,resize-data,release-image,test-utils"; do
    echo "=== $features ===" && cargo test --features "$features" --quiet 2>&1 | tail -3
  done
  ```

  Expected: `test result: ok` for every combination.

- [ ] **Step 3: Commit the spec and plan if not yet committed**

  ```bash
  git add docs/superpowers/specs/2026-05-19-degraded-boot-mode-design.md \
          docs/superpowers/plans/2026-05-20-degraded-boot-mode.md \
          docs/superpowers/reviews/
  git commit -m "docs: add degraded boot mode spec, plan, and review

  Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
  ```
