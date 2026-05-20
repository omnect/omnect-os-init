//! Preflight step: data partition auto-resize
//!
//! On a live bootloader: checks the `omnect_resized_data` guard and, if
//! absent, expands the data partition via `filesystem::resize_data`.
//!
//! On a degraded boot (bootloader unavailable): runs resize without the
//! guard. Only reached on release-images; debug-images abort in lib.rs
//! before preflight executes.

use crate::bootloader::BootloaderEnvKey;
use crate::error::Result;
use crate::preflight::PreflightCtx;

pub fn run(ctx: &mut PreflightCtx<'_, '_>) -> Result<()> {
    match ctx.bootloader.available_mut() {
        Some(bl) => {
            if bl.get_env(BootloaderEnvKey::ResizedData)?.is_some() {
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
    use crate::bootloader::{BootloaderEnv, BootloaderEnvKey, MockBootloader};
    use crate::error::BootloaderError;
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
        let bl: Box<dyn crate::bootloader::Bootloader> =
            Box::new(MockBootloader::new().with_env(BootloaderEnvKey::ResizedData, "1"));
        let mut env = BootloaderEnv::Available(bl);
        let mut ctx = PreflightCtx {
            layout: &layout,
            bootloader: &mut env,
        };
        assert!(run(&mut ctx).is_ok());
    }

    #[test]
    fn degraded_env_with_empty_layout_returns_ok() {
        // Data partition absent in layout → resize_if_needed returns Ok immediately.
        // Verifies the Degraded arm is reached and does not panic.
        let layout = empty_layout();
        let mut env = BootloaderEnv::Degraded(BootloaderError::CommandFailed {
            command: "grub-editenv".into(),
            reason: "test".into(),
        });
        let mut ctx = PreflightCtx {
            layout: &layout,
            bootloader: &mut env,
        };
        assert!(run(&mut ctx).is_ok());
    }
}
