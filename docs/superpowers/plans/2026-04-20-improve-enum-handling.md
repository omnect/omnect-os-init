# Improve Enum Handling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace magic integers, magic strings, and untyped string constants with proper Rust enum types across the codebase to gain compile-time safety, remove redundant derived booleans, and eliminate stringly-typed API surfaces.

**Architecture:** Five independent, non-overlapping changes applied in order. Each task is self-contained; later tasks may call `.as_str()` on types introduced by earlier tasks but do not depend on them at the type level. Task 3 touches the most files (partition map key type change); tasks 1, 2, 4, 5 are each confined to one or two files.

**Tech Stack:** Rust, `thiserror`, `serde` (no new crate dependencies — all improvements use the standard library and already-present crates).

---

## File Map

| Task | Modifies | Creates |
|------|----------|---------|
| 1 | `src/runtime/omnect_device_service.rs` | — |
| 2 | `src/filesystem/fsck.rs`, `src/filesystem/mod.rs` | — |
| 3 | `src/partition/layout.rs`, `src/partition/symlinks.rs`, `src/partition/mod.rs`, `src/filesystem/boot_sequence.rs` | — |
| 4 | `src/runtime/omnect_device_service.rs` | — |
| 5 | `src/filesystem/mount.rs`, `src/filesystem/overlayfs.rs`, `src/filesystem/fsck.rs`, `src/filesystem/boot_sequence.rs` | — |

---

## Build & Test Commands

```bash
# All four valid feature combinations required for full coverage
cargo test --features grub,gpt
cargo test --features grub,dos
cargo test --features uboot,gpt
cargo test --features uboot,dos

# Run lint on each bootloader feature (partition feature is also required)
cargo clippy --tests --features grub,gpt -- -D warnings
cargo clippy --tests --features uboot,gpt -- -D warnings

# Formatting
cargo fmt -- --check
```

---

## Task 1: `FactoryResetStatusCode` enum

**Context:** `FactoryResetStatus.status: u32` in `omnect_device_service.rs` carries a doc comment that reads "Status code: 0=success, 1=invalid, 2=error, 3=config_error". This is a textbook magic-integer anti-pattern: callers must read docs to understand legal values and there is no protection against out-of-range integers.

**Files:**
- Modify: `src/runtime/omnect_device_service.rs`

- [ ] **Step 1: Write a failing test that uses the new type**

Add inside `#[cfg(test)] mod tests` in `omnect_device_service.rs`:

```rust
#[test]
fn test_factory_reset_status_code_serializes_as_integer() {
    use serde_json::Value;
    let status = FactoryResetStatus {
        status: FactoryResetStatusCode::Success,
        error: None,
        context: None,
        paths: vec![],
    };
    let json: Value = serde_json::from_str(&serde_json::to_string(&status).unwrap()).unwrap();
    assert_eq!(json["status"], 0);

    let err_status = FactoryResetStatus {
        status: FactoryResetStatusCode::ConfigError,
        error: None,
        context: None,
        paths: vec![],
    };
    let json: Value = serde_json::from_str(&serde_json::to_string(&err_status).unwrap()).unwrap();
    assert_eq!(json["status"], 3);
}
```

- [ ] **Step 2: Run to confirm it fails**

```bash
cargo test --features grub,gpt test_factory_reset_status_code_serializes_as_integer 2>&1 | tail -20
```

Expected: compile error — `FactoryResetStatusCode` is not yet defined.

- [ ] **Step 3: Define `FactoryResetStatusCode` before the struct definitions**

Add directly after the last `const` at the top of `omnect_device_service.rs`, before any `struct` or `fn` definitions:

```rust
/// Outcome codes for a factory reset operation.
///
/// Serialized as a plain integer so the JSON wire format that
/// `omnect-device-service` reads remains unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactoryResetStatusCode {
    Success     = 0,
    Invalid     = 1,
    Error       = 2,
    ConfigError = 3,
}

impl serde::Serialize for FactoryResetStatusCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_u32(*self as u32)
    }
}
```

- [ ] **Step 4: Update `FactoryResetStatus.status` field type**

Replace the field declaration:

```rust
// Before
/// Status code: 0=success, 1=invalid, 2=error, 3=config_error
pub status: u32,

// After
/// Outcome of the factory reset operation.
pub status: FactoryResetStatusCode,
```

- [ ] **Step 5: Update the existing serialization test**

The existing test `test_factory_reset_status_serialization` constructs `FactoryResetStatus` with `status: 0`. Change it to:

```rust
let status = FactoryResetStatus {
    status: FactoryResetStatusCode::Success,
    error: None,
    context: Some("normal".to_string()),
    paths: vec!["/etc/hostname".to_string()],
};
let json = serde_json::to_string(&status).unwrap();
assert!(json.contains("\"status\":0"));
assert!(json.contains("\"paths\""));
```

- [ ] **Step 6: Run all tests to confirm green**

```bash
cargo test --features grub,gpt 2>&1 | tail -10
cargo test --features uboot,dos 2>&1 | tail -10
```

Expected: all tests pass.

- [ ] **Step 7: Lint and format**

```bash
cargo clippy --tests --features grub,gpt -- -D warnings
cargo fmt -- --check
```

- [ ] **Step 8: Commit**

```bash
git add src/runtime/omnect_device_service.rs
git commit -m "refactor(runtime): replace FactoryResetStatus.status: u32 with typed enum

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

## Task 2: `FsckExitCode` newtype + `Display`, drop redundant bool fields

**Context:** `src/filesystem/fsck.rs` has a `mod exit_code` of bare `i32` constants. `FsckResult` carries three derived values that can be computed from the exit code: `success: bool`, `reboot_required: bool`, and the free function `describe_fsck_exit_code(code: i32) -> String`. Replacing these with a typed newtype and a `Display` impl eliminates the duplication, makes bitwise checks self-documenting, and removes the possibility of passing an arbitrary `i32` where an exit code is expected.

**Files:**
- Modify: `src/filesystem/fsck.rs`
- Modify: `src/filesystem/mod.rs` (update re-export)

- [ ] **Step 1: Write failing tests for the new type**

Add to `#[cfg(test)] mod tests` in `fsck.rs`:

```rust
#[test]
fn test_fsck_exit_code_clean() {
    let code = FsckExitCode::from_process(Some(0));
    assert!(code.is_clean());
    assert!(!code.is_reboot_required());
    assert!(code.is_mount_safe());
    assert_eq!(format!("{code}"), "No errors");
}

#[test]
fn test_fsck_exit_code_corrected() {
    let code = FsckExitCode::from_process(Some(1));
    assert!(code.is_corrected());
    assert!(!code.is_reboot_required());
    assert!(code.is_mount_safe());
    assert_eq!(format!("{code}"), "errors corrected");
}

#[test]
fn test_fsck_exit_code_reboot_required() {
    let code = FsckExitCode::from_process(Some(2));
    assert!(code.is_reboot_required());
    assert!(!code.is_mount_safe());
    assert_eq!(format!("{code}"), "reboot required");
}

#[test]
fn test_fsck_exit_code_combined() {
    // Code 3 = CORRECTED | REBOOT_REQUIRED — reboot takes precedence.
    let code = FsckExitCode::from_process(Some(3));
    assert!(code.is_corrected());
    assert!(code.is_reboot_required());
    assert!(!code.is_mount_safe());
    assert_eq!(format!("{code}"), "errors corrected, reboot required");
}

#[test]
fn test_fsck_exit_code_unknown_sentinel() {
    let code = FsckExitCode::from_process(None);
    assert_eq!(code, FsckExitCode::UNKNOWN);
    assert_eq!(code.bits(), -1);
}
```

- [ ] **Step 2: Run to confirm they fail**

```bash
cargo test --features grub,gpt test_fsck_exit_code 2>&1 | tail -20
```

Expected: compile error — `FsckExitCode` is not yet defined.

- [ ] **Step 3: Replace `mod exit_code` with `FsckExitCode` newtype**

Remove the entire `mod exit_code { ... }` block. Add these declarations at the top of `fsck.rs` (after `use` statements, before any `fn` or `struct`):

```rust
use std::fmt;

/// Type-safe wrapper for fsck(8) exit codes.
///
/// The value is a bitmask; individual bits can be tested with the predicate
/// methods below. `UNKNOWN` (-1) is a sentinel for processes killed by signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsckExitCode(i32);

impl FsckExitCode {
    /// No errors detected.
    pub const OK: Self = Self(0);
    /// Filesystem errors corrected (safe to mount with -y).
    pub const CORRECTED: Self = Self(1);
    /// System should be rebooted before mounting.
    pub const REBOOT_REQUIRED: Self = Self(2);
    /// Uncorrectable errors remain.
    pub const ERRORS_UNCORRECTED: Self = Self(4);
    /// Operational error in fsck itself.
    pub const OPERATIONAL_ERROR: Self = Self(8);
    /// Usage or syntax error.
    pub const USAGE_ERROR: Self = Self(16);
    /// Cancelled by user request.
    pub const CANCELLED: Self = Self(32);
    /// Shared library error.
    pub const LIBRARY_ERROR: Self = Self(128);
    /// Sentinel: process was killed by a signal (no exit status from the OS).
    pub const UNKNOWN: Self = Self(-1);

    /// Construct from the raw process exit code.
    ///
    /// `None` means the process was killed by a signal; maps to `UNKNOWN`.
    pub fn from_process(code: Option<i32>) -> Self {
        Self(code.unwrap_or(-1))
    }

    /// The raw integer value (for wire-format serialization).
    pub fn bits(self) -> i32 {
        self.0
    }

    pub fn is_clean(self) -> bool {
        self.0 == 0
    }

    pub fn is_corrected(self) -> bool {
        self.0 & 1 != 0
    }

    pub fn is_reboot_required(self) -> bool {
        self.0 & 2 != 0
    }

    pub fn has_uncorrected_errors(self) -> bool {
        self.0 & 4 != 0
    }

    pub fn has_operational_error(self) -> bool {
        self.0 & 8 != 0
    }

    pub fn is_cancelled(self) -> bool {
        self.0 & 32 != 0
    }

    pub fn is_library_error(self) -> bool {
        self.0 & 128 != 0
    }

    /// Returns `true` if the filesystem is safe to mount.
    ///
    /// True only when the exit code is 0 (clean) or 1 (errors corrected by -y)
    /// **and** reboot is not required. Code 3 (CORRECTED | REBOOT_REQUIRED) sets
    /// this to false — reboot takes precedence over the corrected flag.
    pub fn is_mount_safe(self) -> bool {
        (self.is_clean() || (self.is_corrected() && !self.is_reboot_required()))
    }
}

impl fmt::Display for FsckExitCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_clean() {
            return write!(f, "No errors");
        }
        let mut parts: Vec<&str> = Vec::new();
        if self.is_corrected()           { parts.push("errors corrected"); }
        if self.is_reboot_required()     { parts.push("reboot required"); }
        if self.has_uncorrected_errors() { parts.push("uncorrected errors"); }
        if self.has_operational_error()  { parts.push("operational error"); }
        if self.0 & 16 != 0             { parts.push("usage error"); }
        if self.is_cancelled()           { parts.push("cancelled"); }
        if self.is_library_error()       { parts.push("library error"); }
        if parts.is_empty() {
            write!(f, "unknown error (code {})", self.0)
        } else {
            write!(f, "{}", parts.join(", "))
        }
    }
}
```

- [ ] **Step 4: Update `FsckResult` — remove derived bool fields, change exit_code type**

Replace the current `FsckResult` struct and its `impl` block:

```rust
/// Result of a filesystem check.
#[derive(Debug, Clone)]
pub struct FsckResult {
    /// Device that was checked.
    pub device: PathBuf,
    /// Parsed exit code. Use predicate methods (`is_mount_safe`, `is_reboot_required`, …)
    /// rather than comparing raw bits.
    pub exit_code: FsckExitCode,
    /// Combined stdout + stderr output from fsck.
    pub output: String,
}

impl FsckResult {
    pub fn has_uncorrected_errors(&self) -> bool {
        self.exit_code.has_uncorrected_errors()
    }

    pub fn has_operational_error(&self) -> bool {
        self.exit_code.has_operational_error()
    }
}
```

- [ ] **Step 5: Rewrite `check_filesystem` to use `FsckExitCode`**

Replace the body of `check_filesystem`. Key changes:
- `output.status.code().unwrap_or(exit_code::UNKNOWN)` → `FsckExitCode::from_process(output.status.code())`
- Remove the `FsckResult { success, reboot_required }` fields
- Replace `exit_code == exit_code::OK` → `exit_code.is_clean()` etc.
- Remove the call to `describe_fsck_exit_code`

```rust
fn check_filesystem(device: &Path, fstype: &str) -> Result<FsckResult> {
    log::info!("Running fsck on {}", device.display());

    disable_kmsg_ratelimit();
    let _ratelimit_guard = KmsgRatelimitGuard;

    let mut cmd = Command::new(FSCK_CMD);
    cmd.arg(FSCK_AUTO_REPAIR_FLAG);
    cmd.args([FSCK_TYPE_FLAG, fstype]);
    cmd.arg(device);

    let output = cmd.output().map_err(|e| FilesystemError::FsckFailed {
        device: device.to_path_buf(),
        code: FsckExitCode::UNKNOWN.bits(),
        output: format!("Failed to execute fsck: {}", e),
    })?;

    let exit_code = FsckExitCode::from_process(output.status.code());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined_output = format!("{}{}", stdout, stderr);

    if exit_code.is_clean() {
        log::debug!("fsck: {} is clean", device.display());
    } else if exit_code == FsckExitCode::CORRECTED {
        log::info!(
            "fsck corrected errors on {} (code 1) — filesystem is clean, continuing",
            device.display()
        );
    } else if exit_code.is_reboot_required() {
        log::warn!(
            "fsck on {} requires reboot ({})",
            device.display(),
            exit_code
        );
    } else {
        log::error!(
            "fsck failed on {} with {}: {}",
            device.display(),
            exit_code,
            combined_output.lines().next().unwrap_or("(no output)")
        );
    }

    if exit_code.is_reboot_required() {
        return Err(FilesystemError::FsckRequiresReboot {
            device: device.to_path_buf(),
            code: exit_code.bits(),
            output: combined_output,
        });
    }

    if !exit_code.is_mount_safe() {
        return Err(FilesystemError::FsckFailed {
            device: device.to_path_buf(),
            code: exit_code.bits(),
            output: combined_output,
        });
    }

    Ok(FsckResult {
        device: device.to_path_buf(),
        exit_code,
        output: combined_output,
    })
}
```

- [ ] **Step 6: Rewrite `check_filesystem_lenient` using new types**

```rust
pub fn check_filesystem_lenient(device: &Path, fstype: &str) -> Result<FsckResult> {
    match check_filesystem(device, fstype) {
        Ok(result) => Ok(result),
        Err(FilesystemError::FsckRequiresReboot { device, code, output }) => {
            Err(FilesystemError::FsckRequiresReboot { device, code, output })
        }
        Err(FilesystemError::FsckFailed { device, code, output }) => {
            let exit_code = FsckExitCode(code);
            log::warn!(
                "fsck on {} had errors ({}), continuing anyway",
                device.display(),
                exit_code
            );
            Ok(FsckResult { device, exit_code, output })
        }
        Err(e) => Err(e),
    }
}
```

- [ ] **Step 7: Remove `describe_fsck_exit_code` and update `boot_sequence.rs`**

Delete the `describe_fsck_exit_code` function entirely. In `src/filesystem/boot_sequence.rs`, replace any use of `describe_fsck_exit_code(code)` with `format!("{}", FsckExitCode(code))` — verify there are no usages first:

```bash
grep -rn "describe_fsck_exit_code" src/
```

If no usages remain, remove it from `src/filesystem/mod.rs` re-exports too. Add `FsckExitCode` to the re-exports:

```rust
pub use self::fsck::{FsckExitCode, FsckResult, check_filesystem_lenient};
```

- [ ] **Step 8: Update tests in `fsck.rs` for removed bool fields**

The existing `test_fsck_result_has_uncorrected_errors` test constructs `FsckResult` with `success` and `reboot_required` fields. Update both tests to use the new struct shape:

```rust
#[test]
fn test_fsck_result_has_uncorrected_errors() {
    let result = FsckResult {
        device: PathBuf::from("/dev/sda1"),
        exit_code: FsckExitCode::ERRORS_UNCORRECTED,
        output: String::new(),
    };
    assert!(result.has_uncorrected_errors());

    let clean = FsckResult {
        device: PathBuf::from("/dev/sda1"),
        exit_code: FsckExitCode::OK,
        output: String::new(),
    };
    assert!(!clean.has_uncorrected_errors());
}

#[test]
fn test_fsck_result_has_operational_error() {
    let result = FsckResult {
        device: PathBuf::from("/dev/sda1"),
        exit_code: FsckExitCode::OPERATIONAL_ERROR,
        output: String::new(),
    };
    assert!(result.has_operational_error());
}
```

Remove or update the old `describe_fsck_exit_code` tests:

```rust
#[test]
fn test_fsck_exit_code_display_ok() {
    assert_eq!(format!("{}", FsckExitCode::OK), "No errors");
}

#[test]
fn test_fsck_exit_code_display_corrected() {
    assert_eq!(format!("{}", FsckExitCode::CORRECTED), "errors corrected");
}

#[test]
fn test_fsck_exit_code_display_reboot() {
    assert_eq!(format!("{}", FsckExitCode::REBOOT_REQUIRED), "reboot required");
}

#[test]
fn test_fsck_exit_code_display_combined() {
    // Code 3 = CORRECTED | REBOOT_REQUIRED
    assert_eq!(
        format!("{}", FsckExitCode(3)),
        "errors corrected, reboot required"
    );
}

#[test]
fn test_fsck_exit_code_display_errors() {
    assert_eq!(format!("{}", FsckExitCode::ERRORS_UNCORRECTED), "uncorrected errors");
}
```

- [ ] **Step 9: Run all tests across all four feature combinations**

```bash
cargo test --features grub,gpt && cargo test --features grub,dos && \
cargo test --features uboot,gpt && cargo test --features uboot,dos
```

Expected: all pass.

- [ ] **Step 10: Lint and format**

```bash
cargo clippy --tests --features grub,gpt -- -D warnings
cargo fmt -- --check
```

- [ ] **Step 11: Commit**

```bash
git add src/filesystem/fsck.rs src/filesystem/mod.rs
git commit -m "refactor(filesystem): replace mod exit_code i32 constants with FsckExitCode newtype

Remove the mod exit_code { i32 constants } block.  Introduce FsckExitCode, a
newtype over i32 with typed predicate methods and a Display impl.  This
replaces describe_fsck_exit_code() and removes the redundant success/reboot_required
bool fields from FsckResult — both were derived values of exit_code.

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

## Task 3: `PartitionName` enum — typed partition map keys

**Context:** `src/partition/layout.rs` exposes a `pub mod partition_names` of `&str` constants and `PartitionLayout.partitions: HashMap<String, PathBuf>`. Typos in string keys fail silently at runtime. Replacing the keys with a typed `PartitionName` enum makes invalid partition lookups a compile error. The string representation is preserved via `as_str()` for the ODS JSON wire format and symlink names.

**Files:**
- Modify: `src/partition/layout.rs`
- Modify: `src/partition/symlinks.rs`
- Modify: `src/partition/mod.rs`
- Modify: `src/filesystem/boot_sequence.rs`

- [ ] **Step 1: Write failing tests for `PartitionName`**

Add to `#[cfg(test)] mod tests` in `layout.rs`:

```rust
#[test]
fn test_partition_name_as_str() {
    assert_eq!(PartitionName::Boot.as_str(), "boot");
    assert_eq!(PartitionName::RootA.as_str(), "rootA");
    assert_eq!(PartitionName::RootB.as_str(), "rootB");
    assert_eq!(PartitionName::RootCurrent.as_str(), "rootCurrent");
    assert_eq!(PartitionName::Factory.as_str(), "factory");
    assert_eq!(PartitionName::Cert.as_str(), "cert");
    assert_eq!(PartitionName::Etc.as_str(), "etc");
    assert_eq!(PartitionName::Data.as_str(), "data");
}

#[test]
fn test_partition_layout_uses_typed_keys() {
    let device = RootDevice {
        base: std::path::PathBuf::from("/dev/sda"),
        partition_sep: "",
        root_partition: std::path::PathBuf::from("/dev/sda2"),
    };
    let layout = PartitionLayout::new(device).unwrap();
    // Typed key lookup must resolve
    assert!(layout.partitions.get(&PartitionName::Boot).is_some());
    assert!(layout.partitions.get(&PartitionName::Data).is_some());
}
```

- [ ] **Step 2: Run to confirm they fail**

```bash
cargo test --features grub,gpt test_partition_name 2>&1 | tail -20
```

Expected: compile error.

- [ ] **Step 3: Define `PartitionName` enum in `layout.rs`**

Replace the entire `pub mod partition_names { ... }` block with:

```rust
/// Typed partition identifier.
///
/// Used as the key in `PartitionLayout.partitions`. Call `as_str()` to get the
/// string form required by bootloader env writes and ODS JSON output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PartitionName {
    Boot,
    RootA,
    RootB,
    RootCurrent,
    Factory,
    Cert,
    Etc,
    Data,
    #[cfg(feature = "dos")]
    Extended,
}

impl PartitionName {
    /// The canonical string form of this partition name.
    ///
    /// Used for symlink names, ODS JSON keys, and bootloader env keys.
    pub const fn as_str(self) -> &'static str {
        match self {
            PartitionName::Boot        => "boot",
            PartitionName::RootA       => "rootA",
            PartitionName::RootB       => "rootB",
            PartitionName::RootCurrent => "rootCurrent",
            PartitionName::Factory     => "factory",
            PartitionName::Cert        => "cert",
            PartitionName::Etc         => "etc",
            PartitionName::Data        => "data",
            #[cfg(feature = "dos")]
            PartitionName::Extended    => "extended",
        }
    }
}
```

- [ ] **Step 4: Update `PartitionLayout` to use `HashMap<PartitionName, PathBuf>`**

Change the struct field:

```rust
pub struct PartitionLayout {
    /// Map of partition name to device path.
    pub partitions: HashMap<PartitionName, PathBuf>,
    /// The root device.
    pub device: RootDevice,
}
```

- [ ] **Step 5: Update `build_partition_map` to insert `PartitionName` keys**

Change all `partition_names::BOOT` references to `PartitionName::Boot`, etc.:

```rust
fn build_partition_map(device: &RootDevice) -> crate::partition::Result<HashMap<PartitionName, PathBuf>> {
    let mut partitions = HashMap::new();

    partitions.insert(PartitionName::Boot,  device.partition_path(PARTITION_NUM_BOOT));
    partitions.insert(PartitionName::RootA, device.partition_path(PARTITION_NUM_ROOT_A));
    partitions.insert(PartitionName::RootB, device.partition_path(PARTITION_NUM_ROOT_B));

    let root_current = match partition_suffix(&device.root_partition) {
        Some(n) if n == PARTITION_NUM_ROOT_A => device.partition_path(PARTITION_NUM_ROOT_A),
        Some(n) if n == PARTITION_NUM_ROOT_B => device.partition_path(PARTITION_NUM_ROOT_B),
        _ => {
            return Err(PartitionError::UnknownRootPartition {
                path: device.root_partition.clone(),
            });
        }
    };
    partitions.insert(PartitionName::RootCurrent, root_current);

    #[cfg(feature = "dos")]
    partitions.insert(PartitionName::Extended, device.partition_path(PARTITION_NUM_EXTENDED));

    partitions.insert(PartitionName::Factory, device.partition_path(PARTITION_NUM_FACTORY));
    partitions.insert(PartitionName::Cert,    device.partition_path(PARTITION_NUM_CERT));
    partitions.insert(PartitionName::Etc,     device.partition_path(PARTITION_NUM_ETC));
    partitions.insert(PartitionName::Data,    device.partition_path(PARTITION_NUM_DATA));

    Ok(partitions)
}
```

- [ ] **Step 6: Update `PartitionLayout` methods that look up by key**

In `is_root_a()`:
```rust
fn is_root_a(&self) -> bool {
    partition_suffix(&self.device.root_partition) == Some(PARTITION_NUM_ROOT_A)
}
```
(No change needed — doesn't use the map key.)

In `root_current()`:
```rust
pub fn root_current(&self) -> PathBuf {
    if self.is_root_a() {
        self.partitions.get(&PartitionName::RootA).cloned().unwrap_or_else(|| {
            log::warn!("rootA not in partition map; reconstructing path");
            self.device.partition_path(PARTITION_NUM_ROOT_A)
        })
    } else {
        self.partitions.get(&PartitionName::RootB).cloned().unwrap_or_else(|| {
            log::warn!("rootB not in partition map; reconstructing path");
            self.device.partition_path(PARTITION_NUM_ROOT_B)
        })
    }
}
```

In `get()`:
```rust
pub fn get(&self, name: PartitionName) -> Option<&PathBuf> {
    self.partitions.get(&name)
}
```

- [ ] **Step 7: Update `boot_sequence.rs` — replace `partition_names::*` lookups**

In `src/filesystem/boot_sequence.rs`, add the import:
```rust
use crate::partition::{PartitionLayout, PartitionName};
```

Remove the old `use crate::partition::{PartitionLayout, partition_names}` import. Then replace every `partition_names::BOOT` etc. with `PartitionName::Boot`:

```rust
// mount rootfs
let root_dev = layout.partitions.get(&PartitionName::RootCurrent).ok_or_else(|| { ... })?;

// mount boot
if let Some(boot_dev) = layout.partitions.get(&PartitionName::Boot) { ... }

// fsck calls — name is now PartitionName; convert to &str at the ODS boundary:
fsck_and_record(boot_dev, PartitionName::Boot.as_str(), ods_status, "vfat")?;
// ... same for Factory, Cert, Etc, Data
```

- [ ] **Step 8: Update `symlinks.rs` — iterate over `HashMap<PartitionName, PathBuf>`**

In `src/partition/symlinks.rs`:
1. Add `use crate::partition::layout::PartitionName;`
2. The `ROOTBLK` constant previously lived in `partition_names`. Add a local const in `symlinks.rs`:

```rust
/// Symlink name for the base block device (not a partition — represents the whole disk).
const ROOTBLK_SYMLINK_NAME: &str = "rootblk";
```

3. Replace `symlink_path(partition_names::ROOTBLK)` with `symlink_path(ROOTBLK_SYMLINK_NAME)`.

4. The loop iterates `&layout.partitions` where `name` is now `PartitionName`. Change `symlink_path(name)` to `symlink_path(name.as_str())`:

```rust
for (name, device_path) in &layout.partitions {
    create_symlink(device_path, &symlink_path(name.as_str()))?;
}
```

Same change in `verify_symlinks`:
```rust
for (name, device_path) in &layout.partitions {
    verify_symlink(&symlink_path(name.as_str()), device_path)?;
}
```

- [ ] **Step 9: Update `src/partition/mod.rs` re-exports**

Replace:
```rust
pub use layout::{PartitionLayout, partition_names};
```
With:
```rust
pub use layout::{PartitionLayout, PartitionName};
```

- [ ] **Step 10: Update tests in `layout.rs`**

The existing `build_partition_map` tests use `map.get(partition_names::BOOT)` etc. Change to `map.get(&PartitionName::Boot)`:

```rust
assert_eq!(
    map.get(&PartitionName::Boot),
    Some(&PathBuf::from("/dev/sda1"))
);
// ... etc. for all existing assertions
```

Also update `map.get(partition_names::EXTENDED)` → `map.get(&PartitionName::Extended)` (the `None` assertion in the GPT test stays valid since `Extended` variant only exists under `dos` feature).

- [ ] **Step 11: Run all tests**

```bash
cargo test --features grub,gpt && cargo test --features grub,dos && \
cargo test --features uboot,gpt && cargo test --features uboot,dos
```

Expected: all pass.

- [ ] **Step 12: Lint and format**

```bash
cargo clippy --tests --features grub,gpt -- -D warnings
cargo fmt -- --check
```

- [ ] **Step 13: Commit**

```bash
git add src/partition/layout.rs src/partition/symlinks.rs src/partition/mod.rs \
        src/filesystem/boot_sequence.rs
git commit -m "refactor(partition): replace partition_names string constants with PartitionName enum

Replace pub mod partition_names { &str constants } with a typed PartitionName enum.
PartitionLayout.partitions changes from HashMap<String, PathBuf> to
HashMap<PartitionName, PathBuf>; all callers updated.  The string form is
preserved via PartitionName::as_str() for ODS/bootloader boundaries.

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

## Task 4: `ValidateUpdateState` enum for bootloader env value

**Context:** In `omnect_device_service.rs`, `handle_update_validation` reads the `omnect_validate_update` bootloader env variable and branches on its value with `if value == BOOTLOADER_FLAG_SET` / `else if value == VALIDATE_UPDATE_FAILED_VALUE`. A typed enum makes the domain explicit, the match exhaustive, and the intent clear for future contributors.

**Files:**
- Modify: `src/runtime/omnect_device_service.rs`

- [ ] **Step 1: Write a failing test for the new type**

Add to `#[cfg(test)] mod tests`:

```rust
#[test]
fn test_validate_update_state_from_env_value() {
    assert_eq!(ValidateUpdateState::from_env_value("1"),        ValidateUpdateState::Requested);
    assert_eq!(ValidateUpdateState::from_env_value("failed"),   ValidateUpdateState::Failed);
    assert_eq!(ValidateUpdateState::from_env_value("true"),     ValidateUpdateState::Other);
    assert_eq!(ValidateUpdateState::from_env_value("0"),        ValidateUpdateState::Other);
    assert_eq!(ValidateUpdateState::from_env_value(""),         ValidateUpdateState::Other);
    assert_eq!(ValidateUpdateState::from_env_value("unexpected"), ValidateUpdateState::Other);
}
```

- [ ] **Step 2: Run to confirm it fails**

```bash
cargo test --features grub,gpt test_validate_update_state 2>&1 | tail -20
```

Expected: compile error.

- [ ] **Step 3: Add `ValidateUpdateState` before the first `fn` in the file**

Add after the `const` declarations at the top of `omnect_device_service.rs`:

```rust
/// Parsed value of the `omnect_validate_update` bootloader env variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidateUpdateState {
    /// Value `"1"` — update validation was requested before this boot.
    Requested,
    /// Value `"failed"` — the previous update validation failed.
    Failed,
    /// Any other value — no action required.
    Other,
}

impl ValidateUpdateState {
    fn from_env_value(s: &str) -> Self {
        match s {
            BOOTLOADER_FLAG_SET        => Self::Requested,
            VALIDATE_UPDATE_FAILED_VALUE => Self::Failed,
            _                          => Self::Other,
        }
    }
}
```

- [ ] **Step 4: Rewrite `handle_update_validation` using the enum**

Replace the `if let Some(value) = validate_update { if value == ... } else if ...` block:

```rust
fn handle_update_validation(
    ods_dir: &Path,
    bootloader: &dyn Bootloader,
    uid: u32,
    gid: u32,
) -> Result<()> {
    let validate_update = bootloader
        .get_env(vars::OMNECT_VALIDATE_UPDATE)
        .map_err(|e| {
            InitramfsError::Io(std::io::Error::other(format!(
                "failed to read omnect_validate_update from bootloader: {e}"
            )))
        })?;

    if let Some(value) = validate_update {
        match ValidateUpdateState::from_env_value(&value) {
            ValidateUpdateState::Requested => {
                let trigger_path = ods_dir.join(UPDATE_VALIDATE_FILE);
                fs::write(&trigger_path, BOOTLOADER_FLAG_SET).map_err(|e| {
                    InitramfsError::Io(std::io::Error::other(format!(
                        "Failed to write {}: {}",
                        trigger_path.display(),
                        e
                    )))
                })?;
                set_ownership(&trigger_path, uid, gid)?;
                set_mode(&trigger_path, FILE_MODE_READABLE)?;
                log::info!("Update validation requested - created trigger file");
            }
            ValidateUpdateState::Failed => {
                let failed_path = ods_dir.join(UPDATE_VALIDATE_FAILED_FILE);
                fs::write(&failed_path, BOOTLOADER_FLAG_SET).map_err(|e| {
                    InitramfsError::Io(std::io::Error::other(format!(
                        "Failed to write {}: {}",
                        failed_path.display(),
                        e
                    )))
                })?;
                set_ownership(&failed_path, uid, gid)?;
                set_mode(&failed_path, FILE_MODE_READABLE)?;
                log::warn!("Update validation failed marker created");
            }
            ValidateUpdateState::Other => {}
        }
    }

    let bootloader_updated = bootloader
        .get_env(vars::OMNECT_BOOTLOADER_UPDATED)
        .map_err(|e| {
            InitramfsError::Io(std::io::Error::other(format!(
                "failed to read omnect_bootloader_updated from bootloader: {e}"
            )))
        })?;

    if let Some(value) = bootloader_updated
        && value == BOOTLOADER_FLAG_SET
    {
        let marker_path = ods_dir.join(BOOTLOADER_UPDATED_FILE);
        fs::write(&marker_path, BOOTLOADER_FLAG_SET).map_err(|e| {
            InitramfsError::Io(std::io::Error::other(format!(
                "Failed to write {}: {}",
                marker_path.display(),
                e
            )))
        })?;
        set_ownership(&marker_path, uid, gid)?;
        set_mode(&marker_path, FILE_MODE_RESTRICTED)?;
        log::info!("Bootloader update marker created");
    }

    Ok(())
}
```

- [ ] **Step 5: Run all tests**

```bash
cargo test --features grub,gpt && cargo test --features uboot,dos
```

All existing `test_handle_update_validation_*` tests must still pass.

- [ ] **Step 6: Lint and format**

```bash
cargo clippy --tests --features grub,gpt -- -D warnings
cargo fmt -- --check
```

- [ ] **Step 7: Commit**

```bash
git add src/runtime/omnect_device_service.rs
git commit -m "refactor(runtime): introduce ValidateUpdateState enum for bootloader env dispatch

Replace if/else-if string comparisons on omnect_validate_update env value with
a typed ValidateUpdateState enum and exhaustive match in handle_update_validation.

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

## Task 5: `FsType` enum — typed filesystem type

**Context:** `src/filesystem/mount.rs` has a `mod fstype { &str constants }` and `MountOptions.fstype: Option<String>` (a heap allocation for every mount). `src/filesystem/overlayfs.rs` has `const OVERLAY_FSTYPE: &str = "overlay"`. `check_filesystem` takes `fstype: &str`. Replacing all with a `FsType` enum eliminates heap allocation, removes the private module, and makes invalid filesystem type strings a compile error.

**Files:**
- Modify: `src/filesystem/mount.rs`
- Modify: `src/filesystem/overlayfs.rs`
- Modify: `src/filesystem/fsck.rs`
- Modify: `src/filesystem/boot_sequence.rs`
- Modify: `src/filesystem/mod.rs` (update re-exports)

- [ ] **Step 1: Write failing tests**

Add to `#[cfg(test)] mod tests` in `mount.rs`:

```rust
#[test]
fn test_fstype_as_str() {
    assert_eq!(FsType::Ext4.as_str(), "ext4");
    assert_eq!(FsType::Vfat.as_str(), "vfat");
    assert_eq!(FsType::Tmpfs.as_str(), "tmpfs");
    assert_eq!(FsType::Overlay.as_str(), "overlay");
}

#[test]
fn test_mount_options_ext4_readonly_fstype() {
    let opts = MountOptions::ext4_readonly();
    assert_eq!(opts.fstype, Some(FsType::Ext4));
}

#[test]
fn test_mount_options_vfat_fstype() {
    let opts = MountOptions::vfat();
    assert_eq!(opts.fstype, Some(FsType::Vfat));
}
```

- [ ] **Step 2: Run to confirm they fail**

```bash
cargo test --features grub,gpt test_fstype 2>&1 | tail -20
```

Expected: compile error.

- [ ] **Step 3: Define `FsType` enum in `mount.rs`**

Remove `mod fstype { ... }`. Add before any `struct` or `fn` definition:

```rust
/// Filesystem type for mount operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsType {
    Ext4,
    Vfat,
    Tmpfs,
    Overlay,
}

impl FsType {
    pub const fn as_str(self) -> &'static str {
        match self {
            FsType::Ext4    => "ext4",
            FsType::Vfat    => "vfat",
            FsType::Tmpfs   => "tmpfs",
            FsType::Overlay => "overlay",
        }
    }
}
```

- [ ] **Step 4: Update `MountOptions` struct and all constructors**

Change the field:
```rust
pub struct MountOptions {
    pub fstype: Option<FsType>,
    pub flags: MsFlags,
    pub data: Option<String>,
}
```

Update `Default`:
```rust
impl Default for MountOptions {
    fn default() -> Self {
        Self { fstype: None, flags: MsFlags::empty(), data: None }
    }
}
```

Update all constructor methods (remove `to_string()` calls):
```rust
pub fn ext4_readonly() -> Self {
    Self { fstype: Some(FsType::Ext4), flags: flags::RDONLY, data: None }
}
pub fn ext4_readwrite() -> Self {
    Self { fstype: Some(FsType::Ext4), flags: MsFlags::empty(), data: None }
}
pub fn vfat() -> Self {
    Self { fstype: Some(FsType::Vfat), flags: MsFlags::empty(), data: None }
}
pub fn bind() -> Self {
    Self { fstype: None, flags: flags::BIND, data: None }
}
pub fn tmpfs() -> Self {
    Self { fstype: Some(FsType::Tmpfs), flags: MsFlags::empty(), data: None }
}
```

- [ ] **Step 5: Update `mount()` to convert `FsType` to `&str`**

```rust
pub fn mount(mp: MountPoint) -> Result<()> {
    let source: Option<&Path> = if mp.source.as_os_str().is_empty() {
        None
    } else {
        Some(&mp.source)
    };
    let fstype: Option<&str> = mp.options.fstype.map(|t| t.as_str());
    let data: Option<&str> = mp.options.data.as_deref();

    nix_mount(source, &mp.target, fstype, mp.options.flags, data).map_err(|e| {
        FilesystemError::MountFailed {
            src_path: mp.source.clone(),
            target: mp.target.clone(),
            reason: e.to_string(),
        }
    })?;

    log::info!(
        "Mounted {} on {} ({})",
        mp.source.display(),
        mp.target.display(),
        fstype.unwrap_or("<none>")
    );
    Ok(())
}
```

- [ ] **Step 6: Update `mount_readwrite` and `mount_tmpfs`**

```rust
pub fn mount_readwrite(
    source: impl Into<PathBuf>,
    target: impl Into<PathBuf>,
    fstype: FsType,
) -> Result<()> {
    mount(MountPoint::new(
        source,
        target,
        MountOptions { fstype: Some(fstype), flags: MsFlags::empty(), data: None },
    ))
}

pub fn mount_tmpfs(target: impl Into<PathBuf>, flags: MsFlags, data: Option<&str>) -> Result<()> {
    mount(MountPoint::new(
        "tmpfs",
        target,
        MountOptions { fstype: Some(FsType::Tmpfs), flags, data: data.map(|s| s.to_string()) },
    ))
}
```

- [ ] **Step 7: Export `FsType` from `src/filesystem/mod.rs`**

```rust
pub use self::mount::{
    FsType, MountOptions, MountPoint, is_path_mounted, mount, mount_bind,
    mount_bind_private, mount_readwrite, mount_tmpfs,
};
```

- [ ] **Step 8: Update `overlayfs.rs`**

Remove `const OVERLAY_FSTYPE: &str = "overlay";`. Add import:
```rust
use crate::filesystem::{FsType, MountOptions, MountPoint, mount, mount_bind, mount_bind_private};
```

In `mount_overlay`:
```rust
fn mount_overlay(lower: &Path, upper: &Path, work: &Path, target: &Path) -> Result<()> {
    let options = format!(
        "lowerdir={},upperdir={},workdir={},index=off,uuid=off",
        lower.display(), upper.display(), work.display()
    );
    let mount_opts = MountOptions {
        fstype: Some(FsType::Overlay),
        flags: MsFlags::MS_NOATIME | MsFlags::MS_NODIRATIME,
        data: Some(options.clone()),
    };
    mount(MountPoint::new(FsType::Overlay.as_str(), target, mount_opts)).map_err(|e| {
        FilesystemError::OverlayFailed {
            target: target.to_path_buf(),
            reason: format!("{e}: options={options}"),
        }
    })
}
```

- [ ] **Step 9: Update `fsck.rs` — `check_filesystem` and `check_filesystem_lenient`**

Change signatures to accept `FsType`:

```rust
fn check_filesystem(device: &Path, fstype: FsType) -> Result<FsckResult> {
    // ...
    cmd.args([FSCK_TYPE_FLAG, fstype.as_str()]);
    // ...
}

pub fn check_filesystem_lenient(device: &Path, fstype: FsType) -> Result<FsckResult> {
    // body unchanged except for type of fstype parameter
}
```

Add import at the top of `fsck.rs`:
```rust
use crate::filesystem::FsType;
```

- [ ] **Step 10: Update `boot_sequence.rs` call sites**

Add import:
```rust
use crate::filesystem::{FsType, MountOptions, MountPoint, ...};
```

Change `fsck_and_record` calls:
```rust
fsck_and_record(boot_dev, PartitionName::Boot.as_str(), ods_status, FsType::Vfat)?;
fsck_and_record(factory_dev, PartitionName::Factory.as_str(), ods_status, FsType::Ext4)?;
fsck_and_record(cert_dev, PartitionName::Cert.as_str(), ods_status, FsType::Ext4)?;
fsck_and_record(etc_dev, PartitionName::Etc.as_str(), ods_status, FsType::Ext4)?;
fsck_and_record(data_dev, PartitionName::Data.as_str(), ods_status, FsType::Ext4)?;
```

Change `mount_readwrite` calls:
```rust
mount_readwrite(boot_dev, &boot_mount, FsType::Vfat)?;
```

Update `fsck_and_record` signature:
```rust
pub fn fsck_and_record(
    dev: &Path,
    name: &str,
    ods_status: &mut OdsStatus,
    fstype: FsType,
) -> std::result::Result<(), FilesystemError> {
    match check_filesystem_lenient(dev, fstype) {
        // ...
    }
}
```

Update `fsck_and_record` export in `src/filesystem/mod.rs`.

- [ ] **Step 11: Update mount tests**

```rust
#[test]
fn test_mount_options_ext4_readonly() {
    let opts = MountOptions::ext4_readonly();
    assert_eq!(opts.fstype, Some(FsType::Ext4));
    assert!(opts.flags.contains(MsFlags::MS_RDONLY));
}

#[test]
fn test_mount_options_builder() {
    let opts = MountOptions::ext4_readwrite()
        .noatime()
        .nosuid()
        .with_data("discard");
    assert_eq!(opts.fstype, Some(FsType::Ext4));
    // ... rest unchanged
}

#[test]
fn test_mount_point_new() {
    let mp = MountPoint::new("/dev/sda1", "/mnt/boot", MountOptions::vfat());
    assert_eq!(mp.options.fstype, Some(FsType::Vfat));
}
```

- [ ] **Step 12: Run all four test combinations**

```bash
cargo test --features grub,gpt && cargo test --features grub,dos && \
cargo test --features uboot,gpt && cargo test --features uboot,dos
```

- [ ] **Step 13: Lint and format**

```bash
cargo clippy --tests --features grub,gpt -- -D warnings
cargo clippy --tests --features uboot,gpt -- -D warnings
cargo fmt -- --check
```

- [ ] **Step 14: Commit**

```bash
git add src/filesystem/mount.rs src/filesystem/overlayfs.rs \
        src/filesystem/fsck.rs src/filesystem/boot_sequence.rs \
        src/filesystem/mod.rs
git commit -m "refactor(filesystem): replace mod fstype string constants with FsType enum

Replace mod fstype { &str constants } in mount.rs and OVERLAY_FSTYPE in
overlayfs.rs with a typed FsType enum.  MountOptions.fstype changes from
Option<String> to Option<FsType>, eliminating heap allocation on every mount.
check_filesystem_lenient and mount_readwrite are updated to take FsType.

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

## Final Verification

After all 5 tasks are committed, run the full verification suite:

```bash
# All feature combinations
cargo test --features grub,gpt
cargo test --features grub,dos
cargo test --features uboot,gpt
cargo test --features uboot,dos

# Lint
cargo clippy --tests --features grub,gpt -- -D warnings
cargo clippy --tests --features uboot,gpt -- -D warnings

# Format
cargo fmt -- --check

# Audit
cargo audit
```
