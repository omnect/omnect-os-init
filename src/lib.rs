//! omnect-os-init library
//!
//! This library provides the core functionality for the omnect-os init process.
//! It replaces the bash-based initramfs scripts with a type-safe Rust implementation.

use std::path::Path;

use log::{info, warn};

use crate::{
    config::Config,
    filesystem::mount_core_partitions,
    mode::{BootContext, BootMode},
    partition::{PartitionLayout, create_omnect_symlinks, detect_root_device},
    runtime::OdsStatus,
};

pub mod bootloader;
pub mod config;
pub mod early_init;
pub mod error;
pub mod filesystem;
pub mod logging;
pub mod mode;
pub mod partition;
pub mod runtime;

// Re-export main types for convenience
pub use crate::bootloader::{Bootloader, create_bootloader};
pub use crate::early_init::mount_essential_filesystems;
pub use crate::error::{InitramfsError, Result};
pub use crate::logging::KmsgLogger;

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

    // Mount core partitions (rootfs + boot); boot must be mounted before create_bootloader()
    // (GRUB reads grubenv from rootfs/boot/EFI/BOOT/grubenv).
    mount_core_partitions(&layout, rootfs, &mut ods_status)?;

    // Best-effort: a corrupted grubenv is a recoverable degraded-boot condition.
    // Promote failure to None so the rest of init proceeds; ODS bootloader-dependent
    // state is skipped rather than aborting a boot that otherwise succeeds.
    let bootloader_opt: Option<Box<dyn Bootloader>> = match create_bootloader() {
        Ok(bl) => Some(bl),
        Err(e) => {
            warn!("Bootloader unavailable: {e}; ODS update-validation will be skipped");
            None
        }
    };

    let mode = BootMode::detect(bootloader_opt.as_deref())?;

    let ctx = BootContext::new(&config, &layout, rootfs, bootloader_opt, ods_status);

    #[allow(clippy::single_match)]
    match mode {
        BootMode::Normal => mode::normal::run(ctx),
    }
}
