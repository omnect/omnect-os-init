use anyhow::{bail, ensure, Result};
use std::process::Command;

// ToDo: Currently only works for u-boot devices

pub fn get_uboot_env(key: &str) -> Result<Option<String>> {
    if let Ok(output) = Command::new("/usr/bin/fw_printenv").arg(key).output() {
        ensure!(output.status.success(), "failed to get {key}");
        let status = String::from_utf8(output.stdout)?;
        let vec: Vec<&str> = status.split('=').collect();
        ensure!(vec.len() > 1, "failed to parse value from {key}");
        let val = vec[1];
        if val != "\n" {
            return Ok(Some(val.to_string()));
        }
    } else {
        bail!("fw_printenv error");
    }
    Ok(None)
}

pub fn set_uboot_env(key: &str, value: &str) -> Result<()> {
    ensure!(
        Command::new("/usr/bin/fw_setenv")
            .arg(key)
            .arg(value)
            .status()?
            .success(),
        "fw_setenv {key}={value} failed"
    );
    Ok(())
}
