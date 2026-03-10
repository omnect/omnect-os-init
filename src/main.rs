//! omnect-os-init - Rust-based init process for omnect-os initramfs
//!
//! This binary replaces the bash-based initramfs scripts with a type-safe
//! Rust implementation.

use nix::mount::MsFlags;
use std::fs;
use std::process;
use std::thread;
use std::time::Duration;

use log::{error, info, warn};

use omnect_os_init::{
    Result,
    bootloader::create_bootloader,
    config::Config,
    error::{FilesystemError, InitramfsError},
    filesystem::{
        MountManager, OverlayConfig, check_filesystem_lenient, setup_data_overlay,
        setup_etc_overlay, setup_raw_rootfs_mount,
    },
    logging::{KmsgLogger, log_fatal},
    mount_essential_filesystems,
    partition::{PartitionLayout, create_omnect_symlinks, detect_root_device},
    runtime::{OdsStatus, create_fs_links, create_ods_runtime_files, switch_root},
};

/// Sleep duration for fatal error loop (seconds)
const FATAL_ERROR_SLEEP_SECS: u64 = 60;

fn main() {
    // Mount essential filesystems first (/dev, /proc, /sys, /run)
    if let Err(e) = mount_essential_filesystems() {
        eprintln!("FATAL: Failed to mount essential filesystems: {}", e);
        spawn_emergency_shell();
    }

    // Determine release mode from /proc/cmdline only — rootfs is not yet
    // mounted, so os-release cannot be read reliably here.
    let is_release_image = fs::read_to_string("/proc/cmdline")
        .unwrap_or_default()
        .split_whitespace()
        .any(|p| p == "omnect_release_image=1");

    // Initialize logging
    match KmsgLogger::new() {
        Ok(logger) => {
            if let Err(e) = logger.init() {
                log_fatal(&format!("Logger initialization failed: {}", e));
            }
        }
        Err(e) => {
            log_fatal(&format!("Failed to open kmsg: {}", e));
        }
    }

    // Run main initialization
    if let Err(e) = run() {
        error!("Initramfs failed: {}", e);
        handle_fatal_error(e, is_release_image);
    }
}

fn run() -> Result<()> {
    info!("omnect-os-initramfs starting");

    // Load configuration
    let mut config = Config::load()?;
    info!(
        "Configuration loaded: rootfs_dir={}, release={}",
        config.rootfs_dir.display(),
        config.is_release_image
    );

    // Initialize mount manager for tracking
    let mut mount_manager = MountManager::new();

    // Detect root device
    info!("Detecting root device...");
    let root_device = detect_root_device()?;
    info!(
        "Root device: {} (partition {})",
        root_device.base.display(),
        root_device.root_partition.display()
    );

    // Detect partition layout
    let layout = PartitionLayout::detect(root_device)?;
    info!("Partition table: {}", layout.table_type);

    // Create /dev/omnect/* symlinks
    create_omnect_symlinks(&layout)?;

    // Create bootloader abstraction
    let bootloader = create_bootloader(&config.rootfs_dir)?;
    info!("Bootloader type: {}", bootloader.bootloader_type());

    // Initialize ODS status
    let mut ods_status = OdsStatus::new();

    // Run fsck on partitions and mount them
    mount_partitions(&mut mount_manager, &layout, &config, &mut ods_status)?;

    // Now that rootfs is mounted, read os-release for feature flags.
    // Non-fatal: missing os-release means no features enabled.
    if let Err(e) = config.load_os_release() {
        log::warn!("Failed to read os-release from rootfs: {}", e);
    }
    info!("release={}", config.is_release_image);

    // Setup raw rootfs mount (before overlays)
    setup_raw_rootfs_mount(&mut mount_manager, &config.rootfs_dir)?;

    // Setup overlays
    let overlay_config = OverlayConfig::new(&config.rootfs_dir)
        .with_persistent_var_log(config.has_persistent_var_log());

    setup_etc_overlay(&mut mount_manager, &overlay_config)?;
    setup_data_overlay(&mut mount_manager, &overlay_config)?;

    // Create fs-links
    create_fs_links(&config.rootfs_dir)?;

    // Create ODS runtime files
    create_ods_runtime_files(&ods_status, bootloader.as_ref())?;

    info!("omnect-os-initramfs completed successfully");

    // Release all tracked mounts before exec. The mounts themselves must
    // survive into the new root; the RAII destructor must not unmount them.
    mount_manager.release();

    // Switch root to final rootfs
    switch_root(&config.rootfs_dir, None)?;

    // This should never be reached
    Ok(())
}

/// Mount all required partitions
fn mount_partitions(
    mm: &mut MountManager,
    layout: &PartitionLayout,
    config: &Config,
    ods_status: &mut OdsStatus,
) -> Result<()> {
    let rootfs = &config.rootfs_dir;

    // Mount rootfs read-only
    if let Some(root_dev) = layout.partitions.get("rootCurrent") {
        // Run fsck first; FsckRequiresReboot propagates via ? and triggers a reboot
        let result = check_filesystem_lenient(root_dev)?;
        ods_status.add_fsck_result("root", result.exit_code, result.output);

        mm.mount_readonly(root_dev, rootfs, "ext4")?;
        info!("Mounted rootfs at {}", rootfs.display());
    }

    // Mount boot partition
    if let Some(boot_dev) = layout.partitions.get("boot") {
        let boot_mount = rootfs.join("boot");

        let result = check_filesystem_lenient(boot_dev)?;
        ods_status.add_fsck_result("boot", result.exit_code, result.output);

        mm.mount_readwrite(boot_dev, &boot_mount, "vfat")?;
    }

    // Mount factory partition
    if let Some(factory_dev) = layout.partitions.get("factory") {
        let factory_mount = rootfs.join("mnt/factory");

        let result = check_filesystem_lenient(factory_dev)?;
        ods_status.add_fsck_result("factory", result.exit_code, result.output);

        mm.mount_readonly(factory_dev, &factory_mount, "ext4")?;
    }

    // Mount cert partition
    if let Some(cert_dev) = layout.partitions.get("cert") {
        let cert_mount = rootfs.join("mnt/cert");

        let result = check_filesystem_lenient(cert_dev)?;
        ods_status.add_fsck_result("cert", result.exit_code, result.output);

        mm.mount_readonly(cert_dev, &cert_mount, "ext4")?;
    }

    // Mount etc partition (for overlay upper)
    if let Some(etc_dev) = layout.partitions.get("etc") {
        let etc_mount = rootfs.join("mnt/etc");

        let result = check_filesystem_lenient(etc_dev)?;
        ods_status.add_fsck_result("etc", result.exit_code, result.output);

        mm.mount_readwrite(etc_dev, &etc_mount, "ext4")?;
    }

    // Mount data partition
    if let Some(data_dev) = layout.partitions.get("data") {
        let data_mount = rootfs.join("mnt/data");

        let result = check_filesystem_lenient(data_dev)?;
        ods_status.add_fsck_result("data", result.exit_code, result.output);

        mm.mount_readwrite(data_dev, &data_mount, "ext4")?;
    }

    // /var/volatile provides a writable mount for volatile data under the read-only rootfs
    let var_volatile = rootfs.join("var/volatile");
    mm.mount_tmpfs(&var_volatile, MsFlags::empty(), None)?;

    // /run is NOT mounted here: the initramfs /run tmpfs (mounted by
    // mount_essential_filesystems) is moved into the new root by switch_root
    // using MS_MOVE. Mounting a second tmpfs at /rootfs/run would cause EBUSY
    // and lose any files written there (e.g. ODS runtime state).

    Ok(())
}

/// Handle fatal errors based on image type
fn handle_fatal_error(error: InitramfsError, is_release: bool) -> ! {
    // fsck exit code 2 means the filesystem was repaired but a clean reboot
    // is required before the OS can safely use it.
    if matches!(
        error,
        InitramfsError::Filesystem(FilesystemError::FsckRequiresReboot { .. })
    ) {
        error!("fsck requires reboot: {}", error);
        let _ = nix::sys::reboot::reboot(nix::sys::reboot::RebootMode::RB_AUTOBOOT);
        // reboot(2) should not return; loop as a last resort
        loop {
            thread::sleep(Duration::from_secs(FATAL_ERROR_SLEEP_SECS));
        }
    }

    if is_release {
        // Release image: loop forever to prevent reboot loops
        loop {
            error!("FATAL: {}", error);
            thread::sleep(Duration::from_secs(FATAL_ERROR_SLEEP_SECS));
        }
    } else {
        // Debug image: spawn shell
        warn!("Debug mode: spawning shell due to error: {}", error);
        spawn_debug_shell();
    }
}

/// Spawn emergency shell (before logging available)
fn spawn_emergency_shell() -> ! {
    let _ = process::Command::new("/bin/sh").status();
    loop {
        thread::sleep(Duration::from_secs(FATAL_ERROR_SLEEP_SECS));
    }
}

/// Spawn debug shell for debugging
fn spawn_debug_shell() -> ! {
    let status = process::Command::new("/bin/bash")
        .arg("--init-file")
        .arg("/dev/null")
        .status();

    match status {
        Ok(s) => process::exit(s.code().unwrap_or(1)),
        Err(_) => {
            let _ = process::Command::new("/bin/sh").status();
            process::exit(1);
        }
    }
}
