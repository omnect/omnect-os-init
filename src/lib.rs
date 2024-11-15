mod e2fsck;
mod finish;
mod mount;
mod resizefs;
mod rootblk;
mod util;

use anyhow::{bail, Result};
use log::{error, info};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::fs::{create_dir_all, read_to_string};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use sys_mount::Mount;

#[macro_use]
extern crate lazy_static;
extern crate kernlog;
extern crate log;

lazy_static! {
    static ref PARAMS: Mutex<HashMap<String, String>> = Mutex::new(HashMap::new());
}

pub fn init(params: &mut HashMap<String, String>) -> Result<()> {
    create_dir_all("/dev")?;
    create_dir_all("/proc")?;
    create_dir_all("/sys")?;
    create_dir_all("/run/lock")?;
    create_dir_all("/var/lock")?;
    create_dir_all("/rootfs")?;

    Mount::builder()
        .fstype("devtmpfs")
        .mount("devtmpfs", "/dev")?;
    Mount::builder().fstype("proc").mount("proc", "/proc")?;
    Mount::builder().fstype("sysfs").mount("sysfs", "/sys")?;

    if Path::try_exists(Path::new("/sys/firmware/efi"))? {
        Mount::builder()
            .fstype("efivarfs")
            .mount("none", "/sys/firmware/efi/efivars")?;
    }

    let cmdline = read_to_string("/proc/cmdline")?;
    let mut cmdline = cmdline.split(' ');
    loop {
        let Some(key_value)=cmdline.next() else {
            break;
        };
        let mut key_value = key_value.split('=');
        let Some(key) = key_value.next() else {
            bail!("could create key from {key_value:?}");
        };
        if let Some(value) = key_value.next() {
            params.insert(key.to_string(), value.to_string());
        } else {
            params.insert(key.to_string(), "true".to_string());
        };
    }

    Ok(())
}

pub fn _run(params: &mut HashMap<String, String>) -> Result<()> {
    rootblk::run(params)?;
    e2fsck::run()?;
    //don't stop boot, if resizefs failed
    if let Err(e) = resizefs::run() {
        error!("{e}");
    }
    mount::run()?;
    finish::run(params)?;
    Ok(())
}

pub fn run() -> Result<()> {
    let mut map = PARAMS
        .lock()
        .or_else(|e| bail!("couldn't get PARAMS lock: {e:?}"))?;
    init(&mut map)?;

    let mut printk_devkmsg = OpenOptions::new()
        .write(true)
        .create(false)
        .truncate(true)
        .open("/proc/sys/kernel/printk_devkmsg")?;
    writeln!(printk_devkmsg, "on")?;

    kernlog::init()?;
    info!(
        "init {} ({})",
        env!("CARGO_PKG_VERSION"),
        env!("GIT_SHORT_REV")
    );
    if let Err(e) = _run(&mut map) {
        error!("Application error: {e:#?}");
        return Err(e);
    }
    Ok(())
}
