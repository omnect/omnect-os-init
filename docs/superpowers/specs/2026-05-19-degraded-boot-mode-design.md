# Design: Degraded Boot Mode (v2)

**Date:** 2026-05-19
**Status:** Approved
**Predecessor:** reviewed in `docs/superpowers/reviews/review-degraded-boot-plan.md`.

---

## 1. Motivation

When `open_bootloader_env()` fails in `src/lib.rs`, the system already logs
"booting in degraded mode" and sets `bootloader_opt = None`. Three issues
remain unresolved:

1. **Silent skip on release-image.** `preflight::resize_data` silently skips
   resize when the bootloader is unavailable. On first boot this means the
   data partition is never expanded — a permanent, undetected defect.
2. **No notification to the rootfs.** The running OS has no way to know the
   device booted without a bootloader environment. `omnect-device-service`
   (ODS) cannot report the condition to the cloud.
3. **Debug-image boots silently into a broken state.** A developer
   debugging a device with a broken bootloader env receives no shell, no
   indication of the problem, and a partially-functional system.

## 2. Requirements

| Condition | Image type | Required behaviour |
| --- | --- | --- |
| Bootloader env unavailable | release-image | Boot continues; `degraded_boot` flag written to ODS runtime JSON; `resize-data` runs without the bootloader guard |
| Bootloader env unavailable | debug-image | Debug shell entered immediately; no further init steps run |
| Bootloader env unavailable **and** `mount_core_partitions` returned `FsckRequiresReboot` | either | Reboot is triggered as today; `FsckRequiresReboot` always wins over `DegradedBoot` |

## 3. Architecture

### 3.1 Detection point

Degraded boot is detected once, at the single point where the bootloader
unavailability is first known: the `Err` arm of `open_bootloader_env()` in
`src/lib.rs::run_init`. All handling is concentrated there.

### 3.2 Bootloader environment type

Replace `Option<Box<dyn Bootloader>>` with an explicit enum in
`src/bootloader/mod.rs`:

```rust
pub enum BootloaderEnv {
    Available(Box<dyn Bootloader>),
    Degraded(BootloaderError),
}

impl BootloaderEnv {
    pub fn is_degraded(&self) -> bool {
        matches!(self, Self::Degraded(_))
    }

    pub fn available_mut(&mut self) -> Option<&mut dyn Bootloader> {
        match self {
            Self::Available(b) => Some(b.as_mut()),
            Self::Degraded(_) => None,
        }
    }
}
```

**Important.** `BootloaderEnv` does **not** implement `Bootloader` for the
`Degraded` variant. A null-object that silently no-ops `set_env` would
reintroduce the exact bug this design is fixing: the preflight guard would
appear written, and the next normal boot would not see it. Variants stay
asymmetric so the compiler forces each call site to decide.

### 3.3 Decision function (testable in isolation)

The degraded-boot decision is extracted into a pure function so it can be
covered by unit and integration tests without invoking `run_init`:

```rust
// src/bootloader/mod.rs
pub enum BootloaderDecision {
    /// Continue init with this bootloader env. The bool is true iff degraded.
    Continue(BootloaderEnv, bool),
    /// Abort init with this error (caller will hit handle_fatal_error).
    Abort(InitramfsError),
}

pub fn classify_bootloader(
    open_result: std::result::Result<Box<dyn Bootloader>, BootloaderError>,
    is_release_image: bool,
) -> BootloaderDecision;
```

Behaviour:
- `Ok(bl)` → `Continue(Available(bl), false)`
- `Err(e)` and `is_release_image` → `Continue(Degraded(e), true)`
- `Err(e)` and not `is_release_image` → `Abort(InitramfsError::DegradedBoot(e))`

### 3.4 Boot flow

```
run_init()
  open_bootloader_env()
  → classify_bootloader(result, cfg!(feature = "release-image"))
      Continue(env, degraded) →
        if degraded { ods_status.set_degraded_boot(); }
        // existing path: persist fsck (if env.available_mut()), then core_result?,
        // then preflight, mode dispatch, switch_root
      Abort(err) →
        return Err(err) → handle_fatal_error → spawn_debug_shell()
```

**Precedence of `FsckRequiresReboot`.** `mount_core_partitions` runs *before*
`open_bootloader_env`. Its `Err(FsckRequiresReboot)` value is captured in
`core_result` and stored locally. The flow is:

```
core_result = mount_core_partitions(...)
match classify_bootloader(open_bootloader_env(), is_release) {
    Continue(env, degraded) => {
        if let Some(bl) = env.available_mut() { persist_fsck_results(...); }
        core_result?;                  // ← FsckRequiresReboot reboots here
        if degraded { ods_status.set_degraded_boot(); }
        // continue
    }
    Abort(err) => {
        core_result?;                  // ← FsckRequiresReboot reboots here too
        return Err(err);               // only reached when fsck was fine
    }
}
```

This preserves the existing invariant that `FsckRequiresReboot` always
triggers a reboot, even on a debug-image, regardless of bootloader state.

### 3.5 `resize-data` under degraded boot (release-image only)

`preflight::resize_data::run()` receives the `BootloaderEnv`. On debug-image
the run never reaches preflight (`Abort` taken in `run_init`), so this code
path is only exercised on release-image. The step:

1. Reads the `ResizedData` guard if `env.available_mut()` is `Some`.
2. Calls `resize_if_needed(layout, env.available_mut())` — the
   filesystem-layer function now takes `Option<&mut dyn Bootloader>` and
   skips `set_env(ResizedData)` when `None`.

Idempotency across degraded boots:
- `sgdisk -e` (GPT only): safe on an already-good backup header.
- `parted resizepart 100%`: safe no-op when the partition already fills
  free space.
- `resize2fs -f`: safe no-op on an already-full filesystem.
- `check_filesystem` (`src/filesystem/resize_data.rs:94`): runs every
  degraded boot. Fast on a clean ext4 but **not** a no-op.

### 3.6 ODS interaction (release-image only)

`create_ods_runtime_files` accepts the existing `Option<&dyn Bootloader>`
unchanged. Trade-off worth documenting:

> Under degraded boot, neither `omnect_validate_update` nor
> `omnect_bootloader_updated` trigger files are written, because they
> require the bootloader env. If the degraded boot occurs inside an A/B
> update validation window, the rootfs validation timer will treat the
> update as failed.

The `degraded_boot` JSON field is **inert until `omnect-device-service` is
updated** to read it and report the condition via the cloud twin. This is
tracked separately.

## 4. Component changes

### 4.1 `src/error.rs`

```rust
#[error("degraded boot: {0}")]
DegradedBoot(#[source] BootloaderError),
```

`#[source]` preserves the original cause for diagnostics.

### 4.2 `src/bootloader/mod.rs`

- Add `BootloaderEnv` enum.
- Add `BootloaderDecision` enum.
- Add `classify_bootloader` decision function.
- Re-export both from `lib.rs`.

### 4.3 `src/runtime/omnect_device_service.rs` — `OdsStatus`

```rust
#[serde(skip_serializing_if = "std::ops::Not::not")]
pub degraded_boot: bool,
```

```rust
pub fn set_degraded_boot(&mut self) {
    self.degraded_boot = true;
}
```

Field is serialized into `omnect-os-initramfs.json` only when `true`.

### 4.4 `src/lib.rs`

Replace the inline `match open_bootloader_env()` block with a call to
`classify_bootloader`, propagate `Abort`, and downstream consumers
(`preflight::run`, `BootContext`, mode handlers) accept `BootloaderEnv`
instead of `Option<Box<dyn Bootloader>>`.

Update the comment at `src/lib.rs:64–69` to reflect that
`FsckRequiresReboot` is propagated *after* `classify_bootloader` (it no
longer relies on `open_bootloader_env()` itself failing).

### 4.5 `src/preflight/mod.rs`

- `PreflightCtx.bootloader` becomes `&'b mut BootloaderEnv` (removes the
  `&'b mut Option<Box<dyn Bootloader>>` and the
  `#[allow(clippy::borrowed_box)]` workaround).

### 4.6 `src/preflight/resize_data.rs`

```rust
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
            // Reached only on release-image (debug-image aborted in lib.rs).
            log::warn!("resize-data: running without bootloader guard (degraded boot)");
            crate::filesystem::resize_data::resize_if_needed(ctx.layout, None)
        }
    }
}
```

### 4.7 `src/filesystem/resize_data.rs`

Signature:
```rust
pub fn resize_if_needed(
    layout: &PartitionLayout,
    bootloader: Option<&mut dyn Bootloader>,
) -> Result<()>;
```

The terminal `set_env(ResizedData)` becomes:
```rust
if let Some(bl) = bootloader {
    bl.set_env(BootloaderEnvKey::ResizedData, Some("1"))?;
}
```

### 4.8 Call sites

- `src/lib.rs` — replace inline match with `classify_bootloader`.
- `src/mode/normal.rs` — destructure `BootloaderEnv` instead of
  `Option<Box<dyn Bootloader>>`; `persist_fsck_results` still receives
  `Option<&mut dyn Bootloader>` via `env.available_mut()`.
- `src/runtime/omnect_device_service.rs::create_ods_runtime_files` — caller
  passes `env.available()` (read-only accessor symmetrical to
  `available_mut`).

## 5. Error handling

| Case | Behaviour |
| --- | --- |
| `open_bootloader_env` fails, release-image, fsck OK | `degraded_boot = true`; boot continues |
| `open_bootloader_env` fails, release-image, `FsckRequiresReboot` returned earlier | Reboot (existing behaviour) |
| `open_bootloader_env` fails, debug-image, fsck OK | `Err(DegradedBoot)` → debug shell |
| `open_bootloader_env` fails, debug-image, `FsckRequiresReboot` returned earlier | Reboot (preserves existing invariant) |
| `resize_if_needed` fails in degraded mode | Propagated as fatal error (unchanged) |
| Data partition absent in layout | Logged as warning; resize skipped (unchanged) |

## 6. Testing

### 6.1 Unit tests on `classify_bootloader`

In `src/bootloader/mod.rs::tests`:

- `Ok(bl)` + release → `Continue(Available, false)`
- `Ok(bl)` + debug → `Continue(Available, false)`
- `Err(_)` + release → `Continue(Degraded, true)` (cause preserved)
- `Err(_)` + debug → `Abort(DegradedBoot)` (cause preserved via `#[source]`)

### 6.2 Unit tests on `resize_if_needed` and `write_resize_guard`

In `src/filesystem/resize_data.rs::tests`:

- `bootloader = None` + Data partition absent → returns `Ok`, no command
  invocations.
- `bootloader = Some(MockBootloader::new())` + Data partition absent →
  returns `Ok`, guard **not** set (verifies `set_env` is only reached when
  resize actually ran).
- `write_resize_guard(None)` → `Ok`, no side effects (verifies the degraded-
  mode guard-skip is explicit, not accidental).
- `write_resize_guard(Some(bl))` → `ResizedData` key written to `bl`.

The guard write is extracted into `pub(crate) write_resize_guard` so it can
be tested independently of the resize commands (which require real block
devices).

### 6.3 Unit test on `preflight::resize_data`

Two tests:

1. `degraded_env_skips_guard_write` — uses `empty_layout()` + `BootloaderEnv::Degraded`.
   Data partition absent → returns `Ok`. Verifies the Degraded arm is dispatched
   correctly without panicking. The `empty_layout()` concession is intentional:
   `layout_with_data()` would invoke real `sgdisk`/`parted` commands, which are
   CI/Concourse-only. The guard-write skip is separately verified in §6.2.

2. `degraded_env_with_data_layout_guard_not_written` — uses `layout_with_data()` +
   `BootloaderEnv::Degraded`. The resize commands will fail on a non-existent device
   (`/dev/sda8`), but the error type is asserted to be `ResizeDataError` (command
   failure), **not** a `BootloaderError` — confirming the guard-write path was never
   reached.

### 6.4 Integration test

Add `tests/degraded_boot.rs`:

- Calls `classify_bootloader` via the public API for all four
  `(open_result, is_release)` combinations.
- For the `Continue(Degraded, true)` case, builds an `OdsStatus`, calls
  `set_degraded_boot`, serializes via `serde_json`, asserts the rendered
  JSON contains `"degraded_boot": true` and omits the key when
  `degraded_boot == false`.

`run_init` itself remains untested in `cargo test` (cannot run as PID 1
under cargo). End-to-end verification belongs in the omnect-os Concourse
pipeline (see §6.6).

### 6.5 Feature matrix

`test-utils` must be included in every combination so that
`tests/degraded_boot.rs` (which requires `MockBootloader`) is compiled
and run. Without it, `cargo test` silently skips the integration test
binary.

```
cargo test --features grub,gpt,test-utils
cargo test --features grub,dos,test-utils
cargo test --features uboot,gpt,test-utils
cargo test --features uboot,dos,test-utils
cargo test --features grub,gpt,resize-data,test-utils
cargo test --features grub,dos,resize-data,test-utils
cargo test --features uboot,gpt,resize-data,test-utils
cargo test --features uboot,dos,resize-data,test-utils
cargo test --features grub,gpt,release-image,test-utils
cargo test --features grub,dos,release-image,test-utils
cargo test --features uboot,gpt,release-image,test-utils
cargo test --features uboot,dos,release-image,test-utils
cargo test --features grub,gpt,resize-data,release-image,test-utils
cargo test --features uboot,dos,resize-data,release-image,test-utils
```

### 6.6 End-to-end (out of scope for this repo)

In the omnect-os Concourse pipeline, add a smoke test that:

1. Builds a release image.
2. Corrupts the bootloader env (truncate `grubenv` for GRUB; wipe the
   U-Boot env partition for U-Boot).
3. Boots the image.
4. Asserts `/run/omnect-device-service/omnect-os-initramfs.json` contains
   `"degraded_boot": true`.
5. Asserts the data partition is at expected (full) size after first boot.

## 7. Out of scope

- Any change to `omnect-device-service` itself (tracked separately).
- Recovery from a degraded boot (re-initialising the bootloader env at
  runtime). The current design surfaces the condition; remediation is a
  later task.
- Persisting `degraded_boot` across reboots. Each boot independently
  decides whether it is degraded; ODS sends the per-boot state to the
  cloud.
