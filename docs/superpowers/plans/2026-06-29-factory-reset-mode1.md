# Factory Reset Mode 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port factory-reset mode 1 (backup / reformat / restore) from the legacy bash scripts into the Rust initramfs binary as a new `BootMode::FactoryReset` variant, triggered by a bootloader env variable.

**Architecture:** `BootMode::detect()` reads `BootEnvKey::FactoryReset` from the bootloader env and parses the JSON value into a `FactoryResetConfig`. The orchestrator in `src/mode/factory_reset/mod.rs` clears the trigger, mounts partitions, backs up preserved paths, reformats data/etc, restores the backup, writes status to `OdsStatus`, then always falls through to Normal boot. All errors are `ContinueDegraded`.

**Tech Stack:** Rust, `serde_json` (already in Cargo.toml), `nix` for unmount syscall, `tempfile` (dev-dep, already in Cargo.toml) for tests, external tools `cp`, `sync`, `mkfs.ext4`, `tune2fs`.

**Reference implementation:** `<repo>/.worktrees/feat/factory-reset/src/mode/factory_reset/` — four submodules that map directly onto the tasks below.

**Spec:** `docs/superpowers/specs/2026-06-29-factory-reset-mode1-design.md`

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `Cargo.toml` | Modify | Add `factory-reset = ["core"]` feature |
| `src/filesystem/mount.rs` | Modify | Add `umount()` function |
| `src/filesystem/mod.rs` | Modify | Export `umount` |
| `src/error.rs` | Modify | Add `FactoryResetError` enum + `InitramfsError::FactoryReset` + `recovery_class` entry |
| `src/bootloader/mod.rs` | Modify | Add `BootEnvKey::FactoryReset` feature-gated variant |
| `src/runtime/mod.rs` | Modify | Re-export `FactoryResetStatus`, `FactoryResetStatusCode` |
| `src/runtime/omnect_device_service.rs` | Modify | Remove dead `copy_factory_reset_status()` + `FACTORY_RESET_STATUS_FILE` |
| `src/mode/mod.rs` | Modify | Add `BootMode::FactoryReset(FactoryResetConfig)` + extend `detect()` + declare `pub mod factory_reset` |
| `src/lib.rs` | Modify | Add `BootMode::FactoryReset` dispatch arm |
| `src/mode/factory_reset/config.rs` | Create | `FactoryResetConfig` struct + `build_preserve_list()` |
| `src/mode/factory_reset/backup_restore.rs` | Create | `backup_all()` + `restore_all()` + `RestoreResult` |
| `src/mode/factory_reset/reformat.rs` | Create | `reformat_ext4()` |
| `src/mode/factory_reset/mod.rs` | Create | `run()` orchestrator + `factory_reset_umount()` helper |

---

## Task 1: Foundation — feature flag, `umount`, exports

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/filesystem/mount.rs`
- Modify: `src/filesystem/mod.rs`

- [ ] **Step 1: Add `factory-reset` feature to Cargo.toml**

In `Cargo.toml`, add the new feature (keep alphabetical order):

```toml
[features]
default = ["core"]
core = []
dos = ["core"]
factory-reset = ["core"]   # ← add this line
gpt = ["core"]
grub = ["core"]
persistent-var-log = ["core"]
release-image = ["core"]
resize-data = ["core"]
test-utils = []
uboot = ["core"]
```

- [ ] **Step 2: Add `umount` to `src/filesystem/mount.rs`**

After the `pub fn mount(...)` function, add:

```rust
/// Unmount the filesystem at `path`.
pub fn umount(path: &Path) -> Result<()> {
    nix::mount::umount(path).map_err(|e| FilesystemError::UnmountFailed {
        target: path.to_path_buf(),
        reason: e.to_string(),
    })
}
```

- [ ] **Step 3: Export `umount` from `src/filesystem/mod.rs`**

In the existing re-export line for the `mount` module:

```rust
// Before
pub use self::mount::{
    FsType, MountOptions, MountPoint, is_path_mounted, mount, mount_bind, mount_bind_private,
    mount_readwrite,
};

// After
pub use self::mount::{
    FsType, MountOptions, MountPoint, is_path_mounted, mount, mount_bind, mount_bind_private,
    mount_readwrite, umount,
};
```

- [ ] **Step 4: Verify it compiles**

```bash
cargo check --features grub,gpt,factory-reset
```

Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/filesystem/mount.rs src/filesystem/mod.rs
git commit -m "feat(filesystem): add umount + factory-reset feature flag

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

## Task 2: Error types — `FactoryResetError` + `InitramfsError::FactoryReset`

**Files:**
- Modify: `src/error.rs`

- [ ] **Step 1: Write the failing test first**

At the bottom of `src/error.rs`, add inside the existing `#[cfg(test)] mod recovery_class_tests` block (note: add after the last existing test):

```rust
#[cfg(feature = "factory-reset")]
#[test]
fn factory_reset_error_is_continue_degraded() {
    let err = InitramfsError::FactoryReset(FactoryResetError::InvalidConfig(
        "mode 99 not supported".into(),
    ));
    assert_eq!(err.recovery_class(), RecoveryClass::ContinueDegraded);
}

#[cfg(feature = "factory-reset")]
#[test]
fn factory_reset_backup_error_is_continue_degraded() {
    let err = InitramfsError::FactoryReset(FactoryResetError::BackupFailed {
        path: std::path::PathBuf::from("/etc/hostname"),
        reason: "cp failed".into(),
    });
    assert_eq!(err.recovery_class(), RecoveryClass::ContinueDegraded);
}
```

- [ ] **Step 2: Run test to confirm compile failure**

```bash
cargo test --features grub,gpt,factory-reset 2>&1 | head -20
```

Expected: compile error — `FactoryResetError` and `InitramfsError::FactoryReset` not found.

- [ ] **Step 3: Add `FactoryResetError` enum to `src/error.rs`**

After the existing imports at the top of the file, add the `use` for `PathBuf` if not present (it already is). Then, after the last existing error enum (e.g., `LoggingError`), add:

```rust
/// Errors during factory reset
#[cfg(feature = "factory-reset")]
#[derive(Error, Debug)]
pub enum FactoryResetError {
    #[error("Invalid factory-reset config: {0}")]
    InvalidConfig(String),

    #[error("Missing field in factory-reset config: {0}")]
    MissingField(String),

    #[error("Backup failed for {}: {reason}", path.display())]
    BackupFailed { path: PathBuf, reason: String },

    #[error("Restore failed for {}: {reason}", path.display())]
    RestoreFailed { path: PathBuf, reason: String },

    #[error("Reformat failed for {}: {reason}", device.display())]
    ReformatFailed { device: PathBuf, reason: String },

    #[error("Mount error: {0}")]
    MountError(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

- [ ] **Step 4: Add `InitramfsError::FactoryReset` variant**

In the `InitramfsError` enum, add after the `#[cfg(feature = "resize-data")] ResizeData` variant:

```rust
#[cfg(feature = "factory-reset")]
#[error("Factory reset error: {0}")]
FactoryReset(#[from] FactoryResetError),
```

- [ ] **Step 5: Add the `recovery_class` entry**

In the `recovery_class()` `match` in `InitramfsError`, add after the `resize-data` arm:

```rust
#[cfg(feature = "factory-reset")]
Self::FactoryReset(_) => RecoveryClass::ContinueDegraded,
```

- [ ] **Step 6: Run tests to confirm pass**

```bash
cargo test --features grub,gpt,factory-reset -- factory_reset_error_is_continue_degraded factory_reset_backup_error_is_continue_degraded
```

Expected: 2 tests pass.

- [ ] **Step 7: Confirm non-feature build still compiles**

```bash
cargo check --features grub,gpt
```

Expected: no errors.

- [ ] **Step 8: Commit**

```bash
git add src/error.rs
git commit -m "feat(error): add FactoryResetError + ContinueDegraded recovery class

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

## Task 3: Bootloader key — `BootEnvKey::FactoryReset`

**Files:**
- Modify: `src/bootloader/mod.rs`

- [ ] **Step 1: Write the failing test**

In `src/bootloader/mod.rs`, in the existing `#[cfg(test)] mod tests` block, add:

```rust
#[cfg(feature = "factory-reset")]
#[test]
fn factory_reset_key_as_str() {
    assert_eq!(
        BootEnvKey::FactoryReset.as_str().as_ref(),
        "factory-reset"
    );
}

#[cfg(feature = "factory-reset")]
#[test]
fn factory_reset_key_roundtrip_via_mock() {
    let mut bl = MockBootEnv::new();
    bl.set_env(BootEnvKey::FactoryReset, Some(r#"{"mode":1}"#))
        .unwrap();
    assert_eq!(
        bl.get_env(BootEnvKey::FactoryReset).unwrap(),
        Some(r#"{"mode":1}"#.to_string())
    );
    bl.set_env(BootEnvKey::FactoryReset, None).unwrap();
    assert_eq!(bl.get_env(BootEnvKey::FactoryReset).unwrap(), None);
}
```

- [ ] **Step 2: Run test to confirm compile failure**

```bash
cargo test --features grub,gpt,factory-reset 2>&1 | head -10
```

Expected: compile error — `BootEnvKey::FactoryReset` not found.

- [ ] **Step 3: Add the new variant to `BootEnvKey` enum**

In the `BootEnvKey` enum in `src/bootloader/mod.rs`, after `FirstBootDone`:

```rust
#[cfg(feature = "factory-reset")]
/// `factory-reset` — JSON trigger set by ODS to request a factory reset.
/// Value format: `{"mode":1,"preserve":["applications","network"]}`.
/// Cleared by the initramfs as the first step of the reset sequence.
FactoryReset,
```

- [ ] **Step 4: Add the `as_str` mapping**

In the `as_str()` `match` in `BootEnvKey::impl`, add:

```rust
#[cfg(feature = "factory-reset")]
Self::FactoryReset => Cow::Borrowed("factory-reset"),
```

- [ ] **Step 5: Run tests to confirm pass**

```bash
cargo test --features grub,gpt,factory-reset -- factory_reset_key_as_str factory_reset_key_roundtrip_via_mock
```

Expected: 2 tests pass.

- [ ] **Step 6: Confirm non-feature build still compiles**

```bash
cargo check --features grub,gpt
```

Expected: no errors.

- [ ] **Step 7: Commit**

```bash
git add src/bootloader/mod.rs
git commit -m "feat(bootloader): add BootEnvKey::FactoryReset

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

## Task 4: Config module — `FactoryResetConfig` + `build_preserve_list`

**Files:**
- Create: `src/mode/factory_reset/config.rs`

- [ ] **Step 1: Create the file with stub implementations**

Create `src/mode/factory_reset/config.rs`:

```rust
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::error::{FactoryResetError, Result};

pub(crate) const FACTORY_RESET_CONFIG_FILE: &str = "etc/omnect/factory-reset.json";
const FACTORY_RESET_CONFIG_DIR: &str = "etc/omnect/factory-reset.d";
const PRESERVE_LIST_MANDATORY: &str = "/etc/omnect/factory-reset.d/";
const KEY_APPLICATIONS: &str = "applications";
const KEY_PATHS: &str = "paths";

#[derive(Debug, Deserialize)]
pub struct FactoryResetConfig {
    pub mode: u32,
    #[serde(default)]
    pub preserve: Vec<String>,
}

impl FactoryResetConfig {
    pub fn parse(json: &str) -> Result<Self> {
        serde_json::from_str(json).map_err(|e| {
            FactoryResetError::InvalidConfig(format!("Failed to parse factory-reset JSON: {e}"))
                .into()
        })
    }
}

pub fn build_preserve_list(config: &FactoryResetConfig, rootfs: &Path) -> Result<Vec<String>> {
    let mut list = vec![PRESERVE_LIST_MANDATORY.to_string()];

    let has_non_app_keys = config.preserve.iter().any(|k| k != KEY_APPLICATIONS);

    let config_file = rootfs.join(FACTORY_RESET_CONFIG_FILE);
    let key_config: Option<Value> = if has_non_app_keys {
        let content = std::fs::read_to_string(&config_file).map_err(|e| {
            FactoryResetError::InvalidConfig(format!(
                "Failed to read {}: {e}",
                config_file.display()
            ))
        })?;
        let value: Value = serde_json::from_str(&content).map_err(|e| {
            FactoryResetError::InvalidConfig(format!(
                "Failed to parse {}: {e}",
                config_file.display()
            ))
        })?;
        Some(value)
    } else {
        None
    };

    for key in &config.preserve {
        if key == KEY_APPLICATIONS {
            collect_application_paths(rootfs, &mut list)?;
        } else {
            let value = key_config.as_ref().unwrap_or_else(|| {
                unreachable!("key_config must be Some for non-application keys")
            });
            let paths = value.get(key.as_str()).ok_or_else(|| {
                FactoryResetError::MissingField(format!(
                    "{}: no '{key}' key",
                    config_file.display()
                ))
            })?;
            let arr = paths.as_array().ok_or_else(|| {
                FactoryResetError::InvalidConfig(format!(
                    "{}: value for key '{key}' must be an array",
                    config_file.display()
                ))
            })?;
            for p in arr {
                if let Some(s) = p.as_str() {
                    list.push(s.to_string());
                }
            }
        }
    }

    Ok(list)
}

fn collect_application_paths(rootfs: &Path, list: &mut Vec<String>) -> Result<()> {
    let dir = rootfs.join(FACTORY_RESET_CONFIG_DIR);

    if !dir.exists() {
        return Ok(());
    }

    let entries = std::fs::read_dir(&dir).map_err(|e| {
        FactoryResetError::InvalidConfig(format!("Failed to read {}: {e}", dir.display()))
    })?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let content = std::fs::read_to_string(&path).map_err(|e| {
            FactoryResetError::InvalidConfig(format!("Failed to read {}: {e}", path.display()))
        })?;

        let value: Value = serde_json::from_str(&content).map_err(|e| {
            FactoryResetError::InvalidConfig(format!("{}: invalid JSON ({e})", path.display()))
        })?;

        if let Some(arr) = value.get(KEY_PATHS).and_then(|v| v.as_array()) {
            for p in arr {
                if let Some(s) = p.as_str() {
                    list.push(s.to_string());
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn parse_mode_and_preserve() {
        let cfg = FactoryResetConfig::parse(r#"{"mode":1,"preserve":[]}"#).unwrap();
        assert_eq!(cfg.mode, 1);
        assert!(cfg.preserve.is_empty());
    }

    #[test]
    fn parse_with_preserve_keys() {
        let cfg =
            FactoryResetConfig::parse(r#"{"mode":2,"preserve":["applications","network"]}"#)
                .unwrap();
        assert_eq!(cfg.mode, 2);
        assert_eq!(cfg.preserve, vec!["applications", "network"]);
    }

    #[test]
    fn parse_invalid_json_returns_error() {
        assert!(FactoryResetConfig::parse("not json").is_err());
    }

    #[test]
    fn parse_missing_mode_returns_error() {
        assert!(FactoryResetConfig::parse(r#"{"preserve":[]}"#).is_err());
    }

    #[test]
    fn build_preserve_list_empty_preserve() {
        let temp = TempDir::new().unwrap();
        let cfg = FactoryResetConfig {
            mode: 1,
            preserve: vec![],
        };
        let list = build_preserve_list(&cfg, temp.path()).unwrap();
        assert_eq!(list, vec![PRESERVE_LIST_MANDATORY]);
    }

    #[test]
    fn build_preserve_list_applications_key() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("etc/omnect/factory-reset.d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("app.json"),
            r#"{"paths":["/home/user/.config","var/app"]}"#,
        )
        .unwrap();

        let cfg = FactoryResetConfig {
            mode: 1,
            preserve: vec!["applications".into()],
        };
        let list = build_preserve_list(&cfg, temp.path()).unwrap();
        assert_eq!(list[0], PRESERVE_LIST_MANDATORY);
        assert!(list.contains(&"/home/user/.config".to_string()));
        assert!(list.contains(&"var/app".to_string()));
    }

    #[test]
    fn build_preserve_list_custom_key() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("etc/omnect");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("factory-reset.json"),
            r#"{"network":["/etc/network/interfaces","/etc/wpa_supplicant.conf"]}"#,
        )
        .unwrap();

        let cfg = FactoryResetConfig {
            mode: 1,
            preserve: vec!["network".into()],
        };
        let list = build_preserve_list(&cfg, temp.path()).unwrap();
        assert!(list.contains(&"/etc/network/interfaces".to_string()));
        assert!(list.contains(&"/etc/wpa_supplicant.conf".to_string()));
    }

    #[test]
    fn build_preserve_list_custom_key_non_array_returns_error() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("etc/omnect");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("factory-reset.json"),
            r#"{"network":"/etc/network/interfaces"}"#,
        )
        .unwrap();

        let cfg = FactoryResetConfig {
            mode: 1,
            preserve: vec!["network".into()],
        };
        assert!(build_preserve_list(&cfg, temp.path()).is_err());
    }

    #[test]
    fn build_preserve_list_applications_invalid_json_returns_invalid_config() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("etc/omnect/factory-reset.d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("app.json"), "not json").unwrap();

        let cfg = FactoryResetConfig {
            mode: 1,
            preserve: vec!["applications".into()],
        };
        let error = build_preserve_list(&cfg, temp.path()).unwrap_err();
        assert!(matches!(
            error,
            crate::error::InitramfsError::FactoryReset(FactoryResetError::InvalidConfig(_))
        ));
    }
}
```

- [ ] **Step 2: Run tests to confirm they compile and pass**

```bash
cargo test --features grub,gpt,factory-reset -- mode::factory_reset::config::tests
```

Expected: 7 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/mode/factory_reset/config.rs
git commit -m "feat(factory-reset): add FactoryResetConfig + build_preserve_list

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

## Task 5: Backup/Restore module

**Files:**
- Create: `src/mode/factory_reset/backup_restore.rs`

- [ ] **Step 1: Create `src/mode/factory_reset/backup_restore.rs`**

```rust
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{FactoryResetError, Result};

const CP_CMD: &str = "/bin/cp";
const SYNC_CMD: &str = "/bin/sync";

/// Result of a restore pass over the full preserve list.
pub enum RestoreResult {
    Success,
    PartialFailure { context: String, error: String },
}

/// Backup all preserve-list paths from rootfs into backup_dir.
pub fn backup_all(rootfs: &Path, preserve_list: &[String], backup_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(backup_dir)?;
    for path in preserve_list {
        backup_path(rootfs, path, backup_dir)?;
    }
    Ok(())
}

/// Restore all preserve-list paths from backup_dir back into rootfs.
///
/// Partial restore failures are accumulated and returned as `RestoreResult::PartialFailure`
/// rather than aborting mid-restore. This ensures as many paths as possible are restored
/// even when individual files cannot be copied.
pub fn restore_all(
    rootfs: &Path,
    preserve_list: &[String],
    backup_dir: &Path,
) -> Result<RestoreResult> {
    let mut error_context: Vec<String> = Vec::new();
    let mut last_error: Option<String> = None;

    for path in preserve_list {
        if let Err(e) = restore_path(rootfs, path, backup_dir) {
            log::warn!("restore failed for {path}: {e}");
            error_context.push(format!("{}:restore", path.trim_start_matches('/')));
            last_error = Some(e.to_string());
        }
    }

    if let Some(error) = last_error {
        Ok(RestoreResult::PartialFailure {
            context: error_context.join(";"),
            error,
        })
    } else {
        Ok(RestoreResult::Success)
    }
}

fn backup_path(rootfs: &Path, path: &str, backup_dir: &Path) -> Result<()> {
    let src = rootfs.join(path.trim_start_matches('/'));

    if !src.exists() {
        log::info!("backup: {path} does not exist; skipping");
        return Ok(());
    }

    log::info!("backup: {path}");

    let output = Command::new(CP_CMD)
        .args(["--parents", "-a"])
        .arg(&src)
        .arg(backup_dir)
        .output()
        .map_err(|e| FactoryResetError::BackupFailed {
            path: PathBuf::from(path),
            reason: format!("Failed to run cp: {e}"),
        })?;

    if !output.status.success() {
        return Err(FactoryResetError::BackupFailed {
            path: PathBuf::from(path),
            reason: format!(
                "cp failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ),
        }
        .into());
    }

    run_sync()?;
    Ok(())
}

fn restore_path(rootfs: &Path, path: &str, backup_dir: &Path) -> Result<()> {
    let path_stripped = path.trim_start_matches('/');
    let backup_src = backup_dir
        .join(rootfs.strip_prefix("/").unwrap_or(rootfs))
        .join(path_stripped);

    if !backup_src.exists() {
        log::info!("restore: {path} does not exist in backup; skipping");
        return Ok(());
    }

    log::info!("restore: {path}");

    let dest_full = rootfs.join(path_stripped);
    let dest_dir = dest_full.parent().unwrap_or(rootfs);

    std::fs::create_dir_all(dest_dir).map_err(|e| FactoryResetError::RestoreFailed {
        path: PathBuf::from(path),
        reason: format!("Failed to create directory {}: {e}", dest_dir.display()),
    })?;

    let output = Command::new(CP_CMD)
        .arg("-a")
        .arg(&backup_src)
        .arg(dest_dir)
        .output()
        .map_err(|e| FactoryResetError::RestoreFailed {
            path: PathBuf::from(path),
            reason: format!("Failed to run cp: {e}"),
        })?;

    if !output.status.success() {
        return Err(FactoryResetError::RestoreFailed {
            path: PathBuf::from(path),
            reason: format!(
                "cp failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ),
        }
        .into());
    }

    run_sync()?;
    Ok(())
}

fn run_sync() -> Result<()> {
    let status = Command::new(SYNC_CMD)
        .status()
        .map_err(|e| FactoryResetError::Io(std::io::Error::other(format!("sync failed: {e}"))))?;
    if !status.success() {
        return Err(
            FactoryResetError::Io(std::io::Error::other(format!("sync exited with {status}")))
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn backup_path_nonexistent_source_silently_skips() {
        let temp = TempDir::new().unwrap();
        let rootfs = temp.path().join("rootfs");
        let backup = temp.path().join("backup");
        fs::create_dir_all(&rootfs).unwrap();
        fs::create_dir_all(&backup).unwrap();
        backup_path(&rootfs, "/etc/hostname", &backup).unwrap();
        // No backup created, no error
        assert!(!backup.join("etc/hostname").exists());
    }

    #[test]
    fn restore_path_restores_file_from_backup() {
        let temp = TempDir::new().unwrap();
        let rootfs = temp.path().join("rootfs");
        let backup = temp.path().join("backup");
        let backup_root = backup.join(rootfs.strip_prefix("/").unwrap_or(rootfs.as_path()));
        let restored = rootfs.join("etc/hostname");
        let backup_src = backup_root.join("etc/hostname");

        fs::create_dir_all(restored.parent().unwrap()).unwrap();
        fs::create_dir_all(backup_src.parent().unwrap()).unwrap();
        fs::write(&backup_src, "restored-hostname").unwrap();

        restore_path(&rootfs, "/etc/hostname", &backup).unwrap();

        assert_eq!(fs::read_to_string(restored).unwrap(), "restored-hostname");
    }

    #[test]
    fn restore_path_nonexistent_backup_silently_skips() {
        let temp = TempDir::new().unwrap();
        let rootfs = temp.path().join("rootfs");
        let backup = temp.path().join("backup");
        fs::create_dir_all(&rootfs).unwrap();
        fs::create_dir_all(&backup).unwrap();

        restore_path(&rootfs, "/etc/hostname", &backup).unwrap();
        assert!(!rootfs.join("etc/hostname").exists());
    }

    #[test]
    fn restore_all_success_on_empty_list() {
        let temp = TempDir::new().unwrap();
        let rootfs = temp.path().join("rootfs");
        let backup = temp.path().join("backup");
        fs::create_dir_all(&rootfs).unwrap();
        fs::create_dir_all(&backup).unwrap();

        let result = restore_all(&rootfs, &[], &backup).unwrap();
        assert!(matches!(result, RestoreResult::Success));
    }

    #[test]
    fn restore_all_partial_failure_accumulates_context() {
        let temp = TempDir::new().unwrap();
        let rootfs = temp.path().join("rootfs");
        let backup = temp.path().join("backup");
        let backup_root = backup.join(rootfs.strip_prefix("/").unwrap_or(rootfs.as_path()));

        // Backup file exists
        fs::create_dir_all(backup_root.join("etc")).unwrap();
        fs::write(backup_root.join("etc/hostname"), "host").unwrap();

        // Place a regular file at rootfs/etc so create_dir_all(rootfs/etc) fails,
        // triggering RestoreFailed for the path
        fs::create_dir_all(&rootfs).unwrap();
        fs::write(rootfs.join("etc"), "not-a-dir").unwrap();

        let result = restore_all(&rootfs, &["/etc/hostname".to_string()], &backup).unwrap();
        assert!(matches!(result, RestoreResult::PartialFailure { .. }));
        if let RestoreResult::PartialFailure { context, .. } = result {
            assert!(context.contains("etc/hostname:restore"));
        }
    }
}
```

- [ ] **Step 2: Run tests to confirm they compile and pass**

```bash
cargo test --features grub,gpt,factory-reset -- mode::factory_reset::backup_restore::tests
```

Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/mode/factory_reset/backup_restore.rs
git commit -m "feat(factory-reset): add backup_all + restore_all

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

## Task 6: Reformat module

**Files:**
- Create: `src/mode/factory_reset/reformat.rs`

No unit tests — `mkfs.ext4`/`tune2fs` require a real block device. Verified by integration test on hardware.

- [ ] **Step 1: Create `src/mode/factory_reset/reformat.rs`**

```rust
use std::path::Path;
use std::process::Command;

use crate::error::{FactoryResetError, Result};

const MKFS_EXT4_CMD: &str = "/sbin/mkfs.ext4";
const TUNE2FS_CMD: &str = "/sbin/tune2fs";
const MKFS_FORCE_FLAG: &str = "-F";
const MKFS_QUIET_FLAG: &str = "-q";
const TUNE2FS_MAX_MOUNT_COUNT_FLAG: &str = "-c";
const TUNE2FS_CHECK_INTERVAL_FLAG: &str = "-i";
const TUNE2FS_LABEL_FLAG: &str = "-L";
const TUNE2FS_NO_LIMIT: &str = "-1";
const TUNE2FS_ZERO_INTERVAL: &str = "0";

/// Reformat a partition as ext4 and apply omnect tunables.
///
/// Equivalent to:
/// ```sh
/// mkfs.ext4 -F -q <device>
/// tune2fs <device> -c -1 -i 0 -L <label>
/// ```
pub fn reformat_ext4(device: &Path, label: &str) -> Result<()> {
    log::info!("Reformatting {} with label={label}", device.display());

    let mkfs = Command::new(MKFS_EXT4_CMD)
        .args([MKFS_FORCE_FLAG, MKFS_QUIET_FLAG])
        .arg(device)
        .output()
        .map_err(|e| FactoryResetError::ReformatFailed {
            device: device.to_path_buf(),
            reason: format!("Failed to run mkfs.ext4: {e}"),
        })?;

    if !mkfs.status.success() {
        return Err(FactoryResetError::ReformatFailed {
            device: device.to_path_buf(),
            reason: format!(
                "mkfs.ext4 failed ({}): {}",
                mkfs.status,
                String::from_utf8_lossy(&mkfs.stderr)
            ),
        }
        .into());
    }

    let tune = Command::new(TUNE2FS_CMD)
        .arg(device)
        .args([
            TUNE2FS_MAX_MOUNT_COUNT_FLAG,
            TUNE2FS_NO_LIMIT,
            TUNE2FS_CHECK_INTERVAL_FLAG,
            TUNE2FS_ZERO_INTERVAL,
            TUNE2FS_LABEL_FLAG,
            label,
        ])
        .output()
        .map_err(|e| FactoryResetError::ReformatFailed {
            device: device.to_path_buf(),
            reason: format!("Failed to run tune2fs: {e}"),
        })?;

    if !tune.status.success() {
        return Err(FactoryResetError::ReformatFailed {
            device: device.to_path_buf(),
            reason: format!(
                "tune2fs failed ({}): {}",
                tune.status,
                String::from_utf8_lossy(&tune.stderr)
            ),
        }
        .into());
    }

    log::info!(
        "Reformatted {} with label={label} successfully",
        device.display()
    );
    Ok(())
}
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo check --features grub,gpt,factory-reset
```

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add src/mode/factory_reset/reformat.rs
git commit -m "feat(factory-reset): add reformat_ext4

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

## Task 7: Orchestration module + declare submodule in mode

**Files:**
- Modify: `src/runtime/mod.rs`
- Create: `src/mode/factory_reset/mod.rs`
- Modify: `src/mode/mod.rs` (add `pub mod factory_reset` declaration — **not** the BootMode changes yet)

- [ ] **Step 1: Re-export `FactoryResetStatus` and `FactoryResetStatusCode` from `src/runtime/mod.rs`**

`factory_reset/mod.rs` imports these types from `crate::runtime`. Add them to the re-exports (the `omnect_device_service` sub-module is private so they must be re-exported):

```rust
// Before
pub use self::omnect_device_service::{
    ODS_RUNTIME_DIR, OdsStatus, ResizeOutcome, ResizeStatus, create_ods_runtime_files,
};

// After
pub use self::omnect_device_service::{
    ODS_RUNTIME_DIR, OdsStatus, ResizeOutcome, ResizeStatus, create_ods_runtime_files,
    FactoryResetStatus, FactoryResetStatusCode,
};
```

- [ ] **Step 2: Create stub `src/mode/factory_reset/mod.rs` with just the umount test target**

```rust
pub mod backup_restore;
pub mod config;
pub mod reformat;

use std::path::PathBuf;

use log::warn;

use crate::{
    bootloader::BootEnvKey,
    error::{FactoryResetError, InitramfsError, Result},
    filesystem::{
        MountOptions, MountPoint, is_path_mounted, mount, mount_points, setup_data_overlay,
        setup_etc_overlay, umount,
    },
    mode::{BootContext, factory_reset::backup_restore::RestoreResult},
    partition::{PartitionLayout, PartitionName},
    runtime::{FactoryResetStatus, FactoryResetStatusCode},
};

use crate::mode::factory_reset::{
    backup_restore::{backup_all, restore_all},
    config::{FactoryResetConfig, build_preserve_list},
    reformat::reformat_ext4,
};

const FACTORY_RESET_BACKUP_DIR: &str = "/tmp/factory_reset/backup";

/// Entry point for factory-reset mode.
///
/// Clears the trigger env var, runs the reset sequence, writes status to
/// `ods_status`, and always delegates to Normal boot — never blocks the device.
pub fn run(mut ctx: BootContext<'_>, config: FactoryResetConfig) -> Result<()> {
    if let Some(bl) = ctx.boot_env.available_mut() {
        if let Err(e) = bl.set_env(BootEnvKey::FactoryReset, None) {
            warn!("Failed to clear factory-reset bootloader var: {e}; proceeding anyway");
        }
    }

    let status = match run_reset(ctx.layout, ctx.rootfs, &config) {
        Ok(status) => status,
        Err(e) => {
            warn!("Factory reset failed: {e}; continuing with Normal boot");
            let code = match &e {
                InitramfsError::FactoryReset(FactoryResetError::InvalidConfig(_)) => {
                    FactoryResetStatusCode::Invalid
                }
                InitramfsError::FactoryReset(FactoryResetError::BackupFailed { .. })
                | InitramfsError::FactoryReset(FactoryResetError::RestoreFailed { .. }) => {
                    FactoryResetStatusCode::Error
                }
                _ => FactoryResetStatusCode::ConfigError,
            };
            FactoryResetStatus {
                status: code,
                error: Some(e.to_string()),
                context: None,
                paths: vec![],
            }
        }
    };
    ctx.ods_status.set_factory_reset(status);

    crate::mode::normal::run(ctx)
}

/// Inner reset sequence — returns the final status on success/partial-restore,
/// or `Err` on hard failures (mount, backup, reformat).
fn run_reset(
    layout: &PartitionLayout,
    rootfs: &std::path::Path,
    config: &FactoryResetConfig,
) -> Result<FactoryResetStatus> {
    if config.mode != 1 {
        let msg = format!("factory reset mode {} is not supported", config.mode);
        warn!("{msg}");
        return Err(FactoryResetError::InvalidConfig(msg).into());
    }

    let mut mounts: Vec<PathBuf> = Vec::new();
    factory_reset_mount(layout, rootfs, &mut mounts)?;

    let preserve_list = build_preserve_list(config, rootfs).inspect_err(|_| {
        let _ = factory_reset_umount(&mut mounts);
    })?;

    let backup_dir = PathBuf::from(FACTORY_RESET_BACKUP_DIR);
    backup_all(rootfs, &preserve_list, &backup_dir).inspect_err(|_| {
        let _ = factory_reset_umount(&mut mounts);
    })?;

    factory_reset_umount(&mut mounts)?;

    let data_dev = layout.partitions.get(&PartitionName::Data).ok_or_else(|| {
        FactoryResetError::MountError("data partition not found in layout".to_string())
    })?;
    let etc_dev = layout.partitions.get(&PartitionName::Etc).ok_or_else(|| {
        FactoryResetError::MountError("etc partition not found in layout".to_string())
    })?;

    reformat_ext4(data_dev, "data")?;
    reformat_ext4(etc_dev, "etc")?;

    factory_reset_mount(layout, rootfs, &mut mounts)?;

    let restore_result = restore_all(rootfs, &preserve_list, &backup_dir).inspect_err(|_| {
        let _ = factory_reset_umount(&mut mounts);
    })?;

    factory_reset_umount(&mut mounts)?;

    log::info!("factory-reset complete");

    let status = match restore_result {
        RestoreResult::Success => FactoryResetStatus {
            status: FactoryResetStatusCode::Success,
            error: None,
            context: None,
            paths: preserve_list,
        },
        RestoreResult::PartialFailure { context, error } => FactoryResetStatus {
            status: FactoryResetStatusCode::Error,
            error: Some(error),
            context: Some(context),
            paths: preserve_list,
        },
    };

    Ok(status)
}

/// Mount factory (ro, if present), etc (rw), data (rw) and set up overlays.
///
/// Tracks each mount in `mounts` so `factory_reset_umount` can reverse them.
fn factory_reset_mount(
    layout: &PartitionLayout,
    rootfs: &std::path::Path,
    mounts: &mut Vec<PathBuf>,
) -> Result<()> {
    if let Some(factory_dev) = layout.partitions.get(&PartitionName::Factory) {
        let factory_mount = rootfs.join(mount_points::FACTORY_PARTITION);
        std::fs::create_dir_all(&factory_mount)?;
        mount(MountPoint::new(
            factory_dev,
            &factory_mount,
            MountOptions::ext4_readonly(),
        ))
        .map_err(|e| FactoryResetError::MountError(format!("factory: {e}")))?;
        mounts.push(factory_mount);
    }

    if let Some(etc_dev) = layout.partitions.get(&PartitionName::Etc) {
        let etc_mount = rootfs.join(mount_points::ETC_PARTITION);
        std::fs::create_dir_all(&etc_mount)?;
        mount(MountPoint::new(
            etc_dev,
            &etc_mount,
            MountOptions::ext4_readwrite(),
        ))
        .map_err(|e| FactoryResetError::MountError(format!("etc: {e}")))?;
        mounts.push(etc_mount);
    }

    if let Some(data_dev) = layout.partitions.get(&PartitionName::Data) {
        let data_mount = rootfs.join(mount_points::DATA_PARTITION);
        std::fs::create_dir_all(&data_mount)?;
        mount(MountPoint::new(
            data_dev,
            &data_mount,
            MountOptions::ext4_readwrite(),
        ))
        .map_err(|e| FactoryResetError::MountError(format!("data: {e}")))?;
        mounts.push(data_mount);
    }

    setup_etc_overlay(rootfs)
        .map_err(|e| FactoryResetError::MountError(format!("etc overlay: {e}")))?;
    mounts.push(rootfs.join("etc"));

    setup_data_overlay(rootfs)
        .map_err(|e| FactoryResetError::MountError(format!("data overlay: {e}")))?;
    mounts.push(rootfs.join("home"));
    mounts.push(rootfs.join("var/lib"));
    mounts.push(rootfs.join("usr/local"));
    #[cfg(feature = "persistent-var-log")]
    mounts.push(rootfs.join("var/log"));

    Ok(())
}

/// Unmount all factory-reset mounts in reverse order.
///
/// Continues on individual failures and returns the last error, if any, so
/// the caller always attempts to unmount everything regardless of partial failures.
pub(crate) fn factory_reset_umount(mounts: &mut Vec<PathBuf>) -> Result<()> {
    let mut last_err: Option<InitramfsError> = None;
    for path in mounts.drain(..).rev() {
        if is_path_mounted(&path).unwrap_or(false) {
            if let Err(e) = umount(&path) {
                warn!("Failed to unmount {}: {e}", path.display());
                last_err = Some(e.into());
            }
        }
    }
    if let Some(e) = last_err {
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_reset_umount_succeeds_for_empty_mount_list() {
        let mut mounts: Vec<PathBuf> = Vec::new();
        assert!(factory_reset_umount(&mut mounts).is_ok());
        assert!(mounts.is_empty());
    }
}
```

- [ ] **Step 2: Declare the submodule in `src/mode/mod.rs`**

Add the module declaration (feature-gated) after the `pub mod normal;` line:

```rust
#[cfg(feature = "factory-reset")]
pub mod factory_reset;
```

- [ ] **Step 3: Run the umount test to confirm it passes**

```bash
cargo test --features grub,gpt,factory-reset -- mode::factory_reset::tests::factory_reset_umount_succeeds_for_empty_mount_list
```

Expected: 1 test passes.

- [ ] **Step 4: Run all factory-reset tests so far**

```bash
cargo test --features grub,gpt,factory-reset
```

Expected: all existing tests pass, new factory-reset tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/mode/factory_reset/mod.rs src/mode/mod.rs
git commit -m "feat(factory-reset): add orchestration run() + factory_reset_umount

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

## Task 8: Mode dispatch — `BootMode::FactoryReset` + `detect()`

**Files:**
- Modify: `src/mode/mod.rs`

- [ ] **Step 1: Write the failing tests**

In `src/mode/mod.rs`, in the existing `#[cfg(test)] mod tests` block, add:

```rust
#[cfg(feature = "factory-reset")]
mod factory_reset_detect_tests {
    use super::*;
    use crate::bootloader::{BootEnvKey, MockBootEnv};

    #[test]
    fn detect_normal_when_factory_reset_key_absent() {
        let mock = MockBootEnv::new();
        let mode = BootMode::detect(Some(&mock)).unwrap();
        assert!(matches!(mode, BootMode::Normal));
    }

    #[test]
    fn detect_factory_reset_when_key_present_valid_json() {
        let mock = MockBootEnv::new()
            .with_env(BootEnvKey::FactoryReset, r#"{"mode":1,"preserve":[]}"#);
        let mode = BootMode::detect(Some(&mock)).unwrap();
        assert!(matches!(mode, BootMode::FactoryReset(_)));
        if let BootMode::FactoryReset(config) = mode {
            assert_eq!(config.mode, 1);
            assert!(config.preserve.is_empty());
        }
    }

    #[test]
    fn detect_normal_when_key_present_invalid_json() {
        let mock = MockBootEnv::new().with_env(BootEnvKey::FactoryReset, "not-json");
        let mode = BootMode::detect(Some(&mock)).unwrap();
        assert!(matches!(mode, BootMode::Normal));
    }

    #[test]
    fn detect_normal_when_bootloader_unavailable() {
        let mode = BootMode::detect(None).unwrap();
        assert!(matches!(mode, BootMode::Normal));
    }

    #[test]
    fn detect_normal_when_get_env_fails() {
        let mock = MockBootEnv::new().with_get_env_error();
        let mode = BootMode::detect(Some(&mock)).unwrap();
        assert!(matches!(mode, BootMode::Normal));
    }
}
```

- [ ] **Step 2: Run to confirm compile failure**

```bash
cargo test --features grub,gpt,factory-reset 2>&1 | grep "error\[" | head -5
```

Expected: compile error — `BootMode::FactoryReset` not found.

- [ ] **Step 3: Add `BootMode::FactoryReset` variant and extend `detect()`**

In `src/mode/mod.rs`, update the `BootMode` enum:

```rust
/// The detected boot mode to execute.
pub enum BootMode {
    Normal,
    #[cfg(feature = "factory-reset")]
    FactoryReset(factory_reset::config::FactoryResetConfig),
}
```

Replace the `detect()` implementation:

```rust
impl BootMode {
    /// Detect the boot mode from the boot environment.
    ///
    /// When the `factory-reset` bootloader env key is set and contains valid
    /// JSON, returns `FactoryReset`. Falls back to `Normal` on any env-read
    /// or JSON parse error — never blocks boot.
    pub fn detect(bl: Option<&dyn BootEnv>) -> Result<Self> {
        #[cfg(feature = "factory-reset")]
        if let Some(bl) = bl {
            match bl.get_env(BootEnvKey::FactoryReset) {
                Ok(Some(json)) => {
                    match factory_reset::config::FactoryResetConfig::parse(&json) {
                        Ok(config) => return Ok(Self::FactoryReset(config)),
                        Err(e) => {
                            log::warn!(
                                "factory-reset: invalid config JSON, booting normally: {e}"
                            );
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    log::warn!("factory-reset: failed to read env, booting normally: {e}");
                }
            }
        }

        Ok(Self::Normal)
    }
}
```

Also add the `BootEnvKey` import to the top of `src/mode/mod.rs` (it's used in the cfg-gated block):

```rust
#[cfg(feature = "factory-reset")]
use crate::bootloader::BootEnvKey;
```

- [ ] **Step 4: Run the new tests to confirm they pass**

```bash
cargo test --features grub,gpt,factory-reset -- factory_reset_detect_tests
```

Expected: 5 tests pass.

- [ ] **Step 5: Confirm existing detect tests still pass**

```bash
cargo test --features grub,gpt,factory-reset -- mode::tests
```

Expected: existing `detect_normal_*` tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/mode/mod.rs
git commit -m "feat(mode): add BootMode::FactoryReset + detect() support

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

## Task 9: Dead code removal and dispatch

**Files:**
- Modify: `src/runtime/omnect_device_service.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Remove dead code from `src/runtime/omnect_device_service.rs`**

Remove the `FACTORY_RESET_STATUS_FILE` constant and the `copy_factory_reset_status()` function entirely.

Remove this constant (line ~35):
```rust
/// Factory reset status file (in /tmp)
const FACTORY_RESET_STATUS_FILE: &str = "/tmp/factory-reset.json";
```

Remove this function call in `create_ods_runtime_files()` (lines ~294–298):
```rust
// Copy factory reset status if exists
if let Some(dst) = copy_factory_reset_status(ods_dir)? {
    set_ownership(&dst, uid, gid)?;
    set_mode(&dst, FilePermission::FileRestricted)?;
}
```

Remove the `copy_factory_reset_status()` function (lines ~406–427):
```rust
/// Copy factory reset status from /tmp if it exists.
/// Returns the destination path if the file was copied.
fn copy_factory_reset_status(ods_dir: &Path) -> Result<Option<PathBuf>> {
    // ... entire function body
}
```

- [ ] **Step 2: Add the factory-reset dispatch arm to `src/lib.rs`**

In `run_init()`, replace the `match BootMode::detect(...)` block:

```rust
// Before
match BootMode::detect(ctx.boot_env.available())? {
    BootMode::Normal => mode::normal::run(ctx),
}

// After
match BootMode::detect(ctx.boot_env.available())? {
    BootMode::Normal => mode::normal::run(ctx),
    #[cfg(feature = "factory-reset")]
    BootMode::FactoryReset(config) => mode::factory_reset::run(ctx, config),
}
```

- [ ] **Step 3: Verify all four non-factory-reset feature combinations compile**

```bash
cargo check --features grub,gpt && \
cargo check --features grub,dos && \
cargo check --features uboot,gpt && \
cargo check --features uboot,dos
```

Expected: all pass with no errors.

- [ ] **Step 4: Verify all four factory-reset feature combinations compile**

```bash
cargo check --features grub,gpt,factory-reset && \
cargo check --features grub,dos,factory-reset && \
cargo check --features uboot,gpt,factory-reset && \
cargo check --features uboot,dos,factory-reset
```

Expected: all pass with no errors.

- [ ] **Step 5: Commit**

```bash
git add src/runtime/omnect_device_service.rs src/lib.rs
git commit -m "feat(factory-reset): wire dispatch + remove dead tmp-file code

Signed-off-by: Joerg Zeidler <62105035+JoergZeidler@users.noreply.github.com>"
```

---

## Task 10: Full validation

- [ ] **Step 1: Run all tests without factory-reset (regression check)**

```bash
cargo test --features grub,gpt && \
cargo test --features grub,dos && \
cargo test --features uboot,gpt && \
cargo test --features uboot,dos
```

Expected: all pass.

- [ ] **Step 2: Run all tests with factory-reset enabled**

```bash
cargo test --features grub,gpt,factory-reset && \
cargo test --features grub,dos,factory-reset && \
cargo test --features uboot,gpt,factory-reset && \
cargo test --features uboot,dos,factory-reset
```

Expected: all pass.

- [ ] **Step 3: Run `cargo clippy` on all factory-reset combinations**

```bash
cargo clippy --tests --features grub,gpt,factory-reset -- -D warnings && \
cargo clippy --tests --features uboot,gpt,factory-reset -- -D warnings
```

Expected: no warnings.

- [ ] **Step 4: Run `cargo fmt` check**

```bash
cargo fmt -- --check
```

Expected: no formatting issues. If issues exist: `cargo fmt` then re-check.

- [ ] **Step 5: Tag the branch as ready for review**

```bash
git log --oneline feat/factory-reset-mode1
```

Expected output (one commit per task):
```
<sha> feat(factory-reset): wire dispatch + remove dead tmp-file code
<sha> feat(mode): add BootMode::FactoryReset + detect() support
<sha> feat(factory-reset): add orchestration run() + factory_reset_umount
<sha> feat(factory-reset): add reformat_ext4
<sha> feat(factory-reset): add backup_all + restore_all
<sha> feat(factory-reset): add FactoryResetConfig + build_preserve_list
<sha> feat(bootloader): add BootEnvKey::FactoryReset
<sha> feat(error): add FactoryResetError + ContinueDegraded recovery class
<sha> feat(filesystem): add umount + factory-reset feature flag
<sha> docs(specs): add factory-reset mode 1 design
```
