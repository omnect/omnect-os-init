# Factory Reset — mkfs-Corruption Brick Residual Fix — Design

**Date:** 2026-07-08
**Status:** Approved
**Tracks:** [omnect/omnect-os-init#17](https://github.com/omnect/omnect-os-init/issues/17)

> Reviewed against current `main` (see `docs/superpowers/reviews/review-brick-residual-fix.md`,
> local/untracked). Corrections from that review are folded in directly below; the one substantive
> open question it raised was resolved by the team on 2026-07-09 (§0).

## 0. Self-heal mechanism — resolved: re-mkfs, no plain-retry stage

The review (`docs/superpowers/reviews/review-brick-residual-fix.md`) questioned whether re-running
`mkfs.ext4` on remount failure is justified, since a plain (non-destructive) mount retry is cheaper
and already covers kernel/device timing races. Considered and rejected as the *sole* mechanism:

- A plain mount retry only has a chance of succeeding **before any reformat has happened** — mount
  is read-only against the filesystem's on-disk state, so retrying it after a write (`mkfs`) has
  already failed to produce a mountable result changes nothing; only another write can change the
  outcome. So a plain-retry stage would only ever help wedged in *before* the first reformat-retry,
  never between or after reformat attempts.
- `omnect-os-init` runs on eMMC, NVMe, and SD-card storage in the field (confirmed in
  `meta-omnect`: `doc/PHYTEC.md` eMMC `/dev/mmcblk2`, `doc/WELOTEC.md` NVMe `/dev/nvme0n1`). All
  three have an internal flash-translation layer with bad-block remapping — a second `mkfs` write
  to the same logical blocks can land on different physical media if the first write caused the
  controller to mark a block bad. Re-mkfs therefore has a genuine (not just hypothetical) chance of
  healing a failure a plain retry cannot, beyond the narrow "metadata didn't commit" case.

**Team decision (2026-07-09):** keep the mechanism as originally specified in §2/§3.2 — two `mkfs`
attempts total (the original reformat in `run_destructive_phase`, plus one retry reformat on mount
failure), then mount; no separate non-destructive plain-mount stage. On exhaustion, persist the
failure exactly as already specified: `context` note (§3.3) and bootloader-env signal (§3.1/§3.5).
No changes to §2–§6 were needed — they already described this mechanism.

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

1. **Self-heal**: on remount/overlay failure for a reformatted partition, re-`mkfs.ext4` it to a
   known-good empty state and retry once, per partition. This directly satisfies "must never block
   boot" for the dominant case (a bad reformat run, not dead hardware) with minimal new surface —
   it reuses the existing reformat-then-reseed-from-factory flow already in
   `run_destructive_phase`. Re-mkfs vs. a plain mount retry was discussed and resolved in favor of
   re-mkfs — see §0.
2. **Defense in depth**: if the retry is also exhausted, persist a compact failure signal to the
   bootloader environment *before* handing off to `normal::run`, mirroring the existing
   `save_fsck_status` pattern (`bootloader/mod.rs:98-106`) that survives even a later halt. This
   does not by itself prevent the halt in the exhausted-retry case — see §2.1.

### 2.1 Alternatives considered

| Option | Verdict | Why |
|---|---|---|
| Re-mkfs to known-good empty state, retry once | **Adopted** (§0) | Smallest surface, matches existing reformat/reseed flow; has a genuine chance of healing a bad-block/FTL-remap case on the eMMC/NVMe/SD storage this runs on in the field, which a plain retry cannot |
| Plain mount retry (no re-mkfs), retry once | **Rejected as sole mechanism** (§0) | Only ever useful *before* the first reformat-retry (mount is read-only, so retrying it after a failed write changes nothing); does not cover the metadata-not-committed or bad-block-remap cases re-mkfs does |
| New degraded fallback boot mode (mount `etc` read-only from factory defaults, skip the overlay) | **Out of scope**, deferred | Solves a different problem (repairable-vs-not signaling on genuinely dead storage) than "must not brick on a bad reformat"; a real feature with its own design (new mode-like path, new `create_ods_runtime_files`/`switch_root` interaction), not a fix-sized change |
| Persist `data_wiped` status to bootloader env before handoff, alone | **Adopted as a layer, not standalone** | Makes the outcome visible but does not itself stop the halt; needed regardless once retries are also exhausted |

### 2.2 Terminal fallback (retry exhausted)

If the one retry per partition also fails, this is treated as genuine hardware failure, not a
software defect in the reset sequence — no further `mkfs` attempt is expected to help. The
bootloader-env failure signal (§3.1) is written, and the code path is allowed to continue into
`normal::run`, which will still hit `MountFailed`/`FsckFailed` → `Fatal` and halt. This residual is
accepted: it
is now a real storage-hardware fault, and the improvement is that the failure is diagnosable
(`fw_printenv`/`grub-editenv list` shows the signal) rather than silent. Inventing further
in-initramfs fallback behavior for this case is explicitly out of scope (see the deferred fallback
boot mode above).

### 2.3 Partition scope

Applies symmetrically to both `data` and `etc` — `run_destructive_phase` reformats and remounts
both, and `normal::run`'s remount hits the identical `MountFailed → Fatal` halt for either one. The
"known-good empty state" for `data` is trivially the freshly-reformatted empty filesystem (no
factory-seed step, unlike `etc`).

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

`run_destructive_phase` changes from a single `factory_reset_mount` call after reformatting to a
bounded reformat-retry loop.

**Correction from review:** the original draft had this function return `Result<()>` and take no
access to the bootloader env, but then described both writing to the bootloader env (§3.1) and
setting a `context` note on *success* (§3.3) as if they happened here — neither is possible with
that signature. Fixed by returning a `RetryReport` and moving the actual bootloader-env write up
into `run()` (§3.5), which is the only place that owns `ctx.boot_env`.

```rust
/// Outcome of the reformat-retry loop, carried back so the caller can set the
/// success `context` note (§3.3) and, on failure, the bootloader-env signal (§3.5).
struct RetryReport {
    /// Partitions that needed one reformat-and-retry (empty on a clean first mount).
    retried: Vec<PartitionName>,
    /// Set when the retry was exhausted: the partition and reason to persist.
    exhausted: Option<(PartitionName, String)>,
}

/// Mount the reformatted partitions, self-healing a single bad reformat per
/// partition before giving up.
///
/// Each of `data`/`etc` gets at most one reformat-and-retry: if the
/// post-reformat mount or overlay setup fails, the failing partition
/// (resolved from the error — `MountFailed.src_path` vs. device paths, or
/// `OverlayFailed.target` vs. mount points; see §3.6) is re-`mkfs`'d once and
/// the mount retried. If the same partition fails a second time, retrying is
/// abandoned for good and the partition + reason is returned in
/// `RetryReport.exhausted` (NOT propagated as `Err`) — see §2.2/§0 for why this
/// is treated as a hardware fault, and §3.2/§3.5 for why it must stay typed.
///
/// Deliberately does NOT take `boot_env` — persistence happens in `run()`
/// (§3.5) to keep the bootloader env out of this deep call chain.
fn mount_reformatted_with_retry(
    layout: &PartitionLayout,
    rootfs: &Path,
    ods_status: &mut OdsStatus,
    mounts: &mut Vec<PathBuf>,
    targets: &ReformatTargets,
    ops: &mut dyn ReformatRetryOps,
) -> Result<RetryReport> { /* ... */ }

/// Seam over the reformat/mount side effects of the retry loop, so the
/// bounded-retry control flow can be unit tested without real block devices.
/// Scoped narrowly to this function — not a general injectable-everything
/// refactor of the module (see §5 disposition for hardening item 7).
trait ReformatRetryOps {
    fn reformat(&mut self, device: &Path, label: &str) -> Result<()>;
    fn mount_all(&mut self) -> Result<()>;
}
```

Control flow: try `mount_all()` (mount + overlay setup). On failure, resolve the failing partition
to `data`/`etc` — for `FilesystemError::MountFailed { src_path, .. }` by matching `src_path`
against `targets.data_dev`/`etc_dev`; for `FilesystemError::OverlayFailed { target, .. }` (which
carries either the overlay upper/work directory under the partition mount point, or the overlay
mount target `rootfs/etc`/`rootfs/home` — see §3.6) by matching `target` against the `etc`/`data`
mount points with `starts_with` (prefix match) or against the overlay targets with exact equality.
Then:

- **not yet retried** → record the partition in `retried`, `reformat()` it, loop once more;
- **already retried** → set `exhausted = Some((partition, reason))` and **return
  `Ok(RetryReport{ retried, exhausted })`** — do *not* propagate `Err` here (see the boxed note
  below on why this is a deliberate `Ok`, not an error);
- **resolves to neither** (`factory`, or an unresolvable path) → propagate the `Err` immediately —
  see the `factory`-partition note below.

**Why the exhausted case returns `Ok`, not `Err`.** The exhausted `(partition, reason)` must reach
`run()` as *structured* data so `save_factory_reset_failure` (§3.1) can be called with a typed
`PartitionName` — that is the entire point of the bootloader-env signal (§2 point 2). If the
exhausted case propagated `Err` instead, `run_reset` would fold it into
`destructive_phase_failure_status(e, …)` (`mod.rs:131`), which keeps only `error: Some(e.to_string())`
— the structured partition would be lost inside a string and `status.exhausted_signal()` (§3.5)
would always be `None`, so the signal would **never** be written. Returning `Ok(RetryReport)` keeps
the exhausted signal typed all the way out; `run_destructive_phase` then builds the failure status
itself (below).

`run_destructive_phase` consumes the report:
- `report.exhausted == Some((part, reason))` → the reformatted partition is still unmountable, so
  `restore_all` cannot run. Build the failure status directly (`status: Error`, `data_wiped: true`,
  `paths` = preserve list) and attach the exhausted signal to it (§3.5), then return `Ok(status)`.
- `report.exhausted == None` → mounts succeeded (possibly after one reformat retry). Proceed to
  `restore_all` as today; build the success/partial status and, if `report.retried` is non-empty,
  set the `context` retry note (§3.3).

A worst case of both partitions being corrupt costs at most one retry each (bounded: at most 3
total `mount_all` attempts), so the loop always terminates.

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
    "context": "etc reformatted twice: initial remount failed",
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
`"etc reformatted twice: initial remount failed;etc/hostname:restore"`, falling back to whichever
one is present when only one occurred.

### 3.4 `src/error.rs`

Hardening item 5 (§5) enumerates `FactoryResetError` variants explicitly in `recovery_class()`
instead of the current `Self::FactoryReset(_) => RecoveryClass::ContinueDegraded` wildcard, so a
future variant added without updating this match is a compile error — consistent with the
exhaustive-match convention already applied to `InitramfsError` itself.

### 3.5 `src/mode/factory_reset/mod.rs` — `run()` persistence (new, from review)

`save_factory_reset_failure` (§3.1) needs `ctx.boot_env`, which is not threaded into `run_reset` or
`run_destructive_phase` today (`run_reset` takes `layout, rootfs, config, ods_status`;
`run_destructive_phase` takes `layout, rootfs, mounts, ods_status, targets`). Rather than plumb the
bootloader env down through both, the failing partition + reason is carried out on the returned
`FactoryResetStatus` and written in `run()` — which already owns `ctx.boot_env` — after `run_reset`
returns and before `normal::run(ctx)`.

**Carrier for the exhausted signal.** `FactoryResetStatus` gains a non-serialized field:

```rust
pub struct FactoryResetStatus {
    // ... existing serialized fields (status, error, context, paths, data_wiped) ...
    /// Set when the destructive phase exhausted its reformat retry on a
    /// partition (§3.2). Not user-facing status — it exists only to carry the
    /// typed (partition, reason) up to run() for the bootloader-env write.
    #[serde(skip)]
    exhausted_signal: Option<(PartitionName, String)>,
}
```

`#[serde(skip)]` keeps it out of the ODS JSON — it is an internal carrier, not status. `run_destructive_phase`
sets it when `report.exhausted` is `Some` (§3.2); every other construction site leaves it `None`
(default). `run()` reads it via a small `exhausted_signal()` accessor:

```text
run():
  status = run_reset(...)              // Ok(status) even on destructive failure, as today
  if let Some((part, reason)) = status.exhausted_signal() {
      if let Some(bl) = ctx.boot_env.available_mut() {
          let _ = bl.save_factory_reset_failure(part, reason);  // best-effort
      }
  }
  ctx.ods_status.set_factory_reset(status);
  normal::run(ctx)
```

A degraded bootloader env is not a new failure mode here; the write is best-effort like the
existing `set_env` calls elsewhere in this module.

Note this depends on the exhausted case in §3.2 returning `Ok(RetryReport{ exhausted: Some(...) })`
rather than propagating `Err` — otherwise `run_reset` would build the failure status via
`destructive_phase_failure_status`, which leaves `exhausted_signal: None`, and the write would never
fire. See the boxed note in §3.2.

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
  reformat_ext4(data), reformat_ext4(etc)
  report = mount_reformatted_with_retry():   // returns Result<RetryReport>
    attempt mount_all()   (mount + overlay setup)
      Ok → RetryReport{ retried, exhausted: None }
      Err(MountFailed{src_path}|OverlayFailed{target}) resolving to data/etc,
          not yet retried:
        record partition in `retried`
        reformat_ext4(partition); retry mount_all() (once per partition)
      Err(...) resolving to data/etc, already retried:
        return Ok(RetryReport{ retried, exhausted: Some((partition, reason)) })
      Err(...) resolving to factory/unknown:
        propagate Err immediately (no retry, no signal)
  match report.exhausted:
    Some((part, reason)) → build failure status (Error, data_wiped:true, paths),
                           attach exhausted_signal; skip restore_all; return Ok(status)
    None → restore_all(...); unmount_tracked(...);
           build success/partial status; if report.retried non-empty set context note (§3.3)

run():
  status = run_reset(...)   // Ok(status) in all cases above
  if let Some((part, reason)) = status.exhausted_signal():
      save_factory_reset_failure(part, reason)   [bootloader env, best-effort]
  ctx.ods_status.set_factory_reset(status)
  → mode::normal::run(ctx)  [always, unchanged]
    mount_remaining_partitions:
      - if self-heal succeeded above: mounts cleanly, boots normally
      - if retries were exhausted above: hits the same corruption again →
        MountFailed/FsckFailed → Fatal → halt (accepted residual, see §2.2;
        bootloader env now holds the diagnosable failure signal from this boot)
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
