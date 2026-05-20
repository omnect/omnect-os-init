//! omnect-os-init library
//!
//! This library provides the core functionality for the omnect-os init process.
//! It replaces the bash-based initramfs scripts with a type-safe Rust implementation.

use std::path::Path;

use log::{info, warn};

use crate::{
    config::Config,
    filesystem::{mount_core_partitions, persist_fsck_results},
    mode::{BootContext, BootMode},
    partition::{PartitionLayout, create_omnect_symlinks, detect_root_device},
};

pub mod bootloader;
pub mod config;
pub mod early_init;
pub mod error;
pub mod filesystem;
pub mod logging;
pub mod mode;
pub mod partition;
pub mod preflight;
pub mod runtime;

// Re-export main types for convenience
#[cfg(any(test, feature = "test-utils"))]
pub use crate::bootloader::MockBootloader;
pub use crate::bootloader::{
    Bootloader, BootloaderDecision, BootloaderEnv, classify_bootloader, open_bootloader_env,
};
pub use crate::early_init::mount_essential_filesystems;
pub use crate::error::{InitramfsError, Result};
pub use crate::logging::KmsgLogger;
pub use crate::runtime::OdsStatus;

/// Mount point for the real rootfs inside the initramfs.
const ROOTFS_DIR: &str = "/rootfs";

pub fn run_init() -> Result<()> {
    info!("omnect-os-initramfs starting");

    let config = Config::load()?;
    let rootfs = Path::new(ROOTFS_DIR);

    info!("Detecting root device...");
    let root_device = detect_root_device(&config.cmdline)?;
    info!(
        "Root device: {} (partition {})",
        root_device.base.display(),
        root_device.root_partition.display()
    );

    let layout = PartitionLayout::new(root_device)?;
    create_omnect_symlinks(&layout)?;

    let mut ods_status = OdsStatus::new();

    // Mount core partitions (rootfs + boot). Capture the result rather than
    // propagating immediately: if fsck on the boot partition requires a reboot,
    // ods_status already holds the diagnostic (fsck_and_record stores it before
    // returning the error). We must persist that data to the bootloader env
    // before exiting, so we open the bootloader first and persist best-effort.
    let core_result = mount_core_partitions(&layout, rootfs, &mut ods_status);

    // Best-effort: an unavailable bootloader environment is a recoverable degraded-boot condition.
    // Promote failure to None so the rest of init proceeds rather than aborting a boot that
    // otherwise succeeds.
    // Note: if mount_core_partitions returned FsckRequiresReboot, the boot partition may not
    // be mounted (GRUB). open_bootloader_env() will then fail and fall through to None — this
    // is acceptable; we log and proceed to propagate the original error.
    let mut bootloader_opt: Option<Box<dyn Bootloader>> = match open_bootloader_env() {
        Ok(mut bl) => {
            // Persist boot fsck results immediately — before propagating core_result —
            // so diagnostics survive a FsckRequiresReboot. Clear afterwards so mode
            // handlers only persist the entries they add (factory, cert, etc, data)
            // and the same keys are not written twice on the happy path.
            persist_fsck_results(&ods_status, bl.as_mut(), rootfs);
            ods_status.fsck.clear();
            Some(bl)
        }
        Err(e) => {
            warn!("Bootloader environment unavailable: {e}; booting in degraded mode");
            None
        }
    };

    core_result?;

    {
        let ctx = preflight::PreflightCtx {
            layout: &layout,
            bootloader: bootloader_opt.as_mut(),
        };
        preflight::run(ctx)?;
    }

    let ctx = BootContext::new(&config, &layout, rootfs, bootloader_opt, ods_status);

    match BootMode::detect(ctx.bootloader.as_deref())? {
        BootMode::Normal => mode::normal::run(ctx),
    }
}
