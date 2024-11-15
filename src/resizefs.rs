use super::util;

use anyhow::{ensure, Result};
use log::info;
use std::process::Command;

pub fn run() -> Result<()> {
    let resizedfs = util::get_uboot_env("resized-data")?;
    if resizedfs.is_some() {
        info!("resizefs result: {resizedfs:?}");
        return Ok(());
    }

    info!("resizefs");

    ensure!(
        Command::new("/sbin/e2fsck")
            .args(["-y", "-f", "/dev/omnect/data"])
            .status()?
            .success(),
        "fsck data failed"
    );

    #[cfg(feature = "ptable_gpt")]
    {
        // run_cmd sgdisk ${root_disk} -e
        ensure!(
            Command::new("/usr/sbin/sgdisk")
                .args(["/dev/omnect/rootblk", "-e"])
                .status()?
                .success(),
            "correcting gpt table failed"
        );
        ensure!(
            Command::new("/usr/bin/growpart")
                .args(["/dev/omnect/rootblk", "7"])
                .status()?
                .code()
                != Some(2),
            "growpart data failed"
        );
        ensure!(
            Command::new("/sbin/resize2fs")
                .args(["/dev/omnect/data"])
                .status()?
                .success(),
            "resize2fs data failed"
        );
    }

    #[cfg(feature = "ptable_msdos")]
    {
        ensure!(
            Command::new("/usr/bin/growpart")
                .args(["/dev/omnect/rootblk", "4"])
                .status()?
                .code()
                != Some(2),
            "growpart extended failed"
        );
        ensure!(
            Command::new("/usr/bin/growpart")
                .args(["/dev/omnect/rootblk", "8"])
                .status()?
                .code()
                != Some(2),
            "growpart data failed"
        );
        ensure!(
            Command::new("/sbin/resize2fs")
                .args(["/dev/omnect/data"])
                .status()?
                .success(),
            "resize2fs data failed"
        );
    }

    util::set_uboot_env("resized-data", "1")
}
