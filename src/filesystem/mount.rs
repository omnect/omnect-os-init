//! Mount operations with tracking for cleanup
//!
//! Provides a MountManager that tracks all mounts and can unmount them
//! in reverse order on error or cleanup.

use std::path::{Path, PathBuf};

use nix::mount::{MntFlags, MsFlags, mount, umount2};

use crate::error::FilesystemError;
use crate::filesystem::Result;

/// Mount flag constants
mod flags {
    use nix::mount::MsFlags;

    pub const RDONLY: MsFlags = MsFlags::MS_RDONLY;
    pub const BIND: MsFlags = MsFlags::MS_BIND;
    pub const PRIVATE: MsFlags = MsFlags::MS_PRIVATE;
    pub const REC: MsFlags = MsFlags::MS_REC;
    pub const NOATIME: MsFlags = MsFlags::MS_NOATIME;
    pub const NOSUID: MsFlags = MsFlags::MS_NOSUID;
    pub const NODEV: MsFlags = MsFlags::MS_NODEV;
    pub const NOEXEC: MsFlags = MsFlags::MS_NOEXEC;
}

/// Common filesystem types
mod fstype {
    pub const EXT4: &str = "ext4";
    pub const VFAT: &str = "vfat";
    pub const TMPFS: &str = "tmpfs";
}

/// Options for mounting a filesystem
#[derive(Debug, Clone)]
pub struct MountOptions {
    /// Filesystem type (e.g., "ext4", "vfat", "overlay")
    pub fstype: Option<String>,
    /// Mount flags
    pub flags: MsFlags,
    /// Additional mount data/options string
    pub data: Option<String>,
}

impl Default for MountOptions {
    fn default() -> Self {
        Self {
            fstype: None,
            flags: MsFlags::empty(),
            data: None,
        }
    }
}

impl MountOptions {
    /// Create options for a read-only ext4 mount
    pub fn ext4_readonly() -> Self {
        Self {
            fstype: Some(fstype::EXT4.to_string()),
            flags: flags::RDONLY,
            data: None,
        }
    }

    /// Create options for a read-write ext4 mount
    pub fn ext4_readwrite() -> Self {
        Self {
            fstype: Some(fstype::EXT4.to_string()),
            flags: MsFlags::empty(),
            data: None,
        }
    }

    /// Create options for a FAT32 boot partition
    pub fn vfat() -> Self {
        Self {
            fstype: Some(fstype::VFAT.to_string()),
            flags: MsFlags::empty(),
            data: None,
        }
    }

    /// Create options for a bind mount
    pub fn bind() -> Self {
        Self {
            fstype: None,
            flags: flags::BIND,
            data: None,
        }
    }

    /// Create options for a tmpfs mount
    pub fn tmpfs() -> Self {
        Self {
            fstype: Some(fstype::TMPFS.to_string()),
            flags: MsFlags::empty(),
            data: None,
        }
    }

    /// Add read-only flag
    pub fn readonly(mut self) -> Self {
        self.flags |= flags::RDONLY;
        self
    }

    /// Add noatime flag
    pub fn noatime(mut self) -> Self {
        self.flags |= flags::NOATIME;
        self
    }

    /// Add nosuid flag
    pub fn nosuid(mut self) -> Self {
        self.flags |= flags::NOSUID;
        self
    }

    /// Add nodev flag
    pub fn nodev(mut self) -> Self {
        self.flags |= flags::NODEV;
        self
    }

    /// Add noexec flag
    pub fn noexec(mut self) -> Self {
        self.flags |= flags::NOEXEC;
        self
    }

    /// Set mount data/options string
    pub fn with_data(mut self, data: &str) -> Self {
        self.data = Some(data.to_string());
        self
    }
}

/// Represents a mounted filesystem
#[derive(Debug, Clone)]
pub struct MountPoint {
    /// Source device or path
    pub source: PathBuf,
    /// Target mount point
    pub target: PathBuf,
    /// Mount options used
    pub options: MountOptions,
}

impl MountPoint {
    /// Create a new mount point definition
    pub fn new(
        source: impl Into<PathBuf>,
        target: impl Into<PathBuf>,
        options: MountOptions,
    ) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            options,
        }
    }
}

/// Manages filesystem mounts with tracking for cleanup
///
/// Tracks all mounts made and provides methods to unmount them
/// in reverse order (LIFO) for proper cleanup.
pub struct MountManager {
    mounts: Vec<MountPoint>,
}

impl MountManager {
    /// Create a new mount manager
    pub fn new() -> Self {
        Self { mounts: Vec::new() }
    }

    /// Mount a filesystem and track it
    pub fn mount(&mut self, mp: MountPoint) -> Result<()> {
        // Ensure target directory exists
        if !mp.target.exists() {
            std::fs::create_dir_all(&mp.target).map_err(|e| FilesystemError::MountFailed {
                src_path: mp.source.clone(),
                target: mp.target.clone(),
                reason: format!("Failed to create mount point: {}", e),
            })?;
        }

        // Perform the mount
        let source: Option<&Path> = if mp.source.as_os_str().is_empty() {
            None
        } else {
            Some(&mp.source)
        };

        let fstype: Option<&str> = mp.options.fstype.as_deref();
        let data: Option<&str> = mp.options.data.as_deref();

        mount(source, &mp.target, fstype, mp.options.flags, data).map_err(|e| {
            FilesystemError::MountFailed {
                src_path: mp.source.clone(),
                target: mp.target.clone(),
                reason: e.to_string(),
            }
        })?;

        log::info!(
            "Mounted {} on {} ({})",
            mp.source.display(),
            mp.target.display(),
            mp.options.fstype.as_deref().unwrap_or("bind")
        );

        self.mounts.push(mp);
        Ok(())
    }

    /// Mount a filesystem read-only
    pub fn mount_readonly(
        &mut self,
        source: impl Into<PathBuf>,
        target: impl Into<PathBuf>,
        fstype: &str,
    ) -> Result<()> {
        let options = MountOptions {
            fstype: Some(fstype.to_string()),
            flags: flags::RDONLY,
            data: None,
        };
        self.mount(MountPoint::new(source, target, options))
    }

    /// Mount a filesystem read-write
    pub fn mount_readwrite(
        &mut self,
        source: impl Into<PathBuf>,
        target: impl Into<PathBuf>,
        fstype: &str,
    ) -> Result<()> {
        let options = MountOptions {
            fstype: Some(fstype.to_string()),
            flags: MsFlags::empty(),
            data: None,
        };
        self.mount(MountPoint::new(source, target, options))
    }

    /// Mount a tmpfs filesystem
    pub fn mount_tmpfs(
        &mut self,
        target: impl Into<PathBuf>,
        flags: MsFlags,
        data: Option<&str>,
    ) -> Result<()> {
        let options = MountOptions {
            fstype: Some(fstype::TMPFS.to_string()),
            flags,
            data: data.map(|s| s.to_string()),
        };
        self.mount(MountPoint::new("tmpfs", target, options))
    }

    /// Create a bind mount
    pub fn mount_bind(
        &mut self,
        source: impl Into<PathBuf>,
        target: impl Into<PathBuf>,
    ) -> Result<()> {
        self.mount(MountPoint::new(source, target, MountOptions::bind()))
    }

    /// Create a private bind mount (doesn't propagate submounts)
    pub fn mount_bind_private(
        &mut self,
        source: impl Into<PathBuf>,
        target: impl Into<PathBuf>,
    ) -> Result<()> {
        let source = source.into();
        let target = target.into();

        // First, create the bind mount
        self.mount(MountPoint::new(
            source.clone(),
            target.clone(),
            MountOptions::bind(),
        ))?;

        // Then make it private (remount with MS_PRIVATE)
        self.make_private(&target)?;

        Ok(())
    }

    /// Make a mount point private (no propagation)
    pub fn make_private(&mut self, target: &Path) -> Result<()> {
        mount(
            None::<&str>,
            target,
            None::<&str>,
            flags::PRIVATE | flags::REC,
            None::<&str>,
        )
        .map_err(|e| FilesystemError::MountFailed {
            src_path: PathBuf::new(),
            target: target.to_path_buf(),
            reason: format!("Failed to make mount private: {}", e),
        })?;

        log::debug!("Made {} private", target.display());
        Ok(())
    }

    /// Unmount a specific target
    pub fn umount(&mut self, target: &Path) -> Result<()> {
        umount2(target, MntFlags::empty()).map_err(|e| FilesystemError::UnmountFailed {
            target: target.to_path_buf(),
            reason: e.to_string(),
        })?;

        // Remove from tracking
        self.mounts.retain(|mp| mp.target != target);

        log::info!("Unmounted {}", target.display());
        Ok(())
    }

    /// Unmount all tracked mounts in reverse order
    ///
    /// Continues on error, collecting all errors.
    pub fn umount_all(&mut self) -> Result<()> {
        let mut errors = Vec::new();

        // Unmount in reverse order (LIFO)
        while let Some(mp) = self.mounts.pop() {
            if let Err(e) = umount2(&mp.target, MntFlags::empty()) {
                log::warn!("Failed to unmount {}: {}", mp.target.display(), e);
                errors.push(FilesystemError::UnmountFailed {
                    target: mp.target,
                    reason: e.to_string(),
                });
            } else {
                log::info!("Unmounted {}", mp.target.display());
            }
        }

        if let Some(first_error) = errors.into_iter().next() {
            Err(first_error)
        } else {
            Ok(())
        }
    }

    /// Get the number of tracked mounts
    pub fn mount_count(&self) -> usize {
        self.mounts.len()
    }

    /// Check if a path is currently mounted (tracked)
    pub fn is_mounted(&self, target: &Path) -> bool {
        self.mounts.iter().any(|mp| mp.target == target)
    }

    /// Get all tracked mount points
    pub fn mounts(&self) -> &[MountPoint] {
        &self.mounts
    }

    /// Forget all tracked mounts without unmounting them.
    ///
    /// Call this immediately before exec-ing into the new root so that
    /// the Drop impl does not tear down mounts that must survive into
    /// the new userspace.
    pub fn release(&mut self) {
        self.mounts.clear();
    }
}

impl Default for MountManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MountManager {
    fn drop(&mut self) {
        if !self.mounts.is_empty() {
            log::warn!(
                "MountManager dropped with {} active mounts - unmounting",
                self.mounts.len()
            );
            let _ = self.umount_all();
        }
    }
}

/// Check if a path is mounted by reading /proc/mounts
pub fn is_path_mounted(path: &Path) -> Result<bool> {
    let mounts =
        std::fs::read_to_string("/proc/mounts").map_err(|e| FilesystemError::MountFailed {
            src_path: PathBuf::new(),
            target: path.to_path_buf(),
            reason: format!("Failed to read /proc/mounts: {e}"),
        })?;
    let path_str = path.to_string_lossy();

    Ok(mounts.lines().any(|line| {
        line.split_whitespace()
            .nth(1)
            .is_some_and(|mount_point| mount_point == path_str)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mount_options_default() {
        let opts = MountOptions::default();
        assert!(opts.fstype.is_none());
        assert!(opts.flags.is_empty());
        assert!(opts.data.is_none());
    }

    #[test]
    fn test_mount_options_ext4_readonly() {
        let opts = MountOptions::ext4_readonly();
        assert_eq!(opts.fstype, Some("ext4".to_string()));
        assert!(opts.flags.contains(MsFlags::MS_RDONLY));
    }

    #[test]
    fn test_mount_options_builder() {
        let opts = MountOptions::ext4_readwrite()
            .noatime()
            .nosuid()
            .with_data("discard");

        assert_eq!(opts.fstype, Some("ext4".to_string()));
        assert!(opts.flags.contains(MsFlags::MS_NOATIME));
        assert!(opts.flags.contains(MsFlags::MS_NOSUID));
        assert!(!opts.flags.contains(MsFlags::MS_RDONLY));
        assert_eq!(opts.data, Some("discard".to_string()));
    }

    #[test]
    fn test_mount_point_new() {
        let mp = MountPoint::new("/dev/sda1", "/mnt/boot", MountOptions::vfat());
        assert_eq!(mp.source, PathBuf::from("/dev/sda1"));
        assert_eq!(mp.target, PathBuf::from("/mnt/boot"));
        assert_eq!(mp.options.fstype, Some("vfat".to_string()));
    }

    #[test]
    fn test_mount_manager_new() {
        let mm = MountManager::new();
        assert_eq!(mm.mount_count(), 0);
    }

    #[test]
    fn test_mount_manager_tracking() {
        let mut mm = MountManager::new();

        // Manually add a mount point for testing (without actually mounting)
        mm.mounts.push(MountPoint::new(
            "/dev/sda1",
            "/mnt/test",
            MountOptions::ext4_readonly(),
        ));

        assert_eq!(mm.mount_count(), 1);
        assert!(mm.is_mounted(Path::new("/mnt/test")));
        assert!(!mm.is_mounted(Path::new("/mnt/other")));
    }

    #[test]
    fn test_mount_manager_mounts_accessor() {
        let mut mm = MountManager::new();

        mm.mounts.push(MountPoint::new(
            "/dev/sda1",
            "/mnt/a",
            MountOptions::default(),
        ));
        mm.mounts.push(MountPoint::new(
            "/dev/sda2",
            "/mnt/b",
            MountOptions::default(),
        ));

        let mounts = mm.mounts();
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].target, PathBuf::from("/mnt/a"));
        assert_eq!(mounts[1].target, PathBuf::from("/mnt/b"));
    }
}
