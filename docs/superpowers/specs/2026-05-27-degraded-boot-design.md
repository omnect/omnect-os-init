# Design: Degraded Boot

**Date:** 2026-05-27
**Status:** Draft for review (revised after substantial in-repo work landed)
**Scope:** omnect-os-init — `runtime::OdsStatus.degraded_boot`,
`bootloader::classify_boot_env` + `BootEnvState`, `lib::apply_boot_env_decision`,
and in-code uses of the term "degraded."
**Depends on:** Plan A (Boot Failure & Recovery Policy). Coordinates with Plan B
(fsck) on the `OdsStatus`-anomaly-signal pattern.

---

## 1. Problem (residual)

The original three problems from earlier drafts of this spec have been
largely addressed in-repo. Residual gaps:

- **The shipped `OdsStatus.degraded_boot` is a `bool`** (`set_degraded_boot()`
  sets it to `true`; serialized only when true). ODS receives a "degraded
  happened" signal but **no cause**. A cloud-side operator cannot tell from the
  JSON whether the env file was missing, a tool failed, or the env data was
  invalid — the underlying `BootEnvError::Display` is logged in initramfs but
  not transported. This loses parity with Plan B's `resize_data: Option<ResizeStatus { outcome, reason }>`
  pattern.
- **"Degraded" is still overloaded in code comments.** `boot_sequence.rs`
  ("A degraded boot with a corrupted partition…") uses the term for
  lenient-mount-of-corrupt-partition, while `lib.rs` / `bootloader::mod.rs` use
  it for env-unavailable. Two distinct conditions, one word.

The structural pieces this spec was *originally* going to add are already in
place — see §3 for what is documented (contract) vs §2 for what changes.

---

## 2. Field upgrade: `bool` → `Option<DegradedBootStatus>`

### 2.1 Shape

Replace the shipped field

```rust
pub degraded_boot: bool,
pub fn set_degraded_boot(&mut self);
```

with the struct-with-reason form that mirrors Plan B's `ResizeStatus`:

```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub degraded_boot: Option<DegradedBootStatus>,

pub struct DegradedBootStatus {
    pub reason: String,
}

impl OdsStatus {
    pub fn set_degraded_boot(&mut self, reason: String);
}
```

- `reason` is the `Display` of the `BootEnvError` returned by `open_boot_env()`
  (e.g. `"BootEnv file not found: /rootfs/boot/EFI/BOOT/grubenv"`). One line.
- Serialization stays out of the JSON on the happy path (`Option::is_none`).
- Sole writer remains `apply_boot_env_decision` in `lib.rs` — the `BootEnvState::Degraded(ref e)`
  arm. The call site changes from `ods_status.set_degraded_boot()` to
  `ods_status.set_degraded_boot(e.to_string())`.

### 2.2 Why upgrade

The bool ships a signal that says *something* was wrong without saying *what*.
The struct keeps the wire format absent on the happy path (same as today), adds
one short string when triggered, and aligns with the `ResizeStatus`/`reason`
shape Plan B introduces. ODS and cloud consumers can branch and report. This is
a small change with a clear payoff and no compatibility cost (the field is
already `serde(skip_serializing_if)`).

### 2.3 Test updates

- `degraded_boot_serializes_only_when_true` (already in
  `src/runtime/omnect_device_service.rs`) becomes
  `degraded_boot_serializes_only_when_set`: when `set_degraded_boot("…")` runs,
  JSON contains `"degraded_boot":{"reason":"…"}`; otherwise the key is absent.
- `apply_boot_env_decision` tests in `src/lib.rs` (`degraded_ok_core_sets_degraded_flag`)
  assert `ods.degraded_boot.is_some()` and that the inner `reason` matches the
  injected `BootEnvError::Display`.
- No new test files.

---

## 3. Existing contract — documented, not added

This section ratifies what is already in the codebase so future changes cannot
drift. **No behavior change.** A spec is needed because the contract spans
modules and was previously implicit.

### 3.1 `classify_boot_env` decides the response to env-unavailable

`src/bootloader/mod.rs::classify_boot_env(open_result, is_release_image)`:

- `Ok(bl)` → `Continue(Available(bl))`. Both image types.
- `Err(e)` + **release** → `Continue(Degraded(e))`. Warn-and-continue.
- `Err(e)` + **debug** → `Abort(InitramfsError::DegradedBoot(e))`. Becomes a
  Plan A `Fatal`; handle_fatal_error spawns the debug shell.

This makes env-unavailable an in-band, image-type-aware decision. The error
variant `InitramfsError::DegradedBoot` is *only* produced for the debug-Abort
path; the release-Continue path never raises an error. Plan A classifies
`InitramfsError::DegradedBoot → Fatal` accordingly (Plan A §2.1).

### 3.2 `apply_boot_env_decision` enforces ordering and precedence

`src/lib.rs::apply_boot_env_decision`:

- Calls `persist_fsck_results(ods_status, env.available_mut(), rootfs)` **before**
  propagating `core_result?`. This is the safety-critical "diagnostics survive
  the error" contract from `mount_core_partitions`. The function takes
  `Option<&mut dyn BootEnv>`, so the data-log channel works in degraded mode;
  the bootloader-env channel is skipped when `None`.
- On `BootEnvState::Degraded`, calls `ods_status.set_degraded_boot(...)` after
  `core_result?` propagates (so a `FsckRequiresReboot` reboot path does not
  carry a misleading degraded flag).
- On `BootEnvDecision::Abort`, propagates `core_result?` first so
  `FsckRequiresReboot` wins over `DegradedBoot`. Regression-tested by
  `fsck_reboot_wins_over_abort` and `fsck_reboot_wins_over_degraded_continue`.

### 3.3 Degraded-mode behavior (in-code, ratified)

| Subsystem | Degraded-mode behavior |
|-----------|------------------------|
| `preflight::resize_data` | Runs *without* a guard. The resize is attempted; on success the guard cannot be written, so the resize may re-run next boot. (Plan B reclassifies all resize failures as `ContinueDegraded` + indicator, so this remains best-effort.) |
| `create_ods_runtime_files` / `handle_update_validation` | Skipped — no validate-update trigger files, no `omnect_bootloader_updated` marker. |
| `persist_fsck_results` | Already `Option`-aware: data-log channel still writes (gated on `data_mounted`); bootloader-env channel skipped. |
| Plan A §2.5 default | Honored: `update_pending = false` in degraded mode → on `Fatal`, terminal (`Halt`/`Shell`). Plan C does **not** override this. |

---

## 4. Terminology cleanup

Reserve **"degraded boot"** for the env-unavailable case only.

- `src/filesystem/boot_sequence.rs` (the comment on `fsck_and_record` that says
  "A degraded boot with a corrupted partition is preferable to an unrecoverable
  brick") — re-word as "A lenient mount of a partition with fsck errors is
  preferable to an unrecoverable brick." Same idea, different vocabulary.
- Audit during implementation: any other `// degraded` or doc-string usage that
  refers to the lenient-mount case is reworded the same way. The
  env-unavailable uses (`lib.rs`, `bootloader::mod.rs`, `mode::mod.rs`) stay.

---

## 5. Code structure

- **`src/runtime/omnect_device_service.rs`** — replace `degraded_boot: bool` and
  parameterless `set_degraded_boot` with `Option<DegradedBootStatus>` + a
  `set_degraded_boot(reason: String)` that stores `Some(DegradedBootStatus { reason })`.
  Update the existing serialization test accordingly.
- **`src/lib.rs::apply_boot_env_decision`** — change the call to
  `set_degraded_boot(e.to_string())`. No other call site to update.
- **`src/filesystem/boot_sequence.rs`** — re-word the comment per §4. No code
  change.

No new module; no new external dependency. The persist-split refactor proposed
in earlier drafts is **dropped**: `persist_fsck_results` already accepts
`Option<&mut dyn BootEnv>` and the in-tree path covers degraded mode.

---

## 6. Testing

- **Field absent on healthy boot.** Existing
  `degraded_boot_serializes_only_when_true` test → renamed and adapted: when
  no setter call, JSON has no `degraded_boot` key.
- **Field present with reason on failure.** Existing
  `degraded_ok_core_sets_degraded_flag` in `src/lib.rs` → assert
  `ods.degraded_boot == Some(DegradedBootStatus { reason: "..." })` and that
  serialization round-trips `"degraded_boot":{"reason":"..."}`.
- **`fsck_reboot_wins_*` tests** continue to assert
  `ods.degraded_boot.is_none()` after the reboot path (no degraded flag set when
  reboot wins). Note the assertion shape change: `!ods.degraded_boot` becomes
  `ods.degraded_boot.is_none()`.
- **No new test cases needed** for the documented contracts in §3 — they are
  already covered.

---

## 7. Out of scope

- **The two new orchestration types (`BootEnvState`, `BootEnvDecision`) and
  `apply_boot_env_decision`** — already implemented. §3 documents them; no
  changes proposed.
- **Persist-fsck refactor** — dropped: the function is already `Option`-aware
  and the data-log channel works in degraded mode.
- **Changing the "warn and continue" choice** on release — preserved as today.
- **Factory-reset survival** of the degraded signal — no factory-reset feature
  exists; the signal lives in the runtime JSON only.

---

## 8. Open questions

None blocking.
