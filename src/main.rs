//! omnect-os-init - Rust-based init process for omnect-os initramfs
//!
//! This binary replaces the bash-based initramfs scripts with a type-safe
//! Rust implementation that handles:
//! - Root device detection and partition symlinks
//! - Filesystem mounting with overlayfs
//! - Factory reset
//! - Flash modes
//! - omnect-device-service integration

use omnect_os_init::{
    Config, InitramfsError, KmsgLogger, Result, bootloader::create_bootloader, logging::log_fatal,
    mount_essential_filesystems,
};

use log::{error, info, warn};
use std::process;

fn main() {
    // Mount essential filesystems first (/dev, /proc, /sys)
    // This must happen before anything else, including logging
    if let Err(e) = mount_essential_filesystems() {
        // We can't log yet, so try to write to console
        eprintln!("FATAL: Failed to mount essential filesystems: {}", e);
        // Try emergency shell if available
        let _ = process::Command::new("/bin/sh").status();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }

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

    if let Err(e) = run() {
        error!("Initramfs failed: {}", e);
        handle_fatal_error(e);
    }
}

fn run() -> Result<()> {
    info!("omnect-os-initramfs starting");

    // Load configuration from kernel cmdline and environment
    let config = Config::load()?;
    info!(
        "Configuration loaded: rootfs_dir={}",
        config.rootfs_dir.display()
    );

    // Create bootloader abstraction
    let bootloader = create_bootloader(&config.rootfs_dir)?;
    info!("Bootloader type: {:?}", bootloader.bootloader_type());

    // TODO: Phase 2 - Device detection and partition symlinks
    // TODO: Phase 3 - Filesystem mounting
    // TODO: Phase 4 - Overlayfs setup
    // TODO: Phase 5 - ODS integration and switch_root

    info!("omnect-os-initramfs completed successfully");
    Ok(())
}

/// Handle fatal errors based on image type (debug vs release)
fn handle_fatal_error(error: InitramfsError) -> ! {
    let is_release = std::fs::read_to_string("/etc/os-release")
        .map(|content| content.contains("OMNECT_RELEASE_IMAGE=\"1\""))
        .unwrap_or(false);

    if is_release {
        // Release image: loop forever to prevent reboot loops
        loop {
            error!("FATAL: {}", error);
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    } else {
        // Debug image: spawn a shell for debugging
        warn!("Debug mode: spawning shell due to error: {}", error);

        // Try to spawn bash for debugging
        let status = process::Command::new("/bin/bash")
            .arg("--init-file")
            .arg("/dev/null")
            .status();

        match status {
            Ok(s) => process::exit(s.code().unwrap_or(1)),
            Err(_) => {
                // If bash fails, try sh
                let _ = process::Command::new("/bin/sh").status();
                process::exit(1);
            }
        }
    }
}
