//! omnect-os-init - Rust-based init process for omnect-os initramfs
//!
//! This binary replaces the bash-based initramfs scripts with a type-safe
//! Rust implementation.

use std::process;
use std::thread;
use std::time::Duration;

use log::{error, info, warn};

use omnect_os_init::{
    error::InitramfsError,
    logging::{KmsgLogger, log_fatal},
    mount_essential_filesystems,
    recovery::{Action, decide},
};

/// Sleep duration for fatal error loop (seconds)
const FATAL_ERROR_SLEEP_SECS: u64 = 60;
const BASH_CMD: &str = "/bin/bash";
const SH_CMD: &str = "/bin/sh";

fn main() {
    // Compile-time image-type discriminator. Read first, before any fallible
    // step, so even the earliest failures (mount_essential_filesystems, logger
    // init) respect the release/debug split. Spec invariant 1 (§2.6): release
    // never shells.
    let is_release_image = cfg!(feature = "release-image");

    // Mount essential filesystems first (/dev, /proc, /sys, /run). On
    // failure: release halts; debug spawns the emergency shell.
    if let Err(e) = mount_essential_filesystems() {
        if is_release_image {
            // /dev is not mounted yet — log_fatal/kmsg is unavailable. Use an
            // eprintln! loop so the message keeps reaching the console each cycle.
            loop {
                eprintln!("FATAL: Failed to mount essential filesystems: {e}");
                thread::sleep(Duration::from_secs(FATAL_ERROR_SLEEP_SECS));
            }
        } else {
            eprintln!("FATAL: Failed to mount essential filesystems: {e}");
            spawn_emergency_shell();
        }
    }

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

/// Handle a fatal error per the recovery policy.
///
/// Spec: docs/superpowers/specs/2026-05-27-boot-failure-recovery-policy-design.md §2-§3
fn handle_fatal_error(error: InitramfsError, is_release: bool) -> ! {
    let class = error.recovery_class();
    let update_pending = omnect_os_init::read_update_pending();
    let action = decide(class, is_release, update_pending);

    log_fatal(&format!(
        "fatal error (class={class:?}, update_pending={update_pending}, action={action:?}): {error}"
    ));

    match action {
        Action::Reboot => {
            // nix reboot() returns Result<Infallible>; Ok is uninhabited, so
            // the let-binding is irrefutable — the only reachable path is Err.
            let Err(e) = nix::sys::reboot::reboot(nix::sys::reboot::RebootMode::RB_AUTOBOOT);
            log_fatal(&format!("reboot(2) failed: {e}; halting"));
            // reboot(2) should not return on success; if it does, fall back to halt.
            halt_with_message(&format!("reboot(2) returned unexpectedly after {action:?}"));
        }
        Action::Halt => {
            halt_with_message(&format!("FATAL: {error}"));
        }
        Action::Shell => {
            spawn_debug_shell();
        }
        Action::Continue => {
            // Defensive: ContinueDegraded errors should be absorbed by the
            // caller and never reach the fatal path. If we get here, something
            // failed to suppress the error — treat as Fatal so the device
            // doesn't fall off the policy.
            log_fatal(&format!(
                "BUG: ContinueDegraded reached handle_fatal_error for: {error}"
            ));
            if is_release {
                halt_with_message(&format!("FATAL (defensive): {error}"));
            } else {
                spawn_debug_shell();
            }
        }
    }
}

/// Halt forever with a fixed message written to /dev/kmsg each cycle.
///
/// Used by release images on any Fatal error path. Writes directly to
/// /dev/kmsg via `log_fatal` rather than through the `log` facade, so a
/// failure to initialize the global logger does not silence the message.
/// If /dev/kmsg itself cannot be opened, `log_fatal` is a no-op.
fn halt_with_message(message: &str) -> ! {
    loop {
        log_fatal(message);
        thread::sleep(Duration::from_secs(FATAL_ERROR_SLEEP_SECS));
    }
}

/// Emergency shell invoked before the kmsg logger is initialized.
///
/// `log::*` macros are not yet usable here — only `eprintln!` is safe.
/// Respawns on exit so PID 1 never returns to the kernel.
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

/// Debug shell invoked after a fatal error on a non-release image.
///
/// Respawns on exit (PID 1 must never return to the kernel) and falls
/// back to `sh` when `bash` is unavailable.
fn spawn_debug_shell() -> ! {
    // PID 1 must never exit — the kernel would panic. Respawn the shell
    // in a loop so the operator can re-enter after an accidental exit.
    loop {
        let status = process::Command::new(BASH_CMD)
            .arg("--init-file")
            .arg("/dev/null")
            .status();

        match status {
            Ok(_) => info!("debug shell exited — respawning"),
            Err(e) => {
                warn!("bash unavailable ({e}), falling back to sh");
                match process::Command::new(SH_CMD).status() {
                    Ok(_) => info!("sh exited — respawning"),
                    Err(e) => {
                        error!("sh also unavailable ({e}) — sleeping before retry");
                        thread::sleep(Duration::from_secs(FATAL_ERROR_SLEEP_SECS));
                    }
                }
            }
        }
    }
}
