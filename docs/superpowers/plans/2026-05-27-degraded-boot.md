# Degraded Boot Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Upgrade the shipped `OdsStatus.degraded_boot: bool` to `Option<DegradedBootStatus>` carrying a one-line `reason` (the `Display` of the underlying `BootEnvError`), matching the `ResizeStatus`-style anomaly-signal pattern, and clean up the overloaded "degraded" terminology in code comments.

**Architecture:** One field type change in `runtime::omnect_device_service`, one updated call in `lib::apply_boot_env_decision`, one comment rewrite in `filesystem::boot_sequence`. No new modules; no behavior change beyond the JSON payload now carrying the cause.

**Tech Stack:** Rust 2024, `serde` 1.0. No new external dependencies.

**Spec:** `docs/superpowers/specs/2026-05-27-degraded-boot-design.md`.

**Dependencies:** Independent of Plans A, B, D. May land in any order.

---

## File map

| Path | Action | Responsibility |
|---|---|---|
| `src/runtime/omnect_device_service.rs` | Modify | Replace `degraded_boot: bool` + parameterless `set_degraded_boot()` with `Option<DegradedBootStatus>` + `set_degraded_boot(reason: String)`. Update existing serialization test. |
| `src/lib.rs` | Modify | Update the `BootEnvState::Degraded(ref e)` arm in `apply_boot_env_decision` to pass `e.to_string()` to the new setter. Update the existing `degraded_ok_core_sets_degraded_flag` and `fsck_reboot_wins_*` tests for the new field shape. |
| `src/filesystem/boot_sequence.rs` | Modify | Re-word the misleading "degraded boot with a corrupted partition" comment so "degraded boot" is reserved for the env-unavailable case. |

---

## Task 1: Replace `bool` with `Option<DegradedBootStatus>`

**Files:**
- Modify: `src/runtime/omnect_device_service.rs`

- [ ] **Step 1.1: Update the failing-test side of `degraded_boot_serializes_only_when_true`**

In `src/runtime/omnect_device_service.rs`, find the existing test:

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

Replace it with:

```rust
    #[test]
    fn degraded_boot_serializes_only_when_set() {
        let status_normal = OdsStatus::new();
        let json_normal = serde_json::to_string(&status_normal).unwrap();
        assert!(
            !json_normal.contains("degraded_boot"),
            "degraded_boot must be absent when None; got: {json_normal}"
        );

        let mut status_degraded = OdsStatus::new();
        status_degraded.set_degraded_boot("grubenv missing".to_string());
        let json_degraded = serde_json::to_string(&status_degraded).unwrap();
        assert!(
            json_degraded.contains("\"degraded_boot\""),
            "degraded_boot must be present when Some; got: {json_degraded}"
        );
        assert!(
            json_degraded.contains("\"reason\":\"grubenv missing\""),
            "reason must be present; got: {json_degraded}"
        );
    }
```

- [ ] **Step 1.2: Run the test to confirm it fails to compile**

Run: `cargo test --lib omnect_device_service::tests::degraded_boot_serializes 2>&1 | head -20`
Expected: compile error — either the existing `set_degraded_boot()` takes no argument (mismatch on the new call) or `DegradedBootStatus` type missing.

- [ ] **Step 1.3: Add the `DegradedBootStatus` type**

In `src/runtime/omnect_device_service.rs`, near the other anomaly-signal types (`FactoryResetStatus`, `ResizeStatus` once Plan B lands — if not, put it near `FactoryResetStatus`), add:

```rust
/// Indicator surfaced to ODS when the boot env was unavailable on this boot.
///
/// `None` means boot env was available (or this is a debug image that
/// aborted on env-unavailable rather than continuing). `Some(...)` only on
/// release images that classified env-unavailable as Continue(Degraded).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DegradedBootStatus {
    /// One-line human-readable detail — the Display of the BootEnvError
    /// returned by open_boot_env().
    pub reason: String,
}
```

- [ ] **Step 1.4: Replace the `degraded_boot` field**

In `src/runtime/omnect_device_service.rs`, find the field:

```rust
    /// Set when the bootloader environment was unavailable during boot.
    /// Omitted from JSON when `false` to keep the happy-path payload small
    /// and remain backward-compatible with ODS consumers that predate this field.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub degraded_boot: bool,
```

Replace with:

```rust
    /// Set when the boot env was unavailable during boot. `None` on the
    /// happy path; `Some(DegradedBootStatus { reason })` only when
    /// apply_boot_env_decision saw BootEnvState::Degraded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_boot: Option<DegradedBootStatus>,
```

- [ ] **Step 1.5: Update the setter**

In the `impl OdsStatus { ... }` block, find:

```rust
    /// Mark this boot as degraded (bootloader environment unavailable).
    pub fn set_degraded_boot(&mut self) {
        self.degraded_boot = true;
    }
```

Replace with:

```rust
    /// Mark this boot as degraded (boot env unavailable). `reason` is the
    /// Display of the BootEnvError returned by open_boot_env().
    pub fn set_degraded_boot(&mut self, reason: String) {
        self.degraded_boot = Some(DegradedBootStatus { reason });
    }
```

- [ ] **Step 1.6: Run the single test to confirm it passes**

Run: `cargo test --lib omnect_device_service::tests::degraded_boot_serializes`
Expected: 1 passed.

- [ ] **Step 1.7: Build the whole library to find downstream callers that need updating**

Run: `cargo build 2>&1 | head -60`
Expected: build *fails* because `src/lib.rs::apply_boot_env_decision` still calls `set_degraded_boot()` without an argument and the `apply_boot_env_decision` tests use `!ods.degraded_boot` (now Option, not bool). The next task fixes them.

Note the exact error locations so the next task addresses each.

- [ ] **Step 1.8: (No commit yet — the build is broken; commit after Task 2 fixes call sites.)**

---

## Task 2: Update call sites and existing tests

**Files:**
- Modify: `src/lib.rs`

- [ ] **Step 2.1: Update `apply_boot_env_decision` setter call**

In `src/lib.rs::apply_boot_env_decision`, find:

```rust
            if let BootEnvState::Degraded(ref e) = env {
                warn!("Boot environment unavailable: {e}; booting in degraded mode");
                ods_status.set_degraded_boot();
            }
```

Replace with:

```rust
            if let BootEnvState::Degraded(ref e) = env {
                warn!("Boot env unavailable: {e}; booting in degraded mode");
                ods_status.set_degraded_boot(e.to_string());
            }
```

- [ ] **Step 2.2: Update the `apply_boot_env_decision` tests in `src/lib.rs`**

Find each of these test assertions and replace.

`available_ok_core_returns_available_env`:

```rust
        assert!(!ods.degraded_boot);
```

Replace with:

```rust
        assert!(ods.degraded_boot.is_none());
```

`degraded_ok_core_sets_degraded_flag`:

```rust
        assert!(ods.degraded_boot);
```

Replace with:

```rust
        let degraded = ods
            .degraded_boot
            .as_ref()
            .expect("degraded_boot must be Some after Degraded continue");
        assert!(
            !degraded.reason.is_empty(),
            "reason must contain the BootEnvError Display"
        );
```

`fsck_reboot_wins_over_degraded_continue`:

```rust
        assert!(
            !ods.degraded_boot,
            "degraded flag must not be set on reboot path"
        );
```

Replace with:

```rust
        assert!(
            ods.degraded_boot.is_none(),
            "degraded flag must not be set on reboot path"
        );
```

Search the file for any remaining `ods.degraded_boot` references that compare against a bool and convert them similarly.

- [ ] **Step 2.3: Build and run the full test suite**

Run: `cargo build`
Expected: clean build.

Run: `cargo test --lib`
Expected: all tests pass.

- [ ] **Step 2.4: Check clippy + format**

Run: `cargo clippy --all-features --all-targets -- -D warnings && cargo fmt -- --check`
Expected: no output, exit 0.

- [ ] **Step 2.5: Commit**

```bash
git add src/runtime/omnect_device_service.rs src/lib.rs
git commit -s -m "feat(degraded-boot): carry reason in OdsStatus.degraded_boot

Replace OdsStatus.degraded_boot: bool with
Option<DegradedBootStatus { reason }>. ODS now receives the underlying
BootEnvError Display string instead of an opaque true. Wire format
stays out of the JSON on the happy path (skip_serializing_if). Sole
writer remains apply_boot_env_decision in lib.rs.

Aligns with the resize_data anomaly-signal shape; serialization is
backward-compatible on the absent path. Existing tests in lib.rs and
omnect_device_service.rs updated for the new Option shape.

Spec: docs/superpowers/specs/2026-05-27-degraded-boot-design.md §2"
```

---

## Task 3: Terminology cleanup

**Files:**
- Modify: `src/filesystem/boot_sequence.rs`

Reserve "degraded boot" for the env-unavailable case only.

- [ ] **Step 3.1: Find the misleading comment**

In `src/filesystem/boot_sequence.rs`, locate the doc-comment on `fsck_and_record`:

```rust
/// Run fsck on a partition and record the result (including output) in `ods_status`.
///
/// Lenient by design: partitions that fsck reports as failed (exit ≥ 4) are
/// still recorded and the caller proceeds to mount them. A degraded boot with
/// a corrupted partition is preferable to an unrecoverable brick on an
/// embedded device without physical access. The full fsck result is persisted
/// via `OdsStatus` (→ bootloader env + `/data/var/log/fsck/<partition>.log`)
/// so ODS can act on the degraded state at runtime.
```

- [ ] **Step 3.2: Replace the misleading phrasing**

Replace those lines with:

```rust
/// Run fsck on a partition and record the result (including output) in `ods_status`.
///
/// Lenient by design: partitions that fsck reports as failed (exit ≥ 4) are
/// still recorded and the caller proceeds to mount them. A lenient mount of a
/// partition with fsck errors is preferable to an unrecoverable brick on an
/// embedded device without physical access. The full fsck result is persisted
/// via `OdsStatus` (→ boot env + `/data/var/log/fsck/<partition>.log`) so ODS
/// can act on it at runtime — independent of `OdsStatus.degraded_boot`, which
/// is reserved for the env-unavailable condition.
```

- [ ] **Step 3.3: Audit the rest of the file for other "degraded" uses**

Run: `grep -n "degraded" src/filesystem/boot_sequence.rs`
For each hit, decide:
- Refers to env-unavailable case → leave as-is.
- Refers to "we kept booting with a corrupt partition" → reword similarly to step 3.2.

There should be no other hits in this file after step 3.2; if there are, fix them following the same rule.

- [ ] **Step 3.4: Audit the rest of the codebase**

Run: `grep -rn "degraded" src/ | grep -v "test\|cfg(test)"`
For each hit, apply the same rule. The env-unavailable uses (`lib.rs`, `bootloader::mod.rs`, `mode::mod.rs`) stay; any remaining lenient-mount references are reworded.

- [ ] **Step 3.5: Build to confirm no doc-comment syntax error**

Run: `cargo build` and `cargo doc --no-deps 2>&1 | head -20`
Expected: clean.

- [ ] **Step 3.6: Commit**

```bash
git add src/filesystem/boot_sequence.rs
git commit -s -m "docs(boot_sequence): disambiguate degraded boot vs lenient mount

Reserve the term 'degraded boot' for the env-unavailable case
(OdsStatus.degraded_boot signal). The fsck-lenient case where a
partition with errors is mounted anyway is renamed to 'lenient mount
of a partition with fsck errors' so reviewers can tell the two
conditions apart in code comments.

Spec: docs/superpowers/specs/2026-05-27-degraded-boot-design.md §4"
```

---

## Final verification

- [ ] **Step F.1: Full test suite (both feature combinations)**

```bash
cargo test --all-features
cargo test --no-default-features --features core,grub,gpt
cargo test --no-default-features --features core,uboot,dos
```

Expected: every combination passes.

- [ ] **Step F.2: Clippy + format**

```bash
cargo clippy --all-features --all-targets -- -D warnings
cargo fmt -- --check
```

Expected: clean exit.

- [ ] **Step F.3: Sanity-check the JSON wire format**

In a quick scratch test (or repl) confirm:

```rust
let s = OdsStatus::new();
assert!(!serde_json::to_string(&s).unwrap().contains("degraded_boot"));

let mut s = OdsStatus::new();
s.set_degraded_boot("test reason".into());
let j = serde_json::to_string(&s).unwrap();
assert!(j.contains("\"degraded_boot\":{\"reason\":\"test reason\"}"));
```

The serialization tests added in Task 1 already cover this; this step is a manual sanity-read of the resulting JSON.

---

## Out of scope (handled by other plans)

- **Plan A:** classifies `InitramfsError::DegradedBoot` as `Fatal`. No change to this plan's code.
- **Plan B:** introduces `OdsStatus.resize_data` with the same `Option<Status>` shape used here. Independent.
- **Plan D:** unrelated.
- **The `BootEnvState`, `BootEnvDecision`, and `apply_boot_env_decision` orchestration types** — already implemented in the repo. This plan only updates one call inside `apply_boot_env_decision`'s `Degraded` arm.
- **Persist-fsck refactor (earlier draft of this spec):** dropped — `persist_fsck_results` already accepts `Option<&mut dyn BootEnv>` and writes the `/data` log channel in degraded mode.
