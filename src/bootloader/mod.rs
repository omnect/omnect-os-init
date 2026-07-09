//! BootEnv abstraction module
//!
//! This module provides a trait-based abstraction over different bootloaders
//! (GRUB and U-Boot) to allow unified access to bootloader environment variables.

#[cfg(feature = "grub")]
mod grub;
mod types;
#[cfg(feature = "uboot")]
mod uboot;

use std::borrow::Cow;

use crate::error::BootEnvError;
use crate::filesystem::FsckExitCode;
use crate::partition::PartitionName;

#[cfg(feature = "grub")]
pub use self::grub::GrubBootEnv;
#[cfg(feature = "uboot")]
pub use self::uboot::UBootBootEnv;

pub type Result<T> = std::result::Result<T, BootEnvError>;

/// Upper bound (bytes) on the failure reason stored in the bootloader env by
/// `save_factory_reset_failure`. grubenv is a fixed ~1024-byte shared block, so
/// the diagnostic is kept short and human-readable rather than exhaustive.
///
/// `save_factory_reset_failure` lands in a follow-up change; until then this
/// constant has no non-test caller.
#[cfg(feature = "factory-reset")]
#[allow(dead_code)]
const MAX_FACTORY_RESET_FAILURE_REASON_LEN: usize = 128;

/// Truncate `s` to at most `max_bytes`, never splitting a multi-byte UTF-8
/// character (a naive byte slice would panic on a non-ASCII boundary).
///
/// `save_factory_reset_failure` lands in a follow-up change; until then this
/// helper has no non-test caller.
#[cfg(feature = "factory-reset")]
#[allow(dead_code)]
fn truncate_on_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Decoded fsck result stored in the bootloader environment.
#[derive(Debug, Clone, PartialEq)]
pub struct FsckRecord {
    /// Typed exit code from fsck.
    pub exit_code: FsckExitCode,
    /// Combined stdout + stderr output from fsck.
    pub output: String,
}

/// Typed key for bootloader environment variables.
///
/// Use this instead of raw `&str` keys to prevent typos and make all
/// known env-var names visible in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootEnvKey {
    /// `omnect_validate_update` — OTA update validation state.
    ValidateUpdate,
    /// `omnect_bootloader_updated` — whether the bootloader itself was updated.
    BootloaderUpdated,
    /// `omnect_fsck_<partition>` — fsck result for the given partition.
    FsckStatus(PartitionName),
    /// `omnect_first_boot_done` — set to `"1"` after the first successful
    /// `run_init`. Unified first-boot sentinel; read by the resize-data init
    /// setup step and the first-boot detection in `run_init`.
    FirstBootDone,
    #[cfg(feature = "factory-reset")]
    /// `factory-reset` — JSON trigger set by ODS to request a factory reset.
    /// Value format: `{"mode":1,"preserve":["applications","network"]}`.
    /// Cleared by the initramfs as the first step of the reset sequence.
    FactoryReset,
}

impl BootEnvKey {
    /// Returns the env-var name as it is stored in the bootloader environment.
    pub fn as_str(&self) -> Cow<'static, str> {
        match self {
            Self::ValidateUpdate => Cow::Borrowed("omnect_validate_update"),
            Self::BootloaderUpdated => Cow::Borrowed("omnect_bootloader_updated"),
            Self::FsckStatus(p) => Cow::Owned(format!("omnect_fsck_{p}")),
            Self::FirstBootDone => Cow::Borrowed("omnect_first_boot_done"),
            #[cfg(feature = "factory-reset")]
            Self::FactoryReset => Cow::Borrowed("factory-reset"),
        }
    }
}

/// Trait for boot environment access
///
/// This trait abstracts the differences between GRUB and U-Boot boot
/// environment access, allowing the rest of the codebase to work with
/// bootloader variables in a unified way.
///
/// The minimal required interface is `get_env` and `set_env`. The fsck helpers
/// (`save_fsck_status`, `get_fsck_status`, `clear_fsck_status`) have default
/// implementations that encode/decode via `get_env`/`set_env`. BootEnv
/// backends with custom storage strategies (e.g. GRUB's per-partition files)
/// should override the relevant methods.
pub trait BootEnv: Send + Sync {
    /// Get the value of a bootloader environment variable
    ///
    /// Returns `Ok(None)` if the variable doesn't exist.
    /// Returns `Err` if there was an error accessing the bootloader environment.
    fn get_env(&self, key: BootEnvKey) -> Result<Option<String>>;

    /// Set or delete a bootloader environment variable
    ///
    /// Pass `Some(value)` to set the variable, or `None` to delete it.
    fn set_env(&mut self, key: BootEnvKey, value: Option<&str>) -> Result<()>;

    /// Save fsck result to bootloader environment.
    ///
    /// Stores exit code and full fsck output as gzip+base64 encoded string so the
    /// diagnostic text survives the reboot required after fsck corrects errors.
    fn save_fsck_status(
        &mut self,
        partition: PartitionName,
        code: FsckExitCode,
        output: &str,
    ) -> Result<()> {
        let encoded = types::encode_fsck_output(code.bits(), output);
        self.set_env(BootEnvKey::FsckStatus(partition), Some(&encoded))
    }

    /// Get fsck status from bootloader environment.
    ///
    /// Returns the decoded `FsckRecord` if a value is present,
    /// or `None` if no status was stored for this partition.
    fn get_fsck_status(&self, partition: PartitionName) -> Result<Option<FsckRecord>> {
        Ok(self
            .get_env(BootEnvKey::FsckStatus(partition))?
            .and_then(|v| types::decode_fsck_output(&v)))
    }

    /// Clear fsck status from bootloader environment
    fn clear_fsck_status(&mut self, partition: PartitionName) -> Result<()> {
        self.set_env(BootEnvKey::FsckStatus(partition), None)
    }
}

/// Opens the appropriate bootloader environment implementation based on the build-time feature flag.
///
/// The bootloader type is a build-time property of the target platform:
/// - `grub` feature: x86-64 EFI targets using GRUB (`grub-editenv`)
/// - `uboot` feature: ARM targets using U-Boot (`fw_printenv`/`fw_setenv`)
///
/// Exactly one of `grub` or `uboot` must be enabled; build.rs enforces this.
pub fn open_boot_env() -> Result<Box<dyn BootEnv>> {
    #[cfg(feature = "grub")]
    return Ok(Box::new(GrubBootEnv::new()?));

    #[cfg(feature = "uboot")]
    return Ok(Box::new(UBootBootEnv::new()?));
}

/// Boot environment state: opened successfully, or unavailable with reason.
pub enum BootEnvState {
    /// Boot environment opened successfully.
    Available(Box<dyn BootEnv>),
    /// Boot environment could not be opened.
    Degraded(BootEnvError),
}

impl BootEnvState {
    /// Returns `true` if the bootloader environment is unavailable.
    pub fn is_degraded(&self) -> bool {
        matches!(self, Self::Degraded(_))
    }

    /// Returns a mutable reference to the boot env accessor if available.
    pub fn available_mut(&mut self) -> Option<&mut dyn BootEnv> {
        match self {
            Self::Available(b) => Some(b.as_mut()),
            Self::Degraded(_) => None,
        }
    }

    /// Returns a shared reference to the boot env accessor if available.
    pub fn available(&self) -> Option<&dyn BootEnv> {
        match self {
            Self::Available(b) => Some(b.as_ref()),
            Self::Degraded(_) => None,
        }
    }
}

/// The outcome of `classify_boot_env`.
pub enum BootEnvDecision {
    /// Continue init with this boot env state.
    Continue(BootEnvState),
    /// Abort init with this error — caller passes it to `handle_fatal_error`.
    Abort(crate::error::InitramfsError),
}

/// Decide how to proceed based on the bootloader open result and the image type.
///
/// - `Ok(bl)` → `Continue(Available(bl))` — successful open, both image types.
/// - `Err(e)` + release-image → `Continue(Degraded(e))` — degraded boot continues.
/// - `Err(e)` + debug-image → `Abort(DegradedBoot(e))` — enter debug shell immediately.
pub fn classify_boot_env(
    open_result: std::result::Result<Box<dyn BootEnv>, BootEnvError>,
    is_release_image: bool,
) -> BootEnvDecision {
    match open_result {
        Ok(bl) => BootEnvDecision::Continue(BootEnvState::Available(bl)),
        Err(e) if is_release_image => BootEnvDecision::Continue(BootEnvState::Degraded(e)),
        Err(e) => BootEnvDecision::Abort(crate::error::InitramfsError::DegradedBoot(e)),
    }
}

/// Create a mock boot environment for testing
#[cfg(any(test, feature = "test-utils"))]
pub fn create_mock_bootloader() -> MockBootEnv {
    MockBootEnv::new()
}

/// Mock boot environment for testing
#[cfg(any(test, feature = "test-utils"))]
#[derive(Default)]
pub struct MockBootEnv {
    env: std::collections::HashMap<String, String>,
    /// fsck results stored as typed records — no subprocess encoding needed in tests.
    fsck: std::collections::HashMap<PartitionName, FsckRecord>,
    /// Keys passed to set_env, in call order. Used by tests to verify set_env was/wasn't called.
    pub set_env_calls: Vec<BootEnvKey>,
    /// When true, `get_env` returns an error instead of looking up the key.
    get_env_errors: bool,
    /// When true, `set_env` returns an error instead of setting the key.
    set_env_errors: bool,
}

#[cfg(any(test, feature = "test-utils"))]
impl MockBootEnv {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_env(mut self, key: BootEnvKey, value: &str) -> Self {
        self.env.insert(key.as_str().to_string(), value.to_string());
        self
    }

    pub fn with_get_env_error(mut self) -> Self {
        self.get_env_errors = true;
        self
    }

    pub fn with_set_env_error(mut self) -> Self {
        self.set_env_errors = true;
        self
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl BootEnv for MockBootEnv {
    fn get_env(&self, key: BootEnvKey) -> Result<Option<String>> {
        if self.get_env_errors {
            return Err(crate::error::BootEnvError::CommandFailed {
                command: "mock".into(),
                reason: "injected error".into(),
            });
        }
        Ok(self.env.get(key.as_str().as_ref()).cloned())
    }

    fn set_env(&mut self, key: BootEnvKey, value: Option<&str>) -> Result<()> {
        if self.set_env_errors {
            return Err(crate::error::BootEnvError::CommandFailed {
                command: "mock".into(),
                reason: "injected set_env error".into(),
            });
        }
        self.set_env_calls.push(key);
        match value {
            Some(v) => {
                self.env.insert(key.as_str().to_string(), v.to_string());
            }
            None => {
                self.env.remove(key.as_str().as_ref());
            }
        }
        Ok(())
    }

    fn save_fsck_status(
        &mut self,
        partition: PartitionName,
        code: FsckExitCode,
        output: &str,
    ) -> Result<()> {
        self.fsck.insert(
            partition,
            FsckRecord {
                exit_code: code,
                output: output.to_string(),
            },
        );
        Ok(())
    }

    fn get_fsck_status(&self, partition: PartitionName) -> Result<Option<FsckRecord>> {
        Ok(self.fsck.get(&partition).cloned())
    }

    fn clear_fsck_status(&mut self, partition: PartitionName) -> Result<()> {
        self.fsck.remove(&partition);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod classify_tests {
        use super::*;
        use crate::error::{BootEnvError, InitramfsError};

        fn ok_bootloader() -> std::result::Result<Box<dyn BootEnv>, BootEnvError> {
            Ok(Box::new(MockBootEnv::new()))
        }

        fn err_bootloader() -> std::result::Result<Box<dyn BootEnv>, BootEnvError> {
            Err(BootEnvError::CommandFailed {
                command: "boot-env-tool".into(),
                reason: "not found".into(),
            })
        }

        #[test]
        fn ok_release_image_returns_available_not_degraded() {
            let decision = classify_boot_env(ok_bootloader(), true);
            assert!(matches!(
                decision,
                BootEnvDecision::Continue(BootEnvState::Available(_))
            ));
        }

        #[test]
        fn ok_debug_image_returns_available_not_degraded() {
            let decision = classify_boot_env(ok_bootloader(), false);
            assert!(matches!(
                decision,
                BootEnvDecision::Continue(BootEnvState::Available(_))
            ));
        }

        #[test]
        fn err_release_image_returns_degraded_continue() {
            let decision = classify_boot_env(err_bootloader(), true);
            assert!(matches!(
                decision,
                BootEnvDecision::Continue(BootEnvState::Degraded(_))
            ));
        }

        #[test]
        fn err_debug_image_returns_abort_with_degraded_boot_error() {
            let decision = classify_boot_env(err_bootloader(), false);
            assert!(matches!(
                decision,
                BootEnvDecision::Abort(InitramfsError::DegradedBoot(_))
            ));
        }
    }

    #[test]
    fn test_mock_bootloader_get_set() {
        let mut bl = MockBootEnv::new();

        bl.set_env(BootEnvKey::ValidateUpdate, Some("1")).unwrap();
        assert_eq!(
            bl.get_env(BootEnvKey::ValidateUpdate).unwrap(),
            Some("1".to_string())
        );

        bl.set_env(BootEnvKey::ValidateUpdate, None).unwrap();
        assert_eq!(bl.get_env(BootEnvKey::ValidateUpdate).unwrap(), None);
    }

    #[test]
    fn test_mock_bootloader_with_env() {
        let bl = MockBootEnv::new()
            .with_env(BootEnvKey::ValidateUpdate, "1")
            .with_env(BootEnvKey::BootloaderUpdated, "0");

        assert_eq!(
            bl.get_env(BootEnvKey::ValidateUpdate).unwrap(),
            Some("1".to_string())
        );
        assert_eq!(
            bl.get_env(BootEnvKey::BootloaderUpdated).unwrap(),
            Some("0".to_string())
        );
        assert_eq!(
            bl.get_env(BootEnvKey::FsckStatus(PartitionName::Boot))
                .unwrap(),
            None
        );
    }

    #[test]
    fn test_mock_bootloader_fsck_status() {
        use crate::partition::PartitionName;
        let mut bl = MockBootEnv::new();

        bl.save_fsck_status(
            PartitionName::Boot,
            FsckExitCode::CORRECTED,
            "errors corrected on pass 1",
        )
        .unwrap();

        let retrieved = bl.get_fsck_status(PartitionName::Boot).unwrap();
        assert_eq!(
            retrieved,
            Some(FsckRecord {
                exit_code: FsckExitCode::CORRECTED,
                output: "errors corrected on pass 1".to_string()
            })
        );

        bl.clear_fsck_status(PartitionName::Boot).unwrap();
        assert_eq!(bl.get_fsck_status(PartitionName::Boot).unwrap(), None);
    }

    #[test]
    fn first_boot_done_key_string() {
        // Pin the wire string. ODS / cloud / external tools may match on it
        // so changing it would be a wire-format break.
        assert_eq!(
            BootEnvKey::FirstBootDone.as_str().as_ref(),
            "omnect_first_boot_done"
        );
    }

    #[cfg(feature = "factory-reset")]
    #[test]
    fn factory_reset_key_as_str() {
        assert_eq!(BootEnvKey::FactoryReset.as_str().as_ref(), "factory-reset");
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

    #[cfg(feature = "factory-reset")]
    mod truncate_tests {
        use super::super::truncate_on_char_boundary;

        #[test]
        fn returns_input_when_within_limit() {
            assert_eq!(truncate_on_char_boundary("short", 128), "short");
        }

        #[test]
        fn truncates_ascii_at_limit() {
            assert_eq!(truncate_on_char_boundary("abcdef", 3), "abc");
        }

        #[test]
        fn never_splits_a_multibyte_char() {
            // "é" is 2 bytes (0xC3 0xA9). Truncating to 1 byte must not split it.
            let s = "aé";
            let out = truncate_on_char_boundary(s, 2);
            assert!(s.is_char_boundary(out.len()));
            assert_eq!(out, "a");
        }
    }
}
