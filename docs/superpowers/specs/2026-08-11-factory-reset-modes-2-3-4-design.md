# Factory Reset Modes 2, 3, 4 — Design

**Date:** 2026-08-11
**Status:** Approved

## 1. Overview

Factory reset mode 1 (backup / reformat / restore) is implemented. This design
adds the wipe modes from the legacy bash script:

| Mode | Meaning                            | Implementation (this design)          |
| ---- | ---------------------------------- | ------------------------------------- |
| 2    | overwrite `etc` and `data` with random data (slow, better privacy) | native Rust write loop over the whole device |
| 3    | discard all blocks of `etc` and `data` (fast, needs hardware discard support) | `BLKDISCARD` ioctl |
| 4    | custom wipe hook                   | run `/opt/factory_reset/custom-wipe`  |

The wipe runs between backup/unmount and reformat. A wipe failure is a
warning: the reset always continues with reformat + restore, so the device
stays usable.

### 1.1 Deliberate changes vs the legacy script

These must be listed in the PR description:

- **Mode 2:** the legacy `dd` skipped the first 2048 bytes to keep the fs
  label. The Rust init finds partitions by partition number, never by label,
  so the wipe covers the whole device from byte 0. Same privacy or better.
- **Mode 3:** legacy did mount → `rm -rf *` → `fstrim` → unmount, which trims
  only the blocks freed by `rm` and leaves fs-journal remnants. New: one
  `BLKDISCARD` ioctl discards every block of the partition. This also fixes a
  legacy bug where a failed mount silently counted as a successful wipe.
  On hardware without discard support the ioctl fails cleanly → warning
  (legacy `fstrim` had the same hardware requirement). Discard remains a hint
  on some disks — the "no total privacy guarantee" note in the meta-omnect
  README stays true.
- **Mode 4:** unchanged contract. Same hook path, no arguments, partitions
  unmounted at call time — existing customer bbappends keep working.
- **No dependency changes in the initramfs image:** no `fstrim`/`blkdiscard`
  binary needed; `dd` no longer used for the wipe.

### 1.2 Userland (ODS) contract

No change. ODS already sends numeric modes 1–4 in the trigger
(`Serialize_repr`), and the result schema (status codes 0–4, optional
`error`/`context`, `paths`, `data_wiped`) is unchanged — ODS PR #207 parses
it. The wipe-failure note travels in the existing free-text `context` field.
The only observable delta is intended: a mode-2/3/4 trigger now performs a
reset (status 0/4) instead of failing with status 1.

## 2. Component Changes

### 2.1 `src/mode/factory_reset/config.rs`

- `ResetMode` gains `Mode2 = 2`, `Mode3 = 3`, `Mode4 = 4`.
- `TryFrom<u32>` accepts 1–4; everything else stays rejected (status
  `Invalid`). Mode stays number-only: the omnect-os CI branch
  (`feature_rust_init`) already sends numbers.

### 2.2 New: `src/mode/factory_reset/wipe.rs`

```rust
/// Path of the customer-provided mode-4 hook, installed into the initramfs
/// image by a Yocto bbappend.
const CUSTOM_WIPE_PATH: &str = "/opt/factory_reset/custom-wipe";
/// Chunk size for the mode-2 random overwrite.
const WIPE_CHUNK_SIZE: usize = 1024 * 1024;
```

- `wipe_random(device: &Path) -> Result<()>` — mode 2. Query the device size
  (`BLKGETSIZE64` ioctl), then stream `/dev/urandom` in `WIPE_CHUNK_SIZE`
  chunks over the whole device; log progress to kmsg every
  `WIPE_PROGRESS_LOG_INTERVAL` bytes (named const, 1 GiB); sync at the end.
- `wipe_discard(device: &Path) -> Result<()>` — mode 3. `BLKGETSIZE64` +
  `BLKDISCARD` ioctls, defined via `nix` ioctl macros (nix 0.29 is already a
  dependency; no new crates).
- `run_custom_wipe() -> Result<()>` — mode 4. `Command::new(CUSTOM_WIPE_PATH)`
  with no arguments. Missing binary, spawn error, or non-zero exit → error.
- Testability split: the mode-2 overwrite loop takes an open file + length so
  it is unit-testable against a temp file; `wipe_random` is the thin
  block-device wrapper (size query + call).
- New error variant in `FactoryResetError`, e.g.
  `WipeFailed { device: PathBuf, reason: String }`.

### 2.3 `src/mode/factory_reset/mod.rs`

Flow change in the reset sequence:

```
mount → preserve list → backup → unmount
  → wipe (mode 2/3/4; mode 1: no wipe step)   ← destructive phase starts here
  → reformat + mount with retry (existing)
  → restore → unmount
```

- The wipe is dispatched on `config.mode` and wipes the `etc` and `data`
  devices (same `layout.partitions` lookups as reformat). A failure on one
  device does not skip the other; failures are collected.
- The destructive boundary moves: for modes 2–4 any failure at or after the
  wipe reports `data_wiped: true` (a half-written random overwrite destroys
  data even if reformat never runs). Mode 1 keeps today's boundary
  (first reformat).
- Wipe failures never abort: they become a wipe note (e.g.
  `"wipe of data failed: <reason>"`), collected per device and joined with
  the existing `CONTEXT_SEPARATOR`.
- Injectable ops trait (same pattern as `ReformatRetryOps`) so the dispatch
  and continue-on-failure control flow is unit-testable without block
  devices.

### 2.4 Status mapping

Existing precedence (Error > Warning > Success) extended by the wipe note:

| Wipe | Reformat/restore | Status | Note placement |
| ---- | ---------------- | ------ | -------------- |
| ok / mode 1 | ok | Success (0) | — |
| failed | ok | Warning (4) | wipe note in `context` |
| failed | retried reformat, ok | Warning (4) | retry note + wipe note joined in `context` |
| any | mkfs failed twice / restore partial failure | Error (2) | wipe note joined into `context`; existing `error` semantics unchanged |

Power loss during a wipe: the trigger is already cleared, so the next boot is
a Normal boot on a corrupt partition — caught by the existing fsck/mount
error paths. Same exposure as the legacy script; no new mechanism. The
existing reformat-retry and `FactoryResetLastError` machinery is untouched.

## 3. Testing

- **Config:** modes 2/3/4 accepted; 0, 5, and string `"2"` rejected.
- **`wipe.rs`:**
  - overwrite loop against a temp file: full length overwritten, content is
    not the previous content, no short-write truncation.
  - custom wipe with temp scripts: exit 0 → Ok; exit 1 → Err; missing file →
    Err.
  - `BLKDISCARD`/`BLKGETSIZE64` wrappers stay thin and untested (need a real
    block device); their call sites are covered through the ops trait mock.
- **`mod.rs`:** mode 1 never calls wipe; wipe warning → Warning status; wipe
  failure + restore partial failure → Error with joined context; etc-wipe
  failure still wipes data (continue-on-failure).
- On-device verification runs via the user's private Concourse team with the
  omnect-os `feature_rust_init` CI branch.

## 4. CI and documentation follow-ups (other repos)

- omnect-os CI covers only mode 1 today; nothing breaks. Optional follow-up
  on the `feature_rust_init` branch: add mode-2 and mode-3 test runs (mode 4
  needs a customer bbappend, not generically testable).
- meta-omnect README: rewrite the mode table to describe behaviour instead of
  tools ("2 = overwrite with random data (slow)", "3 = discard all blocks
  (fast, needs hardware discard support)") — part of the migration PR (#685
  chain), not this repo.
- PR description in this repo documents all behaviour changes vs the legacy
  script (section 1.1).
