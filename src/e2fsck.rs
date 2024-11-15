use anyhow::Result;
use log::{error, info};
use std::process::Command;

// ToDo Currently does not provide a result to userspace

pub fn run() -> Result<()> {
    info!("e2fsck");

    // is normally mount readonly; test anyway
    if !Command::new("/sbin/e2fsck")
        .args(["-y", "/dev/omnect/rootCurrent"])
        .status()?
        .success()
    {
        //only log error
        error!("fsck root failed");
    }

    if !Command::new("/sbin/e2fsck")
        .args(["-y", "/dev/omnect/etc"])
        .status()?
        .success()
    {
        //only log error
        error!("fsck etc failed");
    }

    if !Command::new("/sbin/e2fsck")
        .args(["-y", "/dev/omnect/cert"])
        .status()?
        .success()
    {
        //only log error
        error!("fsck cert failed");
    }

    if !Command::new("/sbin/e2fsck")
        .args(["-y", "/dev/omnect/factory"])
        .status()?
        .success()
    {
        //only log error
        error!("fsck factory failed");
    }

    if !Command::new("/sbin/e2fsck")
        .args(["-y", "/dev/omnect/data"])
        .status()?
        .success()
    {
        //only log error
        error!("fsck data failed");
    }
    Ok(())
}
