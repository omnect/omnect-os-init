use std::path::Path;

use log::info;

use crate::{
    Result,
    filesystem::{setup_data_overlay, setup_etc_overlay, setup_raw_rootfs_mount},
    mode::BootContext,
    runtime::{ODS_RUNTIME_DIR, create_fs_links, create_ods_runtime_files, switch_root},
};

pub fn run(ctx: BootContext<'_>) -> Result<()> {
    let BootContext {
        config,
        rootfs,
        bootloader,
        ods_status,
        ..
    } = ctx;

    setup_raw_rootfs_mount(rootfs)?;
    setup_etc_overlay(rootfs)?;
    setup_data_overlay(rootfs)?;
    create_fs_links(rootfs)?;

    create_ods_runtime_files(
        &ods_status,
        bootloader.as_deref(),
        rootfs,
        Path::new(ODS_RUNTIME_DIR),
    )?;

    info!("omnect-os-initramfs completed successfully");

    switch_root(rootfs, &config.cmdline)
}
