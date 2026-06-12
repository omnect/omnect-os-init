//! Preflight step: data partition auto-resize
//!
//! With boot env available: checks the `omnect_resized_data` guard and, if
//! absent, expands the data partition via `filesystem::resize_data`.
//!
//! On a degraded boot (boot env unavailable): runs resize without the guard.
//! Only reached on release-images; debug-images abort in lib.rs before
//! preflight executes.
//!
//! All failure modes except `FsckRequiresReboot` are absorbed as
//! best-effort: `OdsStatus.resize_data` is populated and boot continues.
//! `FsckRequiresReboot` is propagated so Plan A's recovery policy triggers a
//! controlled reboot.

use crate::bootloader::BootEnvKey;
use crate::error::{FilesystemError, InitramfsError, ResizeDataError, Result};
use crate::preflight::PreflightCtx;
use crate::runtime::{OdsStatus, ResizeOutcome, ResizeStatus};

pub fn run(ctx: &mut PreflightCtx<'_, '_, '_>) -> Result<()> {
    handle_result(attempt(ctx), ctx.ods_status)
}

fn handle_result(result: Result<()>, ods_status: &mut OdsStatus) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        // Data partition fsck found uncorrectable errors: a reboot with the
        // corrected partition is the right remedy, not skipping resize.
        Err(
            e @ InitramfsError::ResizeData(ResizeDataError::Filesystem(
                FilesystemError::FsckRequiresReboot { .. },
            )),
        ) => Err(e),
        // Every other failure is non-fatal: record the indicator and continue.
        Err(ref e) => {
            let outcome = outcome_for(e);
            let reason = e.to_string();
            log::warn!("resize-data preflight skipped: {reason}");
            ods_status.set_resize_status(ResizeStatus { outcome, reason });
            Ok(())
        }
    }
}

fn attempt(ctx: &mut PreflightCtx<'_, '_, '_>) -> Result<()> {
    match ctx.boot_env.available_mut() {
        Some(bl) => {
            if bl.get_env(BootEnvKey::ResizedData)?.is_some() {
                log::debug!("resize-data preflight: guard present; already resized");
                return Ok(());
            }
            crate::filesystem::resize_data::resize_if_needed(ctx.layout, Some(bl))
        }
        None => {
            log::warn!("resize-data: running without bootloader guard (degraded boot)");
            crate::filesystem::resize_data::resize_if_needed(ctx.layout, None)
        }
    }
}

fn outcome_for(e: &InitramfsError) -> ResizeOutcome {
    match e {
        InitramfsError::ResizeData(ResizeDataError::Filesystem(FilesystemError::FsckFailed {
            ..
        })) => ResizeOutcome::SkippedFsck,
        InitramfsError::ResizeData(ResizeDataError::InvalidDevicePath(_))
        | InitramfsError::ResizeData(ResizeDataError::ExtendedPartitionNotFound(_))
        | InitramfsError::ResizeData(ResizeDataError::NonUtf8Path(_)) => {
            ResizeOutcome::InvalidLayout
        }
        // CommandFailed and Io: external tool or I/O failure.
        _ => ResizeOutcome::ToolError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootloader::{BootEnvKey, BootEnvState, MockBootEnv};
    use crate::error::BootEnvError;
    use crate::partition::{PartitionLayout, RootDevice};
    use crate::preflight::PreflightCtx;
    use crate::runtime::OdsStatus;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn empty_layout() -> PartitionLayout {
        PartitionLayout {
            partitions: HashMap::new(),
            device: RootDevice {
                base: PathBuf::from("/dev/sda"),
                partition_sep: "",
                root_partition: PathBuf::from("/dev/sda2"),
            },
        }
    }

    fn layout_with_data() -> PartitionLayout {
        let mut partitions = HashMap::new();
        partitions.insert(
            crate::partition::PartitionName::Data,
            PathBuf::from("/dev/sda8"),
        );
        PartitionLayout {
            partitions,
            device: RootDevice {
                base: PathBuf::from("/dev/sda"),
                partition_sep: "",
                root_partition: PathBuf::from("/dev/sda2"),
            },
        }
    }

    #[test]
    fn skips_when_guard_present() {
        // layout_with_data: if the guard check is bypassed, resize_if_needed
        // will attempt to spawn sgdisk/parted (not in test env) and return Err.
        let layout = layout_with_data();
        let bl: Box<dyn crate::bootloader::BootEnv> =
            Box::new(MockBootEnv::new().with_env(BootEnvKey::ResizedData, "1"));
        let mut env = BootEnvState::Available(bl);
        let mut ods = OdsStatus::new();
        let mut ctx = PreflightCtx {
            layout: &layout,
            boot_env: &mut env,
            ods_status: &mut ods,
        };
        assert!(run(&mut ctx).is_ok());
        assert!(
            ods.resize_data.is_none(),
            "guard-present path must not record a resize_data status"
        );
    }

    #[test]
    fn degraded_env_skips_guard_write() {
        // Uses empty_layout: resize_if_needed returns Ok immediately (no data
        // partition), so no real sgdisk/parted is invoked. The purpose of this
        // test is to verify the Degraded arm is dispatched correctly — not to
        // test the resize commands themselves (those are CI/Concourse-only).
        //
        // The guard-write skip behaviour for degraded mode is verified directly
        // in filesystem::resize_data::write_guard_none_does_not_call_set_env.
        let layout = empty_layout();
        let mut env = BootEnvState::Degraded(BootEnvError::CommandFailed {
            command: "boot-env-tool".into(),
            reason: "test".into(),
        });
        let mut ods = OdsStatus::new();
        let mut ctx = PreflightCtx {
            layout: &layout,
            boot_env: &mut env,
            ods_status: &mut ods,
        };
        assert!(run(&mut ctx).is_ok());
    }

    #[test]
    fn degraded_env_with_data_layout_absorbs_tool_error() {
        // Uses layout_with_data() + BootEnvState::Degraded. resize_if_needed
        // is called with None boot_env (degraded arm). With a real device path
        // (/dev/sda8) the resize commands would fail — plan-b absorbs the
        // error and returns Ok, recording the indicator in ods_status.
        let layout = layout_with_data();
        let mut env = BootEnvState::Degraded(BootEnvError::CommandFailed {
            command: "boot-env-tool".into(),
            reason: "test".into(),
        });
        let mut ods = OdsStatus::new();
        let mut ctx = PreflightCtx {
            layout: &layout,
            boot_env: &mut env,
            ods_status: &mut ods,
        };
        // With a data partition present and no real tooling, resize_if_needed
        // will fail. The absorb logic turns the error into Ok + indicator.
        let result = run(&mut ctx);
        assert!(
            result.is_ok(),
            "tool errors must be absorbed; got: {result:?}"
        );
        assert!(
            ctx.ods_status.resize_data.is_some(),
            "ods_status.resize_data must be set after a tool error"
        );
    }

    #[test]
    fn fsck_requires_reboot_propagates_through_handle_result() {
        use crate::error::{FilesystemError, InitramfsError, ResizeDataError};
        use crate::filesystem::FsckExitCode;
        // Calls handle_result() directly so the test covers the run()-level
        // decision logic without needing a real disk.
        let err = InitramfsError::ResizeData(ResizeDataError::Filesystem(
            FilesystemError::FsckRequiresReboot {
                device: std::path::PathBuf::from("/dev/sda5"),
                code: FsckExitCode::REBOOT_REQUIRED,
                output: String::new(),
            },
        ));
        let mut ods = OdsStatus::new();
        let result = handle_result(Err(err), &mut ods);
        assert!(result.is_err(), "FsckRequiresReboot must propagate as Err");
        assert!(
            ods.resize_data.is_none(),
            "FsckRequiresReboot must not record a resize_data indicator"
        );
    }

    #[test]
    fn invalid_layout_error_maps_to_invalid_layout_outcome() {
        use crate::error::{InitramfsError, ResizeDataError};
        let e = InitramfsError::ResizeData(ResizeDataError::InvalidDevicePath(
            std::path::PathBuf::from("/dev/sda"),
        ));
        assert_eq!(outcome_for(&e), ResizeOutcome::InvalidLayout);
    }

    #[test]
    fn fsck_failed_maps_to_skipped_fsck_outcome() {
        use crate::error::{FilesystemError, InitramfsError, ResizeDataError};
        use crate::filesystem::FsckExitCode;
        let e =
            InitramfsError::ResizeData(ResizeDataError::Filesystem(FilesystemError::FsckFailed {
                device: std::path::PathBuf::from("/dev/sda5"),
                code: FsckExitCode::ERRORS_UNCORRECTED,
                output: "some errors".into(),
            }));
        assert_eq!(outcome_for(&e), ResizeOutcome::SkippedFsck);
    }

    #[test]
    fn command_failed_maps_to_tool_error_outcome() {
        use crate::error::{InitramfsError, ResizeDataError};
        let e = InitramfsError::ResizeData(ResizeDataError::CommandFailed {
            command: "resize2fs".into(),
            code: 1,
            output: "failed".into(),
        });
        assert_eq!(outcome_for(&e), ResizeOutcome::ToolError);
    }

    #[test]
    fn io_error_maps_to_tool_error_outcome() {
        use crate::error::{InitramfsError, ResizeDataError};
        let e = InitramfsError::ResizeData(ResizeDataError::Io(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied,
        )));
        assert_eq!(outcome_for(&e), ResizeOutcome::ToolError);
    }
}
