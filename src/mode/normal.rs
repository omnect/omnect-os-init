use std::path::Path;

use log::info;

use crate::{
    Result,
    filesystem::{
        mount_remaining_partitions, persist_fsck_results, setup_data_overlay, setup_etc_overlay,
        setup_raw_rootfs_mount,
    },
    mode::BootContext,
    runtime::{ODS_RUNTIME_DIR, create_fs_links, create_ods_runtime_files, switch_root},
};

fn write_first_boot_marker(first_boot: bool, bootloader: &mut crate::bootloader::BootEnvState) {
    if first_boot
        && let Some(bl) = bootloader.available_mut()
        && let Err(e) = bl.set_env(crate::bootloader::BootEnvKey::FirstBootDone, Some("1"))
    {
        log::warn!("first-boot marker write failed: {e}; will retry next boot");
    }
}

/// Run the normal boot path.
///
/// # Mode obligation: persist fsck results
///
/// This handler (and any future mode handler) is responsible for calling
/// `persist_fsck_results` after `mount_remaining_partitions`, capturing the
/// result before propagating any mount error. Skipping this call means fsck
/// diagnostics for data/factory/cert/etc are silently lost on a failed boot.
pub fn run(ctx: BootContext<'_>) -> Result<()> {
    let BootContext {
        config,
        layout,
        rootfs,
        mut boot_env,
        mut ods_status,
    } = ctx;

    let mount_result = mount_remaining_partitions(layout, rootfs, &mut ods_status);

    // Write fsck diagnostics regardless of bootloader availability so on-disk
    // log files (/data/var/log/fsck/) are produced even in degraded mode.
    persist_fsck_results(&ods_status, boot_env.available_mut(), rootfs);

    mount_result?;

    setup_raw_rootfs_mount(rootfs)?;
    setup_etc_overlay(rootfs)?;
    setup_data_overlay(rootfs)?;
    create_fs_links(rootfs)?;

    create_ods_runtime_files(
        &ods_status,
        boot_env.available(),
        rootfs,
        Path::new(ODS_RUNTIME_DIR),
    )?;

    // Single write point for the unified first-boot sentinel. Best-effort:
    // failure is logged and does not abort the boot — the next boot retries.
    write_first_boot_marker(ods_status.first_boot, &mut boot_env);

    info!("omnect-os-initramfs completed successfully");

    switch_root(rootfs, &config.cmdline)
}

#[cfg(test)]
mod marker_writer_tests {
    use super::*;
    use crate::bootloader::{BootEnvKey, BootEnvState, MockBootEnv};
    use crate::error::BootEnvError;

    #[test]
    fn writes_marker_when_first_boot() {
        let mock = MockBootEnv::new();
        let mut env = BootEnvState::Available(Box::new(mock));
        write_first_boot_marker(true, &mut env);
        let bl = env.available().unwrap();
        assert_eq!(
            bl.get_env(BootEnvKey::FirstBootDone).unwrap(),
            Some("1".to_string())
        );
    }

    #[test]
    fn does_not_write_when_not_first_boot() {
        let mock = MockBootEnv::new();
        let mut env = BootEnvState::Available(Box::new(mock));
        write_first_boot_marker(false, &mut env);
        let bl = env.available().unwrap();
        assert_eq!(bl.get_env(BootEnvKey::FirstBootDone).unwrap(), None);
    }

    #[test]
    fn no_op_on_degraded_env() {
        let mut env = BootEnvState::Degraded(BootEnvError::CommandFailed {
            command: "grub-editenv".into(),
            reason: "test".into(),
        });
        // Must not panic; nothing to assert beyond completion.
        write_first_boot_marker(true, &mut env);
    }
}
