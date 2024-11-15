use anyhow::{Context, Result};
use log::info;
use std::env::set_current_dir;
use std::process::Command;
use std::{collections::HashMap, os::unix::process::CommandExt};
use sys_mount::{Mount, MountFlags};

pub fn run(params: &mut HashMap<String, String>) -> Result<()> {
    info!("initramfs finish");

    Mount::builder()
        .flags(MountFlags::MOVE)
        .mount("/sys", "/rootfs/sys")
        .with_context(|| "couldn't move /sys -> /rootfs/sys")?;

    Mount::builder()
        .flags(MountFlags::MOVE)
        .mount("/dev", "/rootfs/dev")
        .with_context(|| "couldn't move /dev -> /rootfs/dev")?;

    Mount::builder()
        .flags(MountFlags::MOVE)
        .mount("/proc", "/rootfs/proc")
        .with_context(|| "couldn't move /proc -> /rootfs/proc")?;

    let default_init = "/sbin/init".to_string();
    let init = params.get("init").unwrap_or(&default_init);
    info!("init: {init}");

    set_current_dir("/rootfs").with_context(|| "couldn't change into dir /rootfs")?;
    Command::new("/sbin/switch_root")
        .args(["/rootfs", (init.as_str())])
        .exec();

    Ok(())
}
