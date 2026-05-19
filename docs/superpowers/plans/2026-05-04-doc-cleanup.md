# Doc Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove PR-scaffolding, legacy-history, and overly verbose comments from source code; codify documentation standards in `project-context.md` and `.github/copilot-instructions.md`.

**Architecture:** Pure documentation/comment edits — no logic changes. All file edits are independent and can be done in any order. Finish with a single commit. Standards updates go into both project context files so Copilot always picks them up.

**Tech Stack:** Rust (cargo fmt / cargo clippy for verification after edits)

---

## File Map

| File | Change type |
|------|-------------|
| `src/mode/mod.rs` | Remove PR-scaffolding from `BootContext`, `BootMode`, `detect()`, test block |
| `src/lib.rs` | Remove "single_match scaffolding" comment |
| `src/runtime/omnect_device_service.rs` | Remove "legacy bash" from doc block |
| `src/bootloader/grub.rs` | Remove "matches legacy behaviour/bash" from inline comments (3 places) |
| `src/bootloader/types.rs` | Remove "legacy bash script encoding" from doc |
| `src/filesystem/boot_sequence.rs` | Remove "Legacy bash never runs…" from inline comment |
| `project-context.md` | Add planned BootMode variants; add doc standards section |
| `.github/copilot-instructions.md` | Add doc standards section (mirror of project-context.md) |

---

### Task 1: Clean up `src/mode/mod.rs`

**Files:**
- Modify: `src/mode/mod.rs`

Issues to fix:
1. `BootContext` doc — verbose pre-conditions + "Future modes (factory-reset, flash-mode)…"
2. `layout` field — "Reserved for FactoryReset/Resize handlers — unused in normal boot."
3. `BootMode` doc — "Only `Normal` ships in this PR. Future variants…"
4. Commented-out future enum variants with PR references
5. `detect()` doc — `_bl` explanation referencing "respective implementation PR"
6. Test block TODO — full future-variant matrix instructions

- [ ] **Step 1: Edit `BootContext` doc**

Replace:
```rust
/// Context passed to every mode handler.
///
/// Mode handlers are invoked with **all partitions mounted**: rootfs read-only
/// at `/rootfs`, boot at `/rootfs/boot`, factory/data/cert/etc at their
/// standard mount points. `persist_fsck_results` has already run. Handlers
/// own the lifecycle of any overlay or bind mounts and must not assume
/// additional preflight will occur. Future modes (factory-reset, flash-mode)
/// that need to unmount partitions before acting do so internally.
pub struct BootContext<'a> {
    pub(crate) config: &'a Config,
    /// Reserved for FactoryReset/Resize handlers — unused in normal boot.
    #[allow(dead_code)]
```

With:
```rust
/// Context passed to every mode handler.
///
/// All partitions are mounted when a handler is invoked: rootfs read-only
/// at `/rootfs`, boot at `/rootfs/boot`, factory/data/cert/etc at their
/// standard mount points. Handlers own the lifecycle of their overlay
/// and bind mounts.
pub struct BootContext<'a> {
    pub(crate) config: &'a Config,
    #[allow(dead_code)]
```

- [ ] **Step 2: Edit `BootMode` doc — strip PR scaffolding, keep variant hints**

Replace:
```rust
/// The detected boot mode to execute.
///
/// Only `Normal` ships in this PR. Future variants (`FactoryReset`, `Resize`,
/// `FlashMode`) are added in their respective implementation PRs alongside
/// their detection logic, typed payloads, and `BootloaderEnvKey` additions.
pub enum BootMode {
    Normal,
    // FactoryReset(FactoryResetConfig) — added in the factory-reset PR
    // Resize                           — added in the resize PR
    // FlashMode(FlashKind)             — added in the flash-mode PR
}
```

With:
```rust
/// The detected boot mode to execute.
pub enum BootMode {
    Normal,
    // FactoryReset(FactoryResetConfig)
    // Resize
    // FlashMode(FlashKind)
}
```

Note: the commented-out variant hints are intentionally kept — they signal planned features without encoding PR history.

- [ ] **Step 3: Edit `detect()` doc — trim to essentials, keep `_bl` hint**

Replace:
```rust
    /// Detect the boot mode from bootloader environment variables.
    ///
    /// Accepts `Option<&dyn Bootloader>`. Returns `Normal` when the bootloader
    /// is absent (degraded boot: no env vars readable → no special mode).
    ///
    /// The `_bl` parameter is intentionally unused until the first additional
    /// mode variant lands. Rename to `bl` and add detection logic in the
    /// respective implementation PR.
    pub fn detect(_bl: Option<&dyn Bootloader>) -> Result<Self> {
```

With:
```rust
    /// Detect the boot mode from bootloader environment variables.
    ///
    /// Returns `Normal` when the bootloader is absent (degraded boot).
    /// `_bl` becomes active once a non-Normal variant is added.
    pub fn detect(_bl: Option<&dyn Bootloader>) -> Result<Self> {
```

- [ ] **Step 4: Remove test block TODO**

Replace:
```rust
    // TODO: replace with a full env-var × variant matrix when the first non-Normal
    // BootMode variant lands. Each future variant must add tests covering:
    //   - env-var present + live bootloader  → correct variant returned
    //   - env-var present + no bootloader    → degraded-boot fallback (Normal)
    //   - env-var absent                     → Normal
    // Until then these two tests only verify the degraded-boot path is reachable.

    #[test]
```

With:
```rust
    #[test]
```

- [ ] **Step 5: Verify**

```bash
cargo clippy --tests --features grub,gpt -- -D warnings
```
Expected: no warnings or errors.

---

### Task 2: Clean up `src/lib.rs`

**Files:**
- Modify: `src/lib.rs`

- [ ] **Step 1: Remove scaffolding comment**

Replace:
```rust
    // single_match: intentional scaffolding — additional variants land with
    // their implementation PRs.
    #[allow(clippy::single_match)]
```

With:
```rust
    #[allow(clippy::single_match)]
```

- [ ] **Step 2: Verify**

```bash
cargo check --features grub,gpt
```
Expected: compiles clean.

---

### Task 3: Clean up `src/runtime/omnect_device_service.rs`

**Files:**
- Modify: `src/runtime/omnect_device_service.rs`

- [ ] **Step 1: Remove "legacy bash" from doc**

Replace:
```rust
/// Ownership and permissions are set to match legacy bash:
/// - dir: omnect_device_service:omnect_device_service, 775
/// - status JSON: 600
/// - trigger files: 644
/// - bootloader_updated: 600
```

With:
```rust
/// Ownership and permissions:
/// - dir: omnect_device_service:omnect_device_service, 775
/// - status JSON: 600
/// - trigger files: 644
/// - bootloader_updated: 600
```

- [ ] **Step 2: Verify**

```bash
cargo check --features grub,gpt
```
Expected: compiles clean.

---

### Task 4: Clean up `src/bootloader/grub.rs`

**Files:**
- Modify: `src/bootloader/grub.rs`

Three inline comments to fix.

- [ ] **Step 1: Line ~49 — remove "matches legacy behaviour"**

Replace:
```rust
    // Remove file after reading — matches legacy behaviour
```

With:
```rust
    // Remove file after reading; each fsck result is consumed once.
```

- [ ] **Step 2: Line ~157 — remove "Match legacy behaviour and"**

Replace:
```rust
                // Match legacy behaviour and skip; a clean check runs on next boot.
```

With:
```rust
                // Skip; a clean check runs on next boot.
```

- [ ] **Step 3: Line ~180 — remove trailing "Matches legacy bash behaviour."**

Replace:
```rust
                // Non-boot partitions: write diagnostic to a file on the boot partition
                // instead of grubenv. grubenv is a fixed 1024-byte block — storing multiple
                // large encoded blobs there would overflow it. Boot is healthy at this point
                // (its own fsck ran first), so this write is safe regardless of this
                // partition's exit code. Matches legacy bash behaviour.
```

With:
```rust
                // Non-boot partitions: write diagnostic to a file on the boot partition
                // instead of grubenv. grubenv is a fixed 1024-byte block — storing multiple
                // large encoded blobs there would overflow it. Boot is healthy at this point
                // (its own fsck ran first), so this write is safe regardless of this
                // partition's exit code.
```

- [ ] **Step 4: Verify**

```bash
cargo clippy --tests --features grub,gpt -- -D warnings
```
Expected: no warnings.

---

### Task 5: Clean up `src/bootloader/types.rs`

**Files:**
- Modify: `src/bootloader/types.rs`

- [ ] **Step 1: Remove "legacy bash script encoding" phrasing**

Replace:
```rust
/// Produces `base64(gzip("{code}\n{output}"))` using the busybox `gzip` and
/// `base64` applets that are always present in the initramfs. This matches the
/// legacy bash script encoding so ODS can decode the value identically.
```

With:
```rust
/// Produces `base64(gzip("{code}\n{output}"))` using the busybox `gzip` and
/// `base64` applets that are always present in the initramfs. The format must
/// remain stable as ODS decodes this value at runtime.
```

- [ ] **Step 2: Verify**

```bash
cargo check --features grub,gpt
```
Expected: compiles clean.

---

### Task 6: Clean up `src/filesystem/boot_sequence.rs`

**Files:**
- Modify: `src/filesystem/boot_sequence.rs`

- [ ] **Step 1: Remove "Legacy bash never runs…" phrasing**

Replace:
```rust
    // rootCurrent is mounted directly — no fsck. Legacy bash never runs check_fs on
    // rootCurrent either: the kernel's own ext4 journal replay is the correct recovery
    // mechanism. Running fsck -y before mount can interfere with journal replay and
    // cause EUCLEAN on a filesystem that the kernel could have mounted cleanly.
```

With:
```rust
    // rootCurrent is mounted directly without fsck: the kernel's own ext4 journal
    // replay is the correct recovery mechanism. Running fsck -y before mount can
    // interfere with journal replay and cause EUCLEAN on a filesystem that the kernel
    // could have mounted cleanly.
```

- [ ] **Step 2: Verify**

```bash
cargo check --features grub,gpt
```
Expected: compiles clean.

---

### Task 7: Update `project-context.md`

**Files:**
- Modify: `project-context.md`

Two additions:
1. Planned `BootMode` variants under a new "Planned Features" section
2. Documentation standards

- [ ] **Step 1: Add Planned Features section (after section 7)**

Append at end of file:

```markdown

## 8. Planned Features (not yet implemented)

### BootMode variants
The `BootMode` enum (`src/mode/mod.rs`) currently only has `Normal`. The following variants are planned for future implementation PRs:
- `FactoryReset(FactoryResetConfig)` — wipes data partition, re-provisions device
- `Resize` — resizes partitions on first boot after image flash
- `FlashMode(FlashKind)` — enables in-field OS flashing

When implementing a new variant:
1. Add the variant to `BootMode` and update `BootMode::detect()` to read the relevant bootloader env key.
2. Add typed payload structs as needed.
3. Add `BootloaderEnvKey` entries for the detection keys.
4. Add a handler module under `src/mode/` mirroring `src/mode/normal.rs`.
5. Cover in tests: env-var present + live bootloader, env-var present + no bootloader (degraded fallback to `Normal`), env-var absent.
```

- [ ] **Step 2: Add Documentation Standards section**

Append after the planned features section:

```markdown

## 9. Documentation Standards

### Source-code comments and doc-strings
- **Explain "why", not "what":** The code shows what it does; comments explain constraints, non-obvious rationale, or invariants.
- **No history in comments:** Do not reference previous implementations ("legacy bash", "previously this was…"), PR numbers, or merge order.
- **No forward scaffolding in comments:** Do not describe features not yet implemented in the same comment block. Track planned work in section 8 of this file instead.
- **Concise doc-strings:** A doc-string should be as long as it needs to be and no longer. Avoid restating the function signature or obvious behaviour.
```

- [ ] **Step 3: Verify file renders**

```bash
cat project-context.md | tail -60
```
Expected: new sections visible and well-formed markdown.

---

### Task 8: Update `.github/copilot-instructions.md`

**Files:**
- Modify: `.github/copilot-instructions.md`

- [ ] **Step 1: Add Documentation Standards section**

Locate the `## Comments` section (already present) and extend it:

Find:
```markdown
## Comments
- **Explain "Why", Not "What":** Code explains what it does. Comments should explain *why* it does it (e.g., business logic, complex constraints, workarounds).
- **Keep Fresh:** Delete comments that contradict the code. If you change logic, update the reasoning.
- **No Redundancy:** Do not narrate obvious logic (e.g., `i += 1 // Increment i`).
```

Replace with:
```markdown
## Comments
- **Explain "Why", Not "What":** Code explains what it does. Comments should explain *why* it does it (e.g., business logic, complex constraints, workarounds).
- **Keep Fresh:** Delete comments that contradict the code. If you change logic, update the reasoning.
- **No Redundancy:** Do not narrate obvious logic (e.g., `i += 1 // Increment i`).
- **No history:** Do not reference previous implementations ("legacy bash", "previously…"), PR numbers, or merge order.
- **No forward scaffolding:** Do not describe unimplemented future features in source comments. Document planned work in `project-context.md` section 8 instead.
- **Concise doc-strings:** As long as needed, no longer. Do not restate the function signature or obvious behaviour.
```

- [ ] **Step 2: Verify**

```bash
cat .github/copilot-instructions.md | grep -A 10 "## Comments"
```
Expected: all six bullet points visible.

---

### Task 9: Final verification and commit

- [ ] **Step 1: Full lint across all feature combos**

```bash
cargo clippy --tests --features grub,gpt -- -D warnings && \
cargo clippy --tests --features grub,dos -- -D warnings && \
cargo clippy --tests --features uboot,gpt -- -D warnings && \
cargo clippy --tests --features uboot,dos -- -D warnings
```
Expected: all pass with zero warnings.

- [ ] **Step 2: Format check**

```bash
cargo fmt -- --check
```
Expected: no diffs.

- [ ] **Step 3: Run tests**

```bash
cargo test --features grub,gpt && \
cargo test --features grub,dos && \
cargo test --features uboot,gpt && \
cargo test --features uboot,dos
```
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add \
  src/mode/mod.rs \
  src/lib.rs \
  src/runtime/omnect_device_service.rs \
  src/bootloader/grub.rs \
  src/bootloader/types.rs \
  src/filesystem/boot_sequence.rs \
  project-context.md \
  .github/copilot-instructions.md

git commit -m "docs: remove legacy-history and PR-scaffolding from source comments

- BootContext doc: trimmed to essential pre-conditions
- BootMode/detect: removed PR-version scaffolding and _bl note
- mode test block: removed future-variant TODO
- lib.rs: removed single_match scaffolding comment
- omnect_device_service.rs: removed 'legacy bash' phrasing from doc
- grub.rs: removed 'matches legacy behaviour' from inline comments
- types.rs: removed 'legacy bash script encoding' phrasing
- boot_sequence.rs: removed 'Legacy bash never runs' phrasing
- project-context.md: added planned BootMode variants + doc standards
- .github/copilot-instructions.md: added doc standards rules

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```
