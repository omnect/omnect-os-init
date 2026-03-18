//! omnect-os-init - Rust-based init process for omnect-os initramfs
//!
//! This binary replaces the bash-based initramfs scripts with a type-safe
//! Rust implementation.

use nix::mount::MsFlags;
use std::fs;
use std::path::Path;
use std::process;
use std::thread;
use std::time::Duration;

use log::{error, info, warn};

use omnect_os_init::{
    Result,
    bootloader::Bootloader,
    bootloader::create_bootloader,
    config::Config,
    error::{FilesystemError, InitramfsError, PartitionError},
    filesystem::{
        MountManager, OverlayConfig, check_filesystem_lenient, is_path_mounted, setup_data_overlay,
        setup_etc_overlay, setup_raw_rootfs_mount,
    },
    logging::{KmsgLogger, log_fatal},
    mount_essential_filesystems,
    partition::{PartitionLayout, create_omnect_symlinks, detect_root_device, partition_names},
    runtime::{OdsStatus, create_fs_links, create_ods_runtime_files, switch_root},
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

    // Determine release mode from /proc/cmdline — rootfs is not yet mounted
    // so os-release cannot be read here. This value is intentionally kept
    // separate from config.is_release_image (updated inside run()): if run()
    // fails at any point, this cmdline-derived value is the only safe fallback
    // for handle_fatal_error, which must decide debug vs. release behavior
    // before the rootfs is available.
    let is_release_image = match fs::read_to_string("/proc/cmdline") {
        Ok(s) => s.split_whitespace().any(|p| p == "omnect_release_image=1"),
        Err(e) => {
            eprintln!("Warning: failed to read /proc/cmdline: {e}; defaulting to debug mode");
            false
        }
    };

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

    // Initialize ODS status
    let mut ods_status = OdsStatus::new();

    // Run fsck on partitions and mount them.
    // Boot partition must be mounted before create_bootloader() so that
    // GrubBootloader can access the grubenv file at rootfs/boot/EFI/BOOT/grubenv.
    let mount_result = mount_partitions(&mut mount_manager, &layout, &config, &mut ods_status);

    // Attempt to create bootloader and persist fsck results before propagating any
    // mount error. This ensures results are stored even on the FsckRequiresReboot
    // reboot path. For GRUB: requires boot partition mounted; best-effort if it isn't.
    let mut bootloader_result = create_bootloader(&config.rootfs_dir);
    if let Ok(ref mut bl) = bootloader_result {
        info!("Bootloader type: {}", bl.bootloader_type());
        // Persist fsck results: gzip+base64 encoded output (code + full text) to
        // bootloader env, and full output to data partition log.
        // Non-fatal: failures are logged as warnings.
        persist_fsck_results(&ods_status, bl.as_mut(), &config.rootfs_dir);
    } else {
        warn!("Could not create bootloader; fsck results will not be persisted to bootloader env");
    }

    // Propagate mount failure after persistence attempt (FsckRequiresReboot → reboot)
    mount_result?;

    // Safe: mount succeeded means boot partition is mounted, so bootloader was created above.
    let bootloader = bootloader_result?;

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

/// Run fsck on a partition and record the result (including output) in ods_status.
///
/// Intercepts `FsckRequiresReboot` to save the output before propagating, ensuring
/// it is available for persistence even when mounting is aborted early.
fn fsck_and_record(
    dev: &Path,
    name: &str,
    ods_status: &mut OdsStatus,
) -> std::result::Result<(), FilesystemError> {
    match check_filesystem_lenient(dev) {
        Ok(r) => {
            ods_status.add_fsck_result(name, r.exit_code, r.output);
            Ok(())
        }
        Err(FilesystemError::FsckRequiresReboot {
            device,
            code,
            output,
        }) => {
            ods_status.add_fsck_result(name, code, output.clone());
            Err(FilesystemError::FsckRequiresReboot {
                device,
                code,
                output,
            })
        }
        Err(e) => Err(e),
    }
}

/// Mount all required partitions
fn mount_partitions(
    mm: &mut MountManager,
    layout: &PartitionLayout,
    config: &Config,
    ods_status: &mut OdsStatus,
) -> Result<()> {
    let rootfs = &config.rootfs_dir;

    // Mount rootfs read-only — rootCurrent is mandatory; abort if missing.
    let root_dev = layout
        .partitions
        .get(partition_names::ROOT_CURRENT)
        .ok_or_else(|| {
            InitramfsError::Partition(PartitionError::DeviceDetection(
                "rootCurrent not found in partition map; cannot mount rootfs".to_string(),
            ))
        })?;
    fsck_and_record(root_dev, partition_names::ROOT_CURRENT, ods_status)?;
    mm.mount_readonly(root_dev, rootfs, "ext4")?;
    info!("Mounted rootfs at {}", rootfs.display());

    // Mount boot partition
    if let Some(boot_dev) = layout.partitions.get(partition_names::BOOT) {
        let boot_mount = rootfs.join("boot");
        fsck_and_record(boot_dev, partition_names::BOOT, ods_status)?;
        mm.mount_readwrite(boot_dev, &boot_mount, "vfat")?;
    }

    // Mount factory partition
    if let Some(factory_dev) = layout.partitions.get(partition_names::FACTORY) {
        let factory_mount = rootfs.join("mnt/factory");
        fsck_and_record(factory_dev, partition_names::FACTORY, ods_status)?;
        mm.mount_readonly(factory_dev, &factory_mount, "ext4")?;
    }

    // Mount cert partition
    if let Some(cert_dev) = layout.partitions.get(partition_names::CERT) {
        let cert_mount = rootfs.join("mnt/cert");
        fsck_and_record(cert_dev, partition_names::CERT, ods_status)?;
        mm.mount_readonly(cert_dev, &cert_mount, "ext4")?;
    }

    // Mount etc partition (for overlay upper)
    if let Some(etc_dev) = layout.partitions.get(partition_names::ETC) {
        let etc_mount = rootfs.join("mnt/etc");
        fsck_and_record(etc_dev, partition_names::ETC, ods_status)?;
        mm.mount_readwrite(etc_dev, &etc_mount, "ext4")?;
    }

    // Mount data partition
    if let Some(data_dev) = layout.partitions.get(partition_names::DATA) {
        let data_mount = rootfs.join("mnt/data");
        fsck_and_record(data_dev, partition_names::DATA, ods_status)?;
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

/// Persist fsck results after all partitions are mounted.
///
/// For each partition with a non-zero fsck exit code:
/// - Stores the gzip+base64 encoded exit code and full output in the bootloader
///   environment (grubenv / uboot-env) for inspection after the next boot.
/// - Writes the full output to `/data/var/log/fsck/<partition>.log` on the data
///   partition so ODS and operators can inspect it after boot.
fn persist_fsck_results(
    ods_status: &OdsStatus,
    bootloader: &mut dyn Bootloader,
    rootfs_dir: &Path,
) {
    // Bootloader save (grubenv or env file) is the primary persistence mechanism
    // and works as long as the boot partition is mounted — which is true even on
    // the FsckRequiresReboot path (boot is mounted before fsck runs).
    //
    // The data partition log is best-effort: it is only mounted when
    // mount_partitions() succeeds fully, so it may not be available here.
    let log_dir = rootfs_dir.join("mnt/data/var/log/fsck");
    let data_mounted = is_path_mounted(&rootfs_dir.join("mnt/data")).unwrap_or(false);

    for (partition, fsck) in &ods_status.fsck {
        if fsck.code == 0 {
            continue;
        }

        if let Err(e) = bootloader.save_fsck_status(partition, fsck.code, &fsck.output) {
            warn!(
                "Failed to save fsck status for {} to bootloader env: {}",
                partition, e
            );
        }

        if !fsck.output.is_empty() {
            if !data_mounted {
                warn!(
                    "Data partition not mounted; skipping fsck log write for {}",
                    partition
                );
                continue;
            }
            if let Err(e) = fs::create_dir_all(&log_dir) {
                warn!("Failed to create fsck log dir {}: {}", log_dir.display(), e);
            } else {
                let log_path = log_dir.join(format!("{}.log", partition));
                if let Err(e) = fs::write(&log_path, &fsck.output) {
                    warn!("Failed to write fsck log {}: {}", log_path.display(), e);
                } else {
                    info!("Wrote fsck log: {}", log_path.display());
                }
            }
        }
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
