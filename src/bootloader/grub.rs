//! GRUB bootloader implementation
//!
//! This module provides bootloader environment access for GRUB-based systems
//! (typically x86-64 EFI). It uses the `grub-editenv` command to read and
//! write environment variables from the grubenv file.

use super::types::{compress_and_encode, decode_and_decompress, BootloaderType};
use super::Bootloader;
use crate::error::{BootloaderError, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// GRUB bootloader implementation
pub struct GrubBootloader {
    /// Path to the rootfs directory
    rootfs_dir: PathBuf,
    /// Path to the grubenv file
    grubenv_path: PathBuf,
    /// Path to the boot partition mount point
    boot_mount: PathBuf,
    /// Whether we mounted the boot partition
    mounted_boot: bool,
}

impl GrubBootloader {
    /// Create a new GrubBootloader instance
    ///
    /// The rootfs_dir should be the path where the rootfs is mounted.
    pub fn new(rootfs_dir: &Path) -> Result<Self> {
        let boot_mount = rootfs_dir.join("boot");
        let grubenv_path = boot_mount.join("EFI/BOOT/grubenv");

        Ok(Self {
            rootfs_dir: rootfs_dir.to_path_buf(),
            grubenv_path,
            boot_mount,
            mounted_boot: false,
        })
    }

    /// Ensure the boot partition is mounted
    fn ensure_boot_mounted(&mut self) -> Result<()> {
        // Check if already mounted
        if self.is_boot_mounted()? {
            return Ok(());
        }

        // Create mount point if needed
        if !self.boot_mount.exists() {
            fs::create_dir_all(&self.boot_mount).map_err(|e| BootloaderError::MountFailed(
                format!("failed to create mount point: {}", e),
            ))?;
        }

        // Mount the boot partition
        // Note: This assumes /dev/omnect/boot symlink exists
        let status = Command::new("mount")
            .arg("-t")
            .arg("vfat")
            .arg("/dev/omnect/boot")
            .arg(&self.boot_mount)
            .status()
            .map_err(|e| BootloaderError::MountFailed(format!("mount command failed: {}", e)))?;

        if !status.success() {
            return Err(BootloaderError::MountFailed(format!(
                "mount returned exit code {}",
                status.code().unwrap_or(-1)
            ))
            .into());
        }

        self.mounted_boot = true;
        log::info!("Mounted boot partition at {}", self.boot_mount.display());
        Ok(())
    }

    /// Check if boot partition is mounted
    fn is_boot_mounted(&self) -> Result<bool> {
        let mounts = fs::read_to_string("/proc/mounts").map_err(|e| {
            BootloaderError::MountFailed(format!("failed to read /proc/mounts: {}", e))
        })?;

        let boot_mount_str = self.boot_mount.to_string_lossy();
        Ok(mounts.lines().any(|line| {
            line.split_whitespace()
                .nth(1)
                .map(|mp| mp == boot_mount_str.as_ref())
                .unwrap_or(false)
        }))
    }

    /// Run grub-editenv with the given arguments
    fn run_grub_editenv(&self, args: &[&str]) -> Result<String> {
        let grub_editenv = self.rootfs_dir.join("usr/bin/grub-editenv");
        
        let output = Command::new(&grub_editenv)
            .arg(&self.grubenv_path)
            .args(args)
            .output()
            .map_err(|e| BootloaderError::CommandFailed {
                command: "grub-editenv".to_string(),
                reason: e.to_string(),
            })?;

        if !output.status.success() {
            return Err(BootloaderError::CommandFailed {
                command: format!("grub-editenv {:?}", args),
                reason: String::from_utf8_lossy(&output.stderr).to_string(),
            }
            .into());
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Get the path to the fsck status file for a partition
    fn fsck_file_path(&self, partition: &str) -> PathBuf {
        self.boot_mount.join(format!("fsck.{}", partition))
    }
}

impl Bootloader for GrubBootloader {
    fn get_env(&self, key: &str) -> Result<Option<String>> {
        // Note: For get_env, the caller should ensure boot is mounted if needed
        // This is a read-only operation that might fail if boot isn't mounted
        
        let output = match self.run_grub_editenv(&["list"]) {
            Ok(o) => o,
            Err(_) => return Ok(None), // Treat errors as variable not found
        };

        // Parse the output: each line is "key=value"
        for line in output.lines() {
            if let Some((k, v)) = line.split_once('=') {
                if k == key {
                    return Ok(Some(v.to_string()));
                }
            }
        }

        Ok(None)
    }

    fn set_env(&mut self, key: &str, value: Option<&str>) -> Result<()> {
        self.ensure_boot_mounted()?;

        match value {
            Some(v) => {
                self.run_grub_editenv(&["set", &format!("{}={}", key, v)])?;
            }
            None => {
                self.run_grub_editenv(&["unset", key])?;
            }
        }

        // Sync to ensure changes are written
        let _ = Command::new("sync").status();

        Ok(())
    }

    fn save_fsck_status(&mut self, partition: &str, output: &str, code: i32) -> Result<()> {
        self.ensure_boot_mounted()?;

        if partition == "boot" {
            // For boot partition, store in grubenv variable (limited space)
            if code != 2 {
                // Don't save if reboot is required (code 2)
                let encoded = compress_and_encode(output)?;
                
                // Try to set the variable; if it's too big, store a placeholder
                if let Err(_) = self.set_env("omnect_fsck_boot", Some(&encoded)) {
                    let fallback = compress_and_encode("fsck output too big")?;
                    self.set_env("omnect_fsck_boot", Some(&fallback))?;
                }
            }
        } else {
            // For other partitions, store in a file on the boot partition
            let encoded = compress_and_encode(output)?;
            let fsck_file = self.fsck_file_path(partition);
            
            fs::write(&fsck_file, &encoded).map_err(|e| BootloaderError::WriteFailed {
                name: format!("fsck.{}", partition),
                reason: e.to_string(),
            })?;
        }

        Ok(())
    }

    fn get_fsck_status(&self, partition: &str) -> Result<Option<String>> {
        if partition == "boot" {
            // Read from grubenv variable
            match self.get_env("omnect_fsck_boot")? {
                Some(encoded) => Ok(Some(decode_and_decompress(&encoded)?)),
                None => Ok(None),
            }
        } else {
            // Read from file
            let fsck_file = self.fsck_file_path(partition);
            
            if !fsck_file.exists() {
                return Ok(None);
            }

            let encoded = fs::read_to_string(&fsck_file).map_err(|e| BootloaderError::ReadFailed {
                name: format!("fsck.{}", partition),
                reason: e.to_string(),
            })?;

            Ok(Some(decode_and_decompress(&encoded)?))
        }
    }

    fn clear_fsck_status(&mut self, partition: &str) -> Result<()> {
        self.ensure_boot_mounted()?;

        if partition == "boot" {
            self.set_env("omnect_fsck_boot", None)?;
        } else {
            let fsck_file = self.fsck_file_path(partition);
            if fsck_file.exists() {
                fs::remove_file(&fsck_file).map_err(|e| BootloaderError::WriteFailed {
                    name: format!("fsck.{}", partition),
                    reason: e.to_string(),
                })?;
            }
        }

        Ok(())
    }

    fn bootloader_type(&self) -> BootloaderType {
        BootloaderType::Grub
    }
}

impl Drop for GrubBootloader {
    fn drop(&mut self) {
        // Unmount boot partition if we mounted it
        if self.mounted_boot {
            let _ = Command::new("sync").status();
            let _ = Command::new("umount").arg(&self.boot_mount).status();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_mock_grub_env() -> TempDir {
        let temp = TempDir::new().unwrap();
        let rootfs = temp.path();
        
        // Create directory structure
        fs::create_dir_all(rootfs.join("usr/bin")).unwrap();
        fs::create_dir_all(rootfs.join("boot/EFI/BOOT")).unwrap();
        
        // Create a mock grub-editenv script
        let mock_script = r#"#!/bin/sh
echo "Mock grub-editenv"
"#;
        fs::write(rootfs.join("usr/bin/grub-editenv"), mock_script).unwrap();
        
        temp
    }

    #[test]
    fn test_grub_bootloader_creation() {
        let temp = setup_mock_grub_env();
        let result = GrubBootloader::new(temp.path());
        assert!(result.is_ok());
        
        let bl = result.unwrap();
        assert_eq!(bl.bootloader_type(), BootloaderType::Grub);
    }

    #[test]
    fn test_fsck_file_path() {
        let temp = setup_mock_grub_env();
        let bl = GrubBootloader::new(temp.path()).unwrap();
        
        let path = bl.fsck_file_path("data");
        assert!(path.to_string_lossy().contains("fsck.data"));
    }
}
