# Design: `omnect_extra_bootargs` Sync in the Initramfs

**Date:** 2026-07-21
**Status:** Approved
**Scope:** omnect-os-init — `init_setup::extra_bootargs`, `error`, `recovery`,
`runtime::OdsStatus`, `bootloader` (`BootEnvKey::ExtraBootArgs`).
**Depends on:** Boot Failure & Recovery Policy — reuses its `RebootToApply`
recovery class.

---

## 1. Problem

On a freshly flashed device the bootloader env `omnect_extra_bootargs` is empty
or stale, while the boot partition already carries the intended arguments in
`omnect_extra_bootargs_omnect` (and optionally `omnect_extra_bootargs_custom`).
The bootloader (GRUB `linux` line, U-Boot `bootargs`) injects
`omnect_extra_bootargs` into the kernel cmdline at boot time, so the running
kernel booted without the intended arguments. These arguments are
security-critical (e.g. LSM/AppArmor settings), so the device must not run a
full session without them.

The legacy bash init (`meta-omnect` `common-sh`) handled this: on first boot it
built the combined value, wrote it to the env, and forced an immediate
`reboot -f` (after `sync`) so the correct arguments applied from the next boot.

The initial Rust port (`src/init_setup/extra_bootargs.rs` on
`feat/sync-extra-bootargs`) syncs the value but:

- runs on **every** boot with no first-boot / update-pending gate,
- never reboots, so the arguments are not applied for the current session,
- never verifies the write or calls `sync()`.

Two defects follow:

- **OTA rollback can break.** During an OTA validation boot the bootloader sets
  `omnect_extra_bootargs` transiently from `omnect_validate_extra_bootargs` and
  deliberately does **not** `save_env` it, so a failed validation rolls back
  cleanly. An every-boot sync would see the new boot files versus the still-old
  persistent env value and persist the new value early — defeating the rollback
  safety.
- **Fresh-flash arguments never apply immediately** — they wait for the next
  unrelated reboot.

## 2. Non-goals / relationship to OTA

The OTA path is **out of scope** and stays as-is. OTA argument changes are
handled entirely by the swupdate handler
(`swupdate_handler_v2_{grub,uboot}.sh`, which sets
`omnect_validate_extra_bootargs`) plus the bootloader validate mechanism (copies
validate → active for the validation boot without `save_env`; persists on
success, reverts on rollback). The general userspace tool
`/usr/bin/omnect_extra_bootargs.sh` covers changes during normal operation.

The initramfs sync covers **only the fresh-flash case** — the one path with no
prior swupdate-handler run.

## 3. Overview

`init_setup::extra_bootargs` runs as the first init-setup step (before
`resize_data`). On the fresh-flash boot only, it builds the combined argument
value, and if it differs from the bootloader env it writes the value, verifies
it, flushes it to disk, and requests a reboot so the arguments apply from the
next boot. Any failure other than the intended reboot is best-effort: it is
logged, recorded in ODS status, and the boot continues without a reboot.

## 4. Gate — when the sync runs

The sync runs only when all hold:

- `first_boot == true` — the primary condition. `first_boot` is derived from the
  absence of `BootEnvKey::FirstBootDone`, which omnect-os-init writes once at the
  end of `normal::run` and never clears. It survives OTA and factory reset, so
  it is `false` on every boot after the first successful one — including OTA
  validation boots. This alone restricts the sync to fresh flash.
- `update_pending == false` — defence-in-depth against an OTA validation boot,
  read once into the process-global `UPDATE_PENDING` in `run_init`.
- The bootloader env is `Available`. On `Degraded` the step skips, logs a
  warning, and records a `SkippedDegraded` ODS status.

`first_boot` stays `true` across the sync reboot: the marker is written only in
`normal::run`, which runs after `init_setup`, so a reboot in `init_setup`
happens before the marker is written. This is intentional — the gate stays open
until the value converges. The gate is therefore **not** the loop protection
(see §7).

## 5. Building the value

Read `omnect_extra_bootargs_omnect` and the optional
`omnect_extra_bootargs_custom` from the mounted boot partition
(`rootfs/boot`), join with a single space, and trim. Missing or empty files
contribute nothing; both absent yields the empty string. This matches the
legacy script and the userspace tool.

## 6. Change flow

When the built value differs from the current env value:

1. `set_env(BootEnvKey::ExtraBootArgs, Some(new))`, or `set_env(.., None)` when
   the built value is empty.
2. **Read-back verify:** read the key again; it must equal the built value.
3. **`sync()`** — flush the grubenv write to disk.
4. Request a reboot (§8).

When the value is already current: no-op, no reboot.

The boot partition is mounted read-write before init-setup runs
(`mount_core_partitions`), and `grub-editenv` runs as root, so it can update the
grubenv file.

## 7. Loop and brick protection

Two independent guards, both required:

- **Read-back verify** guards against a normalization mismatch — if the
  bootloader tool stores a value that reads back differently (quoting,
  whitespace), `current != new` would stay true forever. If the read-back does
  not match, the step does **not** reboot; it logs and records `Failed`.
- **`sync()`** guards against a lost write. `reboot(2)` with `RB_AUTOBOOT` is a
  hard reboot and does not flush filesystem buffers; without `sync()` the
  grubenv write could be lost across the reboot, so the next boot would see the
  old value and reboot again — an endless loop. The legacy script called `sync`
  before `reboot -f` for the same reason.

Read-back verify alone is not enough: it reads the page cache and can succeed
while nothing is on disk yet. Durability comes from `sync()`.

## 8. Reboot signaling

The step does not reboot directly — the reboot convention keeps that in
`main.rs`. It reuses the existing non-failure reboot class:

- New `InitramfsError` variant `ExtraBootArgsUpdated`.
- `recovery_class()` maps it to `RecoveryClass::RebootToApply` (the exhaustive
  match makes adding the variant a compile-time obligation).
- `recovery::decide` already maps `RebootToApply → Action::Reboot`, executed in
  `main::handle_fatal_error`.

`extra_bootargs::run` returns `Result<()>`: `Err(ExtraBootArgsUpdated)` after a
verified and synced write, `Ok(())` otherwise. `init_setup::run` and `run_init`
propagate it.

Unlike the fsck `RebootToApply` case, this reboot is bounded by read-back verify
plus `sync()`, not by the bootloader (a fresh-flash device has no rollback slot
to bound reboot count). The `RebootToApply` doc comment should note both reboot
reasons and their respective bounds.

## 9. Failure handling

Every outcome except the intended reboot is best-effort — logged, recorded in
ODS status, boot continues without a reboot. A failed sync never blocks boot.

## 10. ODS status — new entry

New field in `OdsStatus`, following the `resize_data` pattern:

```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub extra_bootargs: Option<ExtraBootArgsStatus>,

pub struct ExtraBootArgsStatus {
    pub outcome: ExtraBootArgsOutcome, // serde snake_case
    pub reason: String,
}

pub enum ExtraBootArgsOutcome {
    Applied,        // value written and verified; reboot follows
    AlreadyCurrent, // no change needed
    Failed,         // set_env or read-back verify failed; no reboot
    SkippedDegraded,// boot env unavailable
}
```

Setter `set_extra_bootargs_status`. `Applied` records that a reboot is imminent.

## 11. Testing

- `read_extra_bootargs`: none / omnect-only / custom-only / both / empty
  (existing tests, kept).
- Gate: `first_boot == false` → skip; `update_pending == true` → skip; degraded
  env → skip and record `SkippedDegraded`.
- Change path: differing value → `set_env` + read-back + `Err(ExtraBootArgsUpdated)`.
- Read-back mismatch (mock returns a different value) → no `Err`, ODS `Failed`,
  no reboot.
- `current == new` → no-op, no `Err`, ODS `AlreadyCurrent`.
- `recovery_class`: `ExtraBootArgsUpdated → RebootToApply` (exhaustive-match test).

## 12. Comparison to legacy

| Aspect | Legacy (bash) | This design |
|---|---|---|
| Value build | omnect + optional custom, trimmed | same |
| When | first boot only (`/mnt/etc/upper` missing) | first boot only (`first_boot` env marker) |
| OTA validation boot | not run (first-boot gate) | not run (`first_boot` false + `!update_pending`) |
| On change | `set_env` + `sync` + `reboot -f` | `set_env` + verify + `sync` + `RebootToApply` |
| Durability | `sync` before reboot | `sync` before reboot |
| Loop protection | first-boot gate closed after reboot | read-back verify + `sync` |
| Failure | logged | logged + ODS status |
