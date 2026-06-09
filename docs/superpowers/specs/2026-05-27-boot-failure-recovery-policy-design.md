# Design: Boot Failure & Recovery Policy

**Date:** 2026-05-27
**Status:** Draft for review
**Scope:** omnect Secure OS / omnect-os-init (initramfs PID 1)
**Role:** Foundational policy spec. Plans B (fsck), C (degraded boot), and D
(first-boot detection) reference the contract defined here.

---

## 1. Problem

`main.rs::handle_fatal_error` applies one terminal policy (release → infinite
loop, debug → shell) plus an unconditional reboot for `FsckRequiresReboot`. A
code review of the current tree found this policy is internally inconsistent and
has no test coverage:

- **A/B rollback is not considered.** The bootloader rolls back to the previous
  good slot only for an unconfirmed OTA update (the `omnect_validate_update`
  flag). There is **no** generic boot-failure fallback. Yet a release image that
  hits a fatal error during an unconfirmed-update boot infinite-loops and never
  reboots — so the bootloader never sees the un-cleared flag and never rolls
  back. A device with an intact previous slot is bricked.
- **Reboot loops are unbounded.** `FsckRequiresReboot` reboots on every image
  type with no guard. A partition stuck at fsck "reboot required" reboots forever
  — the exact loop the infinite-loop policy claims to prevent.
- **Release images can drop to an unauthenticated root shell.** On
  `mount_essential_filesystems()` failure the device spawns `/bin/sh` regardless
  of image type.
- **The halt path can spin silently.** When logger init is the failure, the
  release loop logs via the `log` facade with no logger registered — a no-op.
- **The policy is untestable.** It is inline branching in `main.rs` with no pure
  function to assert against.

### 1.1 Established facts (decided during design)

| Fact | Value |
|------|-------|
| Generic boot-failure fallback in bootloader | **None** — rollback is OTA-validation only |
| OTA rollback trigger | Bootloader rolls back on the **first** reboot where `omnect_validate_update` is still set (no counter) |
| Consequence | The OTA-rollback reboot is self-bounding to **one** reboot |
| Release-image interactive shell | **Never** — release halts on all fatal errors |

---

## 2. Recovery model

Every fatal error resolves to exactly one **action**, decided by the error's
*recovery class* and the *boot context*.

### 2.1 Recovery class (a property of the error)

| Class | Meaning | Example errors |
|-------|---------|----------------|
| `ContinueDegraded` | Non-fatal; warn and proceed | boot env unavailable on **release** (handled in-band by `classify_boot_env`, never reaches `handle_fatal_error`); non-zero non-reboot fsck on a data partition |
| `RebootToApply` | A reboot is expected to change the outcome | `FsckRequiresReboot` |
| `Fatal` | Boot cannot proceed | mount failure, missing partition, bad cmdline, missing init, logger init failure, `InitramfsError::DegradedBoot` (boot env unavailable on **debug** — produced by `classify_boot_env`'s `Abort` branch) |

Classification is exhaustive over `InitramfsError` variants. Adding a new error
variant must not compile until it is classified.

### 2.2 Boot context

- `is_release: bool` — compile-time, `cfg!(feature = "release-image")`. Evaluated
  on the **first line** of `main`, before any fallible operation, so the
  early-mount path obeys the same policy.
- `update_pending: bool` — whether `omnect_validate_update` is set in the
  boot env (an unconfirmed OTA-updated slot is booting). Read once. In
  degraded mode (env unreadable) this is **`false`** — see §2.5.

### 2.3 Action mapping

Pure function `decide(class, is_release, update_pending) -> Action`, where
`Action ∈ { Continue, Reboot, Halt, Shell }`:

```
ContinueDegraded                          -> Continue
RebootToApply                             -> Reboot   (guard-bounded, see §2.4)
Fatal + update_pending                    -> Reboot   (bootloader rolls back; self-bounding)
Fatal + !update_pending + is_release      -> Halt
Fatal + !update_pending + !is_release     -> Shell
```

### 2.4 Reboot bounding

- The **OTA-rollback reboot** (`Fatal + update_pending`) is bounded by the
  bootloader to a single reboot: after rollback, the previous good slot boots
  with no validate flag set, so a subsequent failure there is a normal `Fatal`
  boot → `Halt`/`Shell`, not another reboot.
- The **`RebootToApply` reboot** (currently fsck-only) is **unconditional** — no
  initramfs-side guard. This is a deliberate design choice (see Plan B): it
  honors fsck's explicit "reboot to apply" signal and keeps the code path
  trivial. Documented accepted risk: a persistently reboot-required partition
  (fsck repairs that never stick) or a failing disk will reboot-loop. No
  in-initramfs escape; recovery is via remote/manual intervention or a
  subsequent OTA path. Plan B owns the per-fsck-site application; Plan A only
  states the contract.

### 2.5 Default when `update_pending` is unknown

`update_pending` is unknown in two cases: (a) degraded mode — the boot env
is unreadable; (b) an early failure before the boot env is ever read (e.g.
`mount_essential_filesystems` or logger init fails in `main` ahead of
`run_init`). In both, default to **not pending** → terminal (`Halt`/`Shell`),
never reboot.

Rationale: a freshly OTA-updated slot almost always has an intact boot partition
and reaches the env read, so "unknown *and* update pending" is improbable;
choosing terminal avoids an unbounded reboot loop on a genuinely corrupt normal
boot or a broken early init. The degraded-mode case is additionally owned by
**Plan C**, which must honor or explicitly override this default.

### 2.6 Invariants

1. Release images never reach `Shell`.
2. OTA-rollback is self-bounding (bootloader-driven). `RebootToApply` is
   unconditional with a documented loop risk (§2.4). The system has at most
   these two reboot sources; no other code path may initiate a reboot.
3. `Halt` logging must not depend on the global logger — write via direct kmsg
   (`log_fatal`) / `eprintln!` on each cycle, so a logger-init failure still
   produces output.
4. A failed `reboot(2)` is logged (direct kmsg) before falling through to halt.

---

## 3. Code structure (Approach A)

Split classification (next to the errors) from policy (an isolated pure
function).

- **`src/error.rs`**: add `impl InitramfsError { pub fn recovery_class(&self) ->
  RecoveryClass }` — one match arm per variant. Exhaustive: a new variant fails
  to compile until classified.
- **`src/recovery.rs`** (new):
  - `pub enum RecoveryClass { ContinueDegraded, RebootToApply, Fatal }`
  - `pub enum Action { Continue, Reboot, Halt, Shell }`
  - `pub fn decide(class: RecoveryClass, is_release: bool, update_pending: bool)
    -> Action` — pure, no I/O.
- **`src/main.rs`**: `handle_fatal_error` reduces to: compute `recovery_class`
  → read `update_pending` from the bootloader (already opened in `run_init`;
  threaded into the fatal path) → `decide(...)` → execute the `Action`. Action
  *execution* (the `reboot(2)` call, the halt loop, the shell spawn) stays in
  `main.rs`. `is_release` moves to the first line of `main`.

### 3.1 Boundary notes

- The bootloader handle is opened inside `run_init`; the fatal path needs
  `update_pending`. Threading: `run_init` returns enough context, or
  `update_pending` is captured before the fallible region. Detailed wiring is a
  Plan-A implementation concern (the writing-plans step), not a design open
  question.
- `ContinueDegraded` is not produced by `handle_fatal_error` — it is the class of
  errors that callers already swallow (e.g. boot-env-unavailable in
  `run_init`, lenient fsck in `boot_sequence`). It is included in the model so
  classification is total and so Plans B/C can reference one vocabulary.

---

## 4. Testing

- **`recovery::decide` truth table** — assert every `(class, is_release,
  update_pending)` combination (3 × 2 × 2). The pure-function test the review
  found missing for the most safety-critical logic.
- **`InitramfsError::recovery_class` per variant** — assert each error maps to
  its intended class; the exhaustive match pins intent against future drift.
- **Anti-brick contract** — `Fatal + update_pending → Reboot`; `Fatal +
  !update_pending + release → Halt`.
- **Degraded default** — env-unreadable ⇒ `update_pending = false` ⇒ terminal.
- **Out of scope (stated explicitly):** action *execution* (`reboot(2)`, halt
  loop, shell spawn) is PID-1 / syscall behavior and is not unit-tested. The
  `RebootToApply` once-only guard is tested in Plan B with a mock bootloader.

---

## 5. Out of scope (handled by dependent plans)

- **Plan B (fsck):** application of `RebootToApply` at fsck sites (no guard, per
  §2.4); resize-data becomes best-effort + ODS indicator (eliminates the
  strict-check brick); diagnostic persistence.
- **Plan C (degraded boot):** the boot-env-unavailable contract; the
  `degraded_boot` signal to ODS; degraded-mode fatal behavior (must honor or
  override §2.5).
- **Plan D (first-boot detection):** unrelated to recovery policy; listed only to
  confirm the layered split.

---

## 6. Open questions

1. **§2.5 degraded-mode default** — confirmed as "terminal, no reboot" for Plan
   A; Plan C may revisit with a stronger degraded-boot contract.
2. **Threading `update_pending` into the fatal path** — implementation detail for
   the writing-plans step; no design ambiguity, listed for visibility.
