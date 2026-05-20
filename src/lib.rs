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
    // returning the error). We persist that data to the bootloader env before
    // exiting, so we open the bootloader first and persist best-effort.
    let core_result = mount_core_partitions(&layout, rootfs, &mut ods_status);

    // Best-effort: open the bootloader environment. The image type determines how
    // to proceed when it is unavailable — see classify_bootloader.
    //
    // Note: if mount_core_partitions returned FsckRequiresReboot, the boot partition
    // may not be mounted (GRUB), causing open_bootloader_env() to fail. core_result?
    // runs inside both branches below, so FsckRequiresReboot is always propagated
    // before DegradedBoot — the reboot invariant is preserved.
    let is_release = cfg!(feature = "release-image");
    let mut bootloader_env: BootloaderEnv =
        match classify_bootloader(open_bootloader_env(), is_release) {
            BootloaderDecision::Continue(mut env, degraded) => {
                if let Some(bl) = env.available_mut() {
                    persist_fsck_results(&ods_status, bl, rootfs);
                    ods_status.fsck.clear();
                }
                core_result?;
                if degraded {
                    warn!("Bootloader environment unavailable; booting in degraded mode");
                    ods_status.set_degraded_boot();
                }
                env
            }
            BootloaderDecision::Abort(err) => {
                core_result?;
                return Err(err);
            }
        };

    {
        let ctx = preflight::PreflightCtx {
            layout: &layout,
            bootloader: &mut bootloader_env,
        };
        preflight::run(ctx)?;
    }

    let ctx = BootContext::new(&config, &layout, rootfs, bootloader_env, ods_status);

    match BootMode::detect(ctx.bootloader.available())? {
        BootMode::Normal => mode::normal::run(ctx),
    }
}
