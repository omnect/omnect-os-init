# Factory Reset — mkfs-Corruption Brick Residual Fix — Design

**Date:** 2026-07-08
**Status:** Approved
**Tracks:** [omnect/omnect-os-init#17](https://github.com/omnect/omnect-os-init/issues/17)

> Reviewed against current `main` (see `docs/superpowers/reviews/review-brick-residual-fix.md`,
> local/untracked). Corrections from that review are folded in directly below; the one substantive
> open question it raised was resolved by the team on 2026-07-09 (§0).

## 0. Self-heal mechanism — retry `mkfs`, always mount, signal on failure

Per partition (`data`, `etc`): `mkfs.ext4` it; on a `mkfs` failure, retry the `mkfs` once — at most
two `mkfs` per partition. Every `mkfs` failure is logged with `warn!`, so a failure the retry heals
still shows up (an early sign of failing storage). The `mkfs` step never ends the reset on its own:
after the reformats, the mount is **always** attempted. The mount is the real gate:

- mount succeeds → the reset proceeds (restore, then boot), even if a `mkfs` reported errors earlier
  (the filesystem turned out usable);
- mount/overlay fails on `data`/`etc` → the partition is unusable; record the failure signal
  (§3.1/§3.5) and give up. Boot still continues to Normal (§2.2).

**A mount failure is not re-`mkfs`'d.** Re-running a `mkfs` the kernel already accepted (exit 0)
cannot heal a mount/overlay failure — that is a kernel/overlay-option problem or genuinely dead
storage, not something a second identical write fixes. Retry is scoped to the `mkfs` step; the mount
decides the rest. A plain non-destructive mount retry is also not a stage: mount is read-only
against the on-disk state, so retrying it without another write changes nothing.

**History.** Earlier versions (2026-07-09 and 2026-07-14) triggered a re-`mkfs` on a mount failure.
PR review (mlilien, JanZachmann) found that unrealistic — a `mkfs` that exited 0 leaves nothing a
second identical `mkfs` would change. The value of this fix is the diagnosable signal, not the
re-`mkfs`. The model above scopes retry to the `mkfs` step, always attempts the mount, and logs
every `mkfs` failure.

## 1. Problem

PR #16 fixed the realistic brick triggers in factory-reset mode 1 (backup / reformat / restore):
stale-mount/EBUSY via the idempotent mount guard, and post-destruction failures now resolve to a
`data_wiped` status instead of propagating.

**Residual gap.** If `mkfs.ext4` leaves `data` or `etc` genuinely unmountable — not a stale mount,
but real corruption of the freshly-reformatted filesystem — the following chain still bricks the
device:

1. `run_destructive_phase` reformats `data` and `etc` (`mod.rs:154-155`), then tries to remount
   both plus `factory` (`factory_reset_mount`, `mod.rs:216-265`).
2. The remount fails on the corrupted partition → caught and folded into a `FactoryResetStatus`
   with `data_wiped: true`, `status: Error` (`destructive_phase_failure_status`, `mod.rs:197-208`).
   `run_reset` returns `Ok`, so `FactoryResetError` never escapes as `RecoveryClass::Fatal` —
   this part already works as designed.
3. `mode::factory_reset::run` unconditionally calls `mode::normal::run(ctx)` (`mod.rs:76`).
4. `normal::run`'s own `mount_remaining_partitions` mounts the same partitions fresh
   (`src/filesystem/boot_sequence.rs:177`) and hits the same corruption. It runs `fsck_and_record`
   before `mount()` for each partition, so the failure can surface as either
   `FilesystemError::FsckFailed` or `FilesystemError::MountFailed` — both map to
   `recovery_class()` = `Fatal` (`error.rs:65,67`) → halt-loop on a release image.
5. The halt happens inside `mount_result?` (`normal.rs:57`), **before**
   `create_ods_runtime_files` (`normal.rs:64`) ever runs. The `data_wiped` status written in step 2
   only exists in the in-memory `OdsStatus` / `/run` tmpfs, which never reaches the running system
   because `switch_root` (`normal.rs:81`) is never called.

Net effect: a factory reset can still brick the device on genuine reformat corruption, and reports
nothing — violating the mode's core invariant ("a failed reset must never block boot"; see
`docs/superpowers/specs/2026-06-29-factory-reset-mode1-design.md` §3.2, §5). Originally flagged in
[PR #16 review](https://github.com/omnect/omnect-os-init/pull/16#discussion_r3518443460) and
[follow-up](https://github.com/omnect/omnect-os-init/pull/16#discussion_r3519617734), tracked here
because the fix requires a design decision rather than a triage-time patch.

## 2. Chosen approach

Two changes, layered:

1. **Self-heal**: retry a failed `mkfs` once per partition (log every `mkfs` failure), then always
   attempt the mount. The mount decides: success continues the reset; a mount/overlay failure on
   `data`/`etc` gives up. See §0.
2. **Defense in depth**: on a mount failure that gives up, persist a compact failure signal to the
   bootloader environment *before* handing off to `normal::run`, mirroring the existing
   `save_fsck_status` pattern (`bootloader/mod.rs:98-106`) that survives even a later halt. This
   does not by itself prevent the halt — see §2.2.

### 2.1 Alternatives considered

| Option | Verdict | Why |
|---|---|---|
| Retry `mkfs` on a `mkfs` failure; always mount; signal on mount failure | **Adopted** (§0) | A failed `mkfs` is worth one retry (transient/bad-block write); the mount is the real gate and is always attempted; a mount failure is signalled, not blindly re-`mkfs`'d |
| Re-`mkfs` on a mount failure too | **Rejected** (§0) | A `mkfs` that exited 0 leaves nothing a second identical `mkfs` would change; a mount failure is a kernel/overlay-option or dead-storage problem — flagged by mlilien/JanZachmann |
| Plain mount retry (non-destructive), retry once | **Rejected** (§0) | Mount is read-only against the on-disk state, so retrying it without another write changes nothing |
| New degraded fallback boot mode (mount `etc` read-only from factory defaults, skip the overlay) | **Out of scope**, deferred | Solves a different problem (repairable-vs-not signaling on genuinely dead storage); a real feature with its own design (new mode-like path, new `create_ods_runtime_files`/`switch_root` interaction), not a fix-sized change |
| Persist `data_wiped` status to bootloader env before handoff, alone | **Adopted as a layer, not standalone** | Makes the outcome visible but does not itself stop the halt; needed regardless |

### 2.2 Terminal fallback (mount gives up)

When the mount fails on a reformatted `data`/`etc` partition, this is treated as genuine hardware
failure — no further `mkfs` is expected to help. The bootloader-env failure signal (§3.1) is
written, and the code path continues into `normal::run`, which will hit `MountFailed`/`FsckFailed` →
`Fatal` and halt. This residual is accepted: it is a real storage-hardware fault, and the
improvement is that the failure is diagnosable (`fw_printenv`/`grub-editenv list` shows the signal)
rather than silent. Inventing further in-initramfs fallback behavior is out of scope (see the
deferred fallback boot mode above).

### 2.3 Partition scope

Applies symmetrically to both `data` and `etc` — `run_destructive_phase` reformats and mounts both,
and `normal::run`'s remount hits the identical `MountFailed → Fatal` halt for either one. The
"known-good empty state" for `data` is the freshly-reformatted empty filesystem (no factory-seed
step, unlike `etc`).

## 3. Component changes

### 3.1 `src/bootloader/mod.rs`

New `BootEnvKey` variant, feature-gated:

```rust
#[cfg(feature = "factory-reset")]
/// `omnect_factory_reset_last_error` — records a partition that stayed
/// unmountable after a reformat-and-retry during the factory-reset
/// destructive phase, so the failure survives even if the boot that
/// follows halts before switch_root.
FactoryResetLastError,
```

`as_str()`: `Self::FactoryResetLastError => Cow::Borrowed("omnect_factory_reset_last_error")`.

New default-implemented `BootEnv` trait method, alongside `save_fsck_status`:

```rust
/// Persist an unrecoverable factory-reset reformat/mount failure.
///
/// Stored as plain text (`"<partition>:<reason>"`), not gzip+base64 like
/// `save_fsck_status` — the payload is small and bounded, and a human should
/// be able to read it directly with `fw_printenv`/`grub-editenv list` during
/// field debugging without decoding.
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

`truncate_on_char_boundary` does not exist yet and must be added as a small new helper — a naive
byte slice can panic by splitting a multi-byte UTF-8 character; a mount/mkfs error string can
contain non-ASCII (e.g. a device path or kernel message). `PartitionName`'s `Display` yields
`"data"`/`"etc"`, so the `{partition}` format works as shown.

`MAX_FACTORY_RESET_FAILURE_REASON_LEN` is a new named constant bounding the stored reason string
(per the repo's no-magic-numbers convention) — exact value TBD at implementation time, sized to
comfortably fit a `mount(2)`/`mkfs` error string within typical grubenv/uboot-env variable limits.

### 3.2 `src/mode/factory_reset/mod.rs`

`run_destructive_phase` reformats `data`/`etc` (each with one `mkfs` retry), then mounts once, via
`reformat_and_mount_with_retry`. The function returns a `RetryReport` — not the bootloader-env write
itself — so `run()` (§3.5), the only place that owns `ctx.boot_env`, does the write.

```rust
/// Outcome of the reformat+mount, carried back so the caller can set the
/// success `context` note (§3.3) and, on a mount failure, the bootloader-env
/// signal (§3.5).
struct RetryReport {
    /// Partitions whose `mkfs` failed at least once (empty on clean reformats).
    retried: Vec<PartitionName>,
    /// Set when the mount failed on a `data`/`etc` partition: the signal to persist.
    exhausted: Option<ResetFailureSignal>,
}

/// Reformat `data` and `etc`, then mount. A failed `mkfs` is retried once per
/// partition (every failure logged); a `mkfs` that fails twice does not give up
/// — the mount is still attempted. A mount/overlay failure resolving to
/// `data`/`etc` returns that partition in `RetryReport.exhausted` (not `Err`, so
/// the typed signal survives to `run()`, §3.5); a failure resolving to neither
/// (e.g. `factory`) propagates as `Err`.
///
/// Does NOT take `boot_env` — persistence happens in `run()` (§3.5).
fn reformat_and_mount_with_retry(
    rootfs: &Path,
    targets: &ReformatTargets,
    ops: &mut dyn ReformatRetryOps,
) -> Result<RetryReport> { /* ... */ }

/// Seam over the reformat/mount side effects, so the control flow is
/// unit-testable without real block devices. Scoped to this function (see §5,
/// hardening item 7).
trait ReformatRetryOps {
    fn reformat(&mut self, device: &Path, label: &str) -> Result<()>;
    fn mount_all(&mut self) -> Result<()>;
}
```

Control flow, two phases (`mkfs` is per-partition, the mount is holistic):

1. **Reformat phase.** `reformat()` each of `data` and `etc`. On a `mkfs` failure: `warn!`, record
   the partition in `retried`, retry the `mkfs` once; a second failure only `warn!`s — it does NOT
   return early. The mount is always attempted next (the filesystem may be usable, and the mount is
   the real gate).
2. **Mount phase.** `mount_all()` (mount + overlay setup) once. On failure, resolve the partition
   via `resolve_failed_partition`:
   - `FilesystemError::MountFailed { src_path, .. }` → match `src_path` against
     `targets.data_dev`/`etc_dev`.
   - `FilesystemError::OverlayFailed { target, .. }` → `target` is either the overlay upper/work dir
     under the partition mount point, or the overlay mount target `rootfs/etc`/`rootfs/home` (see
     §3.6); match by `starts_with` against `mount_points::ETC_PARTITION`/`DATA_PARTITION`, or by
     equality against the overlay targets.

   Then:
   - **resolves to `data`/`etc`** → `exhausted = Some(ResetFailureSignal { partition, reason })` and
     **return `Ok(RetryReport{ retried, exhausted })`** — no re-`mkfs`, and *not* `Err` (see the
     boxed note below);
   - **resolves to neither** (`factory`, or an unresolvable path) → propagate the `Err`.

At most two `mkfs` per partition; the mount is attempted exactly once.

**Why the exhausted case returns `Ok`, not `Err`.** The signal must reach `run()` as *structured*
data (a typed `PartitionName` + reason) so `save_factory_reset_failure` (§3.1) can be called — that
is the point of the bootloader-env signal (§2 point 2). If it propagated `Err`, the structured
partition would be lost inside an error string and no signal would be written. The signal is
returned in `RetryReport.exhausted` as a `ResetFailureSignal` and carried out of `run_reset`
alongside the status (§3.5).

`run_destructive_phase` consumes the report:
- `report.exhausted == Some(signal)` → the partition is still unusable, so `restore_all` cannot run.
  Build the failure status (`status: Error`, `data_wiped: true`, `paths` = preserve list) and return
  it together with the signal (§3.5).
- `report.exhausted == None` → mounts succeeded (possibly after one retry). Proceed to `restore_all`;
  build the success/partial status and, if `report.retried` is non-empty, set the `context` retry
  note (§3.3).

**Path-match coupling (noted by review).** `src_path` equals `targets.data_dev`/`etc_dev` because
both are read from the same `layout.partitions.get(...)` map — this holds today, but is an implicit
coupling: if a future change canonicalizes or symlink-resolves the device path on only one side
(e.g. `/dev/omnect/data` vs. the resolved block device), the match silently stops firing. Record
this coupling in a code comment at the match site. The mocked `ReformatRetryOps` unit tests (§6)
choose their own paths and cannot catch this kind of real-code divergence — the coupling has to be
caught by code review, not by these tests.

**`factory`-partition failure is out of scope.** If the failing device matches neither `data_dev`
nor `etc_dev` (i.e. the read-only `factory` partition itself fails to mount), that's a pre-existing
condition unrelated to this reset's `reformat_ext4` calls — `factory` is never reformatted here.
This case falls through to today's behavior unchanged: immediate error, no retry, no
`save_factory_reset_failure` call. Bricking on a corrupt `factory` partition is a separate,
pre-existing failure mode, not something this fix addresses.

### 3.3 `src/runtime/omnect_device_service.rs`

`FactoryResetStatus.context` (existing field, currently used for restore-partial-failure details)
also carries a short diagnostic note when a retry succeeded. The note is built from
`RetryReport.retried` (§3.2) — the original draft's `Result<()>` signature had no way to carry this
out, which is what necessitated the `RetryReport` change above.

```json
{
  "factory_reset": {
    "status": 0,
    "context": "etc reformatted twice",
    "paths": ["..."],
    "data_wiped": true
  }
}
```

No new struct field — reuses the existing free-form `context` string. A device that needed a retry
is worth flagging to fleet monitoring even though the reset otherwise completed normally; this
would otherwise be indistinguishable from a clean reset.

**Collision with `restore_all`'s use of `context`.** `RestoreResult::PartialFailure { context, .. }`
(defined in `backup_restore.rs`, consumed at `mod.rs:181-187`) already writes its own diagnostic
string into the same field when a preserved path fails to restore. If a retry happened *and* the
subsequent restore has a partial failure, both messages are relevant and neither should silently
overwrite the other: join them with **`";"`** — the same separator `restore_all` already uses
internally for its own multi-path context, not `"; "` (with a space) — e.g.
`"etc reformatted twice;etc/hostname:restore"`, falling back to whichever one is present when only
one occurred.

### 3.4 `src/error.rs`

Hardening item 5 (§5) enumerates `FactoryResetError` variants explicitly in `recovery_class()`
instead of the current `Self::FactoryReset(_) => RecoveryClass::ContinueDegraded` wildcard, so a
future variant added without updating this match is a compile error — consistent with the
exhaustive-match convention already applied to `InitramfsError` itself.

### 3.5 `src/mode/factory_reset/mod.rs` — `run()` persistence

`save_factory_reset_failure` (§3.1) needs `ctx.boot_env`, which `run_reset`/`run_destructive_phase`
do not hold. The signal is carried out as a return value and written in `run()`, which owns
`ctx.boot_env`, after `run_reset` returns and before `normal::run(ctx)`.

**Carrier for the exhausted signal.** A named type, returned alongside the status. It is not a field
on `FactoryResetStatus`: that struct is the ODS wire DTO and stays all-serialized.

```rust
struct ResetFailureSignal {
    partition: PartitionName,
    reason: String,
}
```

`run_reset` and `run_destructive_phase` return `(FactoryResetStatus, Option<ResetFailureSignal>)`.
`run_destructive_phase` sets the signal to `Some` only on the exhausted path; every other path
returns `None`. `run()`:

```text
run():
  (status, signal) = run_reset(...)     // Ok in all cases, as today
  if let Some(sig) = signal {
      if let Some(bl) = ctx.boot_env.available_mut() {
          let _ = bl.save_factory_reset_failure(sig.partition, &sig.reason);  // best-effort
      }
  }
  ctx.ods_status.set_factory_reset(status);
  normal::run(ctx)
```

A degraded bootloader env is not a new failure mode here; the write is best-effort like the existing
`set_env` calls in this module.

This depends on the exhausted case in §3.2 returning `Ok(RetryReport{ exhausted: Some(...) })` rather
than propagating `Err` — otherwise `run_reset` would fold it into `destructive_phase_failure_status`
with no signal, and the write would never fire.

### 3.6 `OverlayFailed` coverage (new, from review)

`factory_reset_mount` ends with `setup_etc_overlay_tracked` / `setup_data_overlay_tracked`
(`mod.rs:261-262`). On a freshly-`mkfs`'d partition these can fail to create the overlay's
upper/work directories → `FilesystemError::OverlayFailed`, which is **also** `Fatal`
(`error.rs:68`) — the same brick mechanism as `MountFailed`, just one step later in
`factory_reset_mount`. The original draft's retry only matched `MountFailed`, leaving an overlay
failure on a reformatted partition to fall through with no retry and brick exactly as before.

Fix: the retry match in §3.2 treats an `OverlayFailed` that resolves to the `data`/`etc` partition
identically to `MountFailed` — an empty just-`mkfs`'d filesystem failing overlay setup is the same
"bad-reformat" signal as a mount failure. `OverlayFailed.target` carries one of two different paths,
depending on which overlay-setup step failed:

- **Dir-prep failure**: the overlay upper/work directory, which lives *under* the partition mount
  point (e.g. `mnt/etc/upper`, `mnt/data/home/upper`). Resolved by matching `target` against
  `mount_points::ETC_PARTITION`/`DATA_PARTITION` with `starts_with` (prefix match, not exact
  equality).
- **Mount-syscall failure**: the overlay mount target itself, i.e. `rootfs/etc` or `rootfs/home`
  (`overlayfs.rs`'s `mount_overlay` call sites) — a path that starts with neither mount-point
  prefix above, so it needs its own case. Resolved by matching `target` for exact equality against
  `rootfs.join(paths::ETC)` / `rootfs.join(paths::HOME)` (the same private path constants
  `overlayfs.rs` uses to build the overlay target, exposed at `pub(crate)` visibility so this match
  has one source of truth instead of a duplicated string literal).

Both sub-cases resolve to the same partition and get the same retry treatment; neither is `src_path`
equality against `targets.data_dev`/`etc_dev`, which is only for the `MountFailed` arm.

## 4. Data flow (destructive phase, updated)

```
run_destructive_phase:
  report = reformat_and_mount_with_retry():
    phase 1 — reformat: for part in {data, etc}:
      ops.reformat(part)
        Err → warn!; record part in `retried`; ops.reformat(part) once more
          Err again → warn! only — do NOT give up; the mount is still attempted
    phase 2 — mount ONCE:
      ops.mount_all()   (mount + overlay)
        Ok → RetryReport{ retried, exhausted: None }
        Err(MountFailed{src_path} | OverlayFailed{target}) resolving to data/etc →
          exhausted: Some(ResetFailureSignal{part, reason})   (NO re-mkfs)
        Err(...) resolving to factory/unknown → propagate Err (no signal)
  match report.exhausted:
    Some(signal) → build failure status (Error, data_wiped:true, paths);
                   skip restore_all; return (status, Some(signal))
    None → restore_all(...); unmount_tracked(...);
           build success/partial status; if report.retried non-empty set context note (§3.3);
           return (status, None)

run():
  (status, signal) = run_reset(...)   // Ok in all cases above
  if let Some(sig) = signal:
      save_factory_reset_failure(sig.partition, sig.reason)   [bootloader env, best-effort]
  ctx.ods_status.set_factory_reset(status)
  → mode::normal::run(ctx)  [always, unchanged]
    mount_remaining_partitions:
      - mount succeeded above → mounts cleanly, boots normally
      - mount gave up above → hits the same fault again →
        MountFailed/FsckFailed → Fatal → halt (accepted residual, see §2.2;
        bootloader env holds the diagnosable signal from this boot)
```

## 5. Hardening-item disposition

Issue #17 also lists 7 non-blocking hardening items deferred from the PR #16 review. None of them
are required to satisfy this fix's safety invariant (§2), so none of them block this change. Items
1, 4, 5, 6 are cheap and closely related enough to bundle into this fix now. Items 2, 3, and 7
(broad form) were discussed with the team on 2026-07-09 and closed.

**Item 1 is a hard prerequisite for item 2's acceptance, not an independent nice-to-have — see the
note on item 2.**

| # | Item | Location | Decision |
|---|---|---|---|
| 1 | `restore_all` should downgrade a missing-backup entry to `PartialFailure` | `backup_restore.rs` | **Do now** — required for item 2 to be safe, see note below |
| 2 | Move the factory-reset backup off volatile tmpfs, or document the power-loss/OOM tradeoff | `mod.rs` (`FACTORY_RESET_BACKUP_DIR`) | **Won't-do (accepted gap)** — see note below |
| 3 | `PreservePath` newtype carrying `validate_preserve_path`'s guarantee into `backup_all`/`restore_all` | `config.rs` | **Won't-do** — see note below |
| 4 | `ResetMode` newtype (`#[serde(try_from)]`) rejecting unsupported modes at parse time | `config.rs` | **Do now** |
| 5 | Enumerate `FactoryResetError` variants in `recovery_class()` instead of `_` | `error.rs:80` | **Do now** (see §3.4) |
| 6 | Map config I/O read errors to `Io` rather than `InvalidConfig` so ODS status isn't mislabeled `Invalid` | `config.rs` | **Do now** |
| 7 | Injectable seam for `mount`/`umount`/`reformat_ext4` to unit-test ordering invariants; consider extracting `classify_status` | `mod.rs` | **Won't-do (broad form)** — see note below |

**Note on item 2 — won't-do, accepted gap (team decision, 2026-07-09).** Moving the backup off
tmpfs is a real design decision on its own: the destination must not be `data` or `etc` (both get
wiped in `run_destructive_phase`), must survive a power loss between backup and restore, and must
be writable at that point in boot (initramfs, pre-`switch_root`). The team decided to keep the
backup on tmpfs and accept the residual power-loss/OOM window between `backup_all` succeeding and
`restore_all` running — the recommended alternative (remount `factory`, already mounted read-only
during factory-reset, `factory_reset_mount`, `mod.rs:225-233`, read-write for the backup window)
was considered and not pursued; a new partition/volume was rejected outright since it would touch
the partition layout (`config::build`, GPT/DOS tables, the Yocto recipe) and require
re-verification across every `{grub,uboot} × {gpt,dos}` combination per `CLAUDE.md`.

**This decision is only safe because of item 1.** Today, *without* item 1, this gap is silent:
`restore_path` treats a missing backup file as a benign skip (`backup_restore.rs:97-100`,
`if !backup_src.exists() { ...; return Ok(()); }`), so if the tmpfs backup is lost between
`backup_all` and `restore_all`, `restore_all` still returns `RestoreResult::Success` — the ODS
status would report a clean success while the preserved paths were silently never restored. Item 1
(downgrade a missing-backup entry to `PartialFailure`) closes exactly this detection gap, turning a
silent false-success into a visible `PartialFailure`. Accepting item 2's gap without shipping item 1
in the same change would mean accepting an *invisible* data-loss mode, not a reported one — item 1
is therefore required alongside this fix, not an independent, deferrable improvement.

**Note on item 3 — won't-do (team decision, 2026-07-09).** `PreservePath` would make
`validate_preserve_path`'s guarantee (a preserve-list entry is safe to `cp`/wipe) a type-level
invariant carried through `backup_all`/`restore_all`, instead of a validation call whose result
callers must remember stayed valid. Rejected: preserve-list entries are inherently variable —
sourced at runtime from `<rootfs>/etc/omnect/factory-reset.json` / `factory-reset.d/*.json`, not a
fixed, compile-time-known set — so a newtype wrapping "this string passed validation" adds
signature churn through `FactoryResetConfig` → `build_preserve_list` → `backup_all`/`restore_all`
without meaningfully strengthening the guarantee: the validation still has to run once at runtime
against arbitrary config content either way. No current trigger, and no future one identified either.

**Note on item 7 (broad form) — won't-do (team decision, 2026-07-09).** This fix's own retry logic
(§3.2) needed a minimal seam to be unit-testable at all — `ReformatRetryOps`, scoped to just the
reformat-and-retry control flow. That minimal seam **is** being built as part of this fix. Team
decision: no further changes beyond that — generalizing to an injectable seam across the rest of
`mod.rs` (every `mount`/`umount`/`reformat_ext4` call site, plus extracting `classify_status`) is
not being pursued. Those remaining invariants stay verified by code review and manual/hardware
testing, same as today.

## 6. Testing

| Test | Kind | Location |
|---|---|---|
| Retry loop: first mount fails on `etc`, reformat + retry succeeds | Unit (mock `ReformatRetryOps`) | `mode/factory_reset/mod.rs` |
| Retry loop: `etc` fails twice → `RetryReport.exhausted` set, error propagates | Unit (mock) | `mode/factory_reset/mod.rs` |
| Retry loop: both `data` and `etc` each fail once, both recover | Unit (mock) | `mode/factory_reset/mod.rs` |
| Retry loop: `OverlayFailed` on `etc` triggers reformat + retry, same as `MountFailed` | Unit (mock) | `mode/factory_reset/mod.rs` |
| Successful retry sets `context` from `RetryReport.retried` | Unit | `mode/factory_reset/mod.rs` |
| `context` collision: retry note + restore partial-failure joined with `";"` | Unit | `mode/factory_reset/mod.rs` |
| `run()` calls `save_factory_reset_failure` when the returned status carries an exhausted signal | Unit (`MockBootEnv`) | `mode/factory_reset/mod.rs` |
| `save_factory_reset_failure` round-trips via `MockBootEnv` | Unit | `bootloader/mod.rs` |
| `BootEnvKey::FactoryResetLastError.as_str()` | Unit | `bootloader/mod.rs` |
| `truncate_on_char_boundary` never splits a multi-byte UTF-8 char | Unit | `bootloader/mod.rs` |
| `recovery_class()` — each `FactoryResetError` variant individually | Unit | `error.rs` |
| Full reset sequence with a genuinely unmountable partition | Manual (hardware/QEMU with an injected bad-block device) | — |

**Test-coverage caveat.** The mocked `ReformatRetryOps` tests choose their own device paths, so they
cannot catch a real-code `src_path`-vs-`data_dev` divergence (see the path-match coupling note in
§3.2) — that coupling is verified by code review, not by these unit tests.

Run against all feature combinations that interact with factory-reset per `CLAUDE.md`, e.g.
`cargo test --features grub,gpt,factory-reset,test-utils` and the `uboot`/`dos` equivalents.

## 7. Out of scope

- The read-only-etc-from-factory-defaults fallback boot mode (§2.1) — rejected as this fix's
  approach; a separate future feature if the terminal-fallback residual (§2.2) turns out to matter
  in practice.
- Broader in-initramfs fallback behavior for the exhausted-retry case (§2.2) beyond the
  bootloader-env signal — accepted as a hardware-fault residual, not something to engineer around
  here.

Hardening items 2, 3, and the broad form of item 7 are **not** listed here as out of scope — see
§5, where they're left open for the team to decide do-now / later-ticket / won't-do. Only items
genuinely rejected as a direction (the fallback boot mode) or explicitly accepted as a residual
(§2.2) belong in this section.
