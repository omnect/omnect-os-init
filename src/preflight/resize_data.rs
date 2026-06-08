//! Preflight step: data partition auto-resize
//!
//! With boot env available: checks the `omnect_resized_data` guard and, if
//! absent, expands the data partition via `filesystem::resize_data`.
//!
//! On a degraded boot (boot env unavailable): runs resize without the
//! guard. Only reached on release-images; debug-images abort in lib.rs
//! before preflight executes.

use crate::bootloader::BootEnvKey;
use crate::error::Result;
use crate::preflight::PreflightCtx;

pub fn run(ctx: &mut PreflightCtx<'_, '_>) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootloader::{BootEnvKey, BootEnvState, MockBootEnv};
    use crate::error::BootEnvError;
    use crate::partition::{PartitionLayout, RootDevice};
    use crate::preflight::PreflightCtx;
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
        let mut ctx = PreflightCtx {
            layout: &layout,
            boot_env: &mut env,
        };
        assert!(run(&mut ctx).is_ok());
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
            command: "grub-editenv".into(),
            reason: "test".into(),
        });
        let mut ctx = PreflightCtx {
            layout: &layout,
            boot_env: &mut env,
        };
        assert!(run(&mut ctx).is_ok());
    }

    #[test]
    fn degraded_env_with_data_layout_guard_not_written() {
        // Uses layout_with_data() + BootEnvState::Degraded. resize_if_needed
        // is called with None boot_env (degraded arm). With a real device path
        // (/dev/sda8) the resize commands would fail — so this test asserts the
        // error is a command failure (ResizeDataError), NOT a boot env error,
        // which proves the guard-write code path was not reached.
        let layout = layout_with_data();
        let mut env = BootEnvState::Degraded(BootEnvError::CommandFailed {
            command: "grub-editenv".into(),
            reason: "test".into(),
        });
        let mut ctx = PreflightCtx {
            layout: &layout,
            boot_env: &mut env,
        };
        let result = run(&mut ctx);
        // The resize commands fail (no real /dev/sda8) but the error must NOT
        // be a boot env error — confirming the guard-write path was bypassed.
        match result {
            Ok(()) => {} // unlikely in test env, but acceptable
            Err(crate::error::InitramfsError::ResizeData(_)) => {} // expected
            Err(crate::error::InitramfsError::Filesystem(_)) => {} // fsck path
            Err(e) => panic!("unexpected error type — guard-write was reached: {e}"),
        }
    }
}
