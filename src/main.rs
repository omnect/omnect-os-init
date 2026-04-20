//! omnect-os-init - Rust-based init process for omnect-os initramfs
//!
//! This binary replaces the bash-based initramfs scripts with a type-safe
//! Rust implementation.

use std::path::Path;
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
        mount_partitions, persist_fsck_results, setup_data_overlay, setup_etc_overlay,
        setup_raw_rootfs_mount,
    },
    logging::{KmsgLogger, log_fatal},
    mount_essential_filesystems,
    partition::{PartitionLayout, create_omnect_symlinks, detect_root_device},
    runtime::{ODS_RUNTIME_DIR, OdsStatus, create_fs_links, create_ods_runtime_files, switch_root},
};

/// Sleep duration for fatal error loop (seconds)
const FATAL_ERROR_SLEEP_SECS: u64 = 60;
const BASH_CMD: &str = "/bin/bash";
const SH_CMD: &str = "/bin/sh";
/// Mount point for the real rootfs inside the initramfs.
const ROOTFS_DIR: &str = "/rootfs";

fn main() {
    // Mount essential filesystems first (/dev, /proc, /sys, /run)
    if let Err(e) = mount_essential_filesystems() {
        eprintln!("FATAL: Failed to mount essential filesystems: {}", e);
        spawn_emergency_shell();
    }

    // Release vs. debug mode is a build-time property via the `release-image` feature.
    let is_release_image = cfg!(feature = "release-image");

    // Initialize logging — fatal if /dev/kmsg cannot be opened or logger already set.
    // log_fatal() opens /dev/kmsg directly so the message reaches the kernel ring buffer
    // even before the global logger is registered.
    let logger_result = KmsgLogger::new()
        .map_err(|e| InitramfsError::Io(std::io::Error::other(format!("Failed to open kmsg: {e}"))))
        .and_then(|logger| {
            logger.init().map_err(|e| {
                InitramfsError::Io(std::io::Error::other(format!(
                    "Logger initialization failed: {e}"
                )))
            })
        });
    if let Err(ref e) = logger_result {
        log_fatal(&format!("{e}"));
        handle_fatal_error(logger_result.unwrap_err(), is_release_image);
    }

    // Run main initialization
    if let Err(e) = run() {
        error!("Initramfs failed: {}", e);
        handle_fatal_error(e, is_release_image);
    }
}

fn run() -> Result<()> {
    info!("omnect-os-initramfs starting");

    let config = Config::load()?;
    let rootfs = Path::new(ROOTFS_DIR);

    // Detect root device
    info!("Detecting root device...");
    let root_device = detect_root_device(&config.cmdline)?;
    info!(
        "Root device: {} (partition {})",
        root_device.base.display(),
        root_device.root_partition.display()
    );

    // Detect partition layout
    let layout = PartitionLayout::new(root_device)?;

    // Create /dev/omnect/* symlinks
    create_omnect_symlinks(&layout)?;

    // Initialize ODS status
    let mut ods_status = OdsStatus::new();

    // Run fsck on partitions and mount them.
    // Boot partition must be mounted before create_bootloader() so that
    // GrubBootloader can access the grubenv file at rootfs/boot/EFI/BOOT/grubenv.
    let mount_result = mount_partitions(&layout, rootfs, &mut ods_status);

    // Attempt to create bootloader and persist fsck results before propagating any
    // mount error. This ensures results are stored even on the FsckRequiresReboot
    // reboot path. For GRUB: requires boot partition mounted; best-effort if it isn't.
    let mut bootloader_result = create_bootloader();
    if let Ok(ref mut bl) = bootloader_result {
        // Persist fsck results: gzip+base64 encoded output (code + full text) to
        // bootloader env, and full output to data partition log.
        // Non-fatal: failures are logged as warnings.
        persist_fsck_results(&ods_status, bl.as_mut(), rootfs);
    } else {
        warn!("Could not create bootloader; fsck results will not be persisted to bootloader env");
    }

    // Propagate mount failure after persistence attempt (FsckRequiresReboot → reboot)
    mount_result?;

    // Bootloader is expected to be available after a successful mount, but can
    // fail in edge cases (e.g. missing grubenv on a corrupted boot partition).
    // Log a warning and continue — ODS bootloader-dependent state will be skipped
    // rather than aborting a boot that otherwise succeeded.
    let bootloader = match bootloader_result {
        Ok(bl) => Some(bl),
        Err(e) => {
            warn!(
                "Bootloader unavailable after mount: {}; ODS update-validation will be skipped",
                e
            );
            None
        }
    };

    // Setup raw rootfs mount (before overlays)
    setup_raw_rootfs_mount(rootfs)?;

    // Setup overlays
    setup_etc_overlay(rootfs)?;
    setup_data_overlay(rootfs, &config.overlay)?;

    // Create fs-links
    create_fs_links(rootfs)?;

    // Create ODS runtime files
    create_ods_runtime_files(
        &ods_status,
        bootloader.as_deref(),
        rootfs,
        Path::new(ODS_RUNTIME_DIR),
    )?;

    info!("omnect-os-initramfs completed successfully");

    // Switch root to final rootfs
    switch_root(rootfs)?;

    // This should never be reached
    Ok(())
}

/// Handle fatal errors based on image type
fn handle_fatal_error(error: InitramfsError, is_release: bool) -> ! {
    // fsck exit code 2 means fsck explicitly requests a reboot before mounting.
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
    // PID 1 must never exit. Respawn the shell so the operator can retry.
    // Use eprintln! — the kmsg logger may not be initialised yet at this point.
    loop {
        match process::Command::new(SH_CMD).status() {
            Ok(status) => eprintln!("Emergency shell exited with {status} — respawning"),
            Err(e) => {
                eprintln!(
                    "Failed to spawn emergency shell ({e}) — retrying in {FATAL_ERROR_SLEEP_SECS}s"
                );
                thread::sleep(Duration::from_secs(FATAL_ERROR_SLEEP_SECS));
            }
        }
    }
}

/// Spawn debug shell for debugging
fn spawn_debug_shell() -> ! {
    // PID 1 must never exit — the kernel would panic. Respawn the shell
    // in a loop so the operator can re-enter after an accidental exit.
    loop {
        let status = process::Command::new(BASH_CMD)
            .arg("--init-file")
            .arg("/dev/null")
            .status();

        match status {
            Ok(_) => log::info!("debug shell exited — respawning"),
            Err(e) => {
                log::warn!("bash unavailable ({e}), falling back to sh");
                match process::Command::new(SH_CMD).status() {
                    Ok(_) => log::info!("sh exited — respawning"),
                    Err(e) => {
                        log::error!("sh also unavailable ({e}) — sleeping before retry");
                        thread::sleep(Duration::from_secs(FATAL_ERROR_SLEEP_SECS));
                    }
                }
            }
        }
    }
}
