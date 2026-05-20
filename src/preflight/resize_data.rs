//! Preflight step: data partition auto-resize
//!
//! Checks the `omnect_resized_data` guard and, if absent, expands the data
//! partition to fill available disk space via `filesystem::resize_data`.
//! Runs at most once per image lifetime — the guard prevents re-execution.

use crate::bootloader::BootloaderEnvKey;
use crate::error::Result;
use crate::preflight::PreflightCtx;

pub fn run(ctx: &mut PreflightCtx<'_, '_>) -> Result<()> {
    let Some(ref mut bl) = ctx.bootloader else {
        log::debug!("resize-data preflight: bootloader environment unavailable; skipping");
        return Ok(());
    };

    if bl.get_env(BootloaderEnvKey::ResizedData)?.is_some() {
        log::debug!("resize-data preflight: guard present; already resized");
        return Ok(());
    }

    crate::filesystem::resize_data::resize_if_needed(ctx.layout, Some(bl.as_mut()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootloader::{BootloaderEnvKey, MockBootloader};
    use crate::partition::{PartitionLayout, RootDevice};
    use crate::preflight::PreflightCtx;
    use std::collections::HashMap;

    fn empty_layout() -> PartitionLayout {
        PartitionLayout {
            partitions: HashMap::new(),
            device: RootDevice {
                base: std::path::PathBuf::from("/dev/sda"),
                partition_sep: "",
                root_partition: std::path::PathBuf::from("/dev/sda2"),
            },
        }
    }

    fn layout_with_data() -> PartitionLayout {
        let mut partitions = HashMap::new();
        partitions.insert(
            crate::partition::PartitionName::Data,
            std::path::PathBuf::from("/dev/sda8"),
        );
        PartitionLayout {
            partitions,
            device: RootDevice {
                base: std::path::PathBuf::from("/dev/sda"),
                partition_sep: "",
                root_partition: std::path::PathBuf::from("/dev/sda2"),
            },
        }
    }

    #[test]
    fn skips_when_bootloader_unavailable() {
        let layout = empty_layout();
        let mut ctx = PreflightCtx {
            layout: &layout,
            bootloader: None,
        };
        assert!(run(&mut ctx).is_ok());
    }

    #[test]
    fn skips_when_guard_present() {
        // Layout includes a Data partition: if the guard check is bypassed and
        // resize_if_needed runs, it will attempt to spawn sgdisk/parted (not present
        // in the test environment) and return Err. The assert!(is_ok()) below would
        // then fail, catching the bug.
        let layout = layout_with_data();
        let mut bl: Box<dyn crate::bootloader::Bootloader> =
            Box::new(MockBootloader::new().with_env(BootloaderEnvKey::ResizedData, "1"));
        {
            let mut ctx = PreflightCtx {
                layout: &layout,
                bootloader: Some(&mut bl),
            };
            assert!(run(&mut ctx).is_ok());
        }
        assert!(bl.get_env(BootloaderEnvKey::ResizedData).unwrap().is_some());
    }
}
