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
        mut bootloader,
        mut ods_status,
    } = ctx;

    let mount_result = mount_remaining_partitions(layout, rootfs, &mut ods_status);

    if let Some(bl) = bootloader.available_mut() {
        persist_fsck_results(&ods_status, bl, rootfs);
    }

    mount_result?;

    setup_raw_rootfs_mount(rootfs)?;
    setup_etc_overlay(rootfs)?;
    setup_data_overlay(rootfs)?;
    create_fs_links(rootfs)?;

    create_ods_runtime_files(
        &ods_status,
        bootloader.available(),
        rootfs,
        Path::new(ODS_RUNTIME_DIR),
    )?;

    info!("omnect-os-initramfs completed successfully");

    switch_root(rootfs, &config.cmdline)
}
