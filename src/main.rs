//! omnect-os-init - Rust-based init process for omnect-os initramfs
//!
//! This binary replaces the bash-based initramfs scripts with a type-safe
//! Rust implementation.

use std::process;
use std::thread;
use std::time::Duration;

use log::{error, warn};

use omnect_os_init::{
    error::{FilesystemError, InitramfsError},
    logging::{KmsgLogger, log_fatal},
    mount_essential_filesystems,
};

/// Sleep duration for fatal error loop (seconds)
const FATAL_ERROR_SLEEP_SECS: u64 = 60;
const BASH_CMD: &str = "/bin/bash";
const SH_CMD: &str = "/bin/sh";

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
    if let Err(e) = omnect_os_init::run_init() {
        error!("Initramfs failed: {e}");
        handle_fatal_error(e, is_release_image);
    }
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
