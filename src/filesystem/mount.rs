//! Mount operations
//!
//! Provides free functions for performing mount operations, along with
//! `MountOptions` and `MountPoint` builder types for composing mount flags.

use std::fmt;
use std::path::{Path, PathBuf};

use nix::mount::{MsFlags, mount as nix_mount};

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
    pub const NODIRATIME: MsFlags = MsFlags::MS_NODIRATIME;
    pub const NOSUID: MsFlags = MsFlags::MS_NOSUID;
    pub const NODEV: MsFlags = MsFlags::MS_NODEV;
    pub const NOEXEC: MsFlags = MsFlags::MS_NOEXEC;
}

/// Filesystem type for mount and fsck operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsType {
    Ext4,
    Vfat,
    Tmpfs,
    Overlay,
}

impl FsType {
    /// Canonical filesystem type string passed to mount(2) and fsck -t.
    pub const fn as_str(self) -> &'static str {
        match self {
            FsType::Ext4 => "ext4",
            FsType::Vfat => "vfat",
            FsType::Tmpfs => "tmpfs",
            FsType::Overlay => "overlay",
        }
    }
}

impl fmt::Display for FsType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for FsType {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Options for mounting a filesystem
#[derive(Debug, Clone)]
pub struct MountOptions {
    /// Filesystem type
    pub fstype: Option<FsType>,
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
            fstype: Some(FsType::Ext4),
            flags: flags::RDONLY,
            data: None,
        }
    }

    /// Create options for a read-write ext4 mount
    pub fn ext4_readwrite() -> Self {
        Self {
            fstype: Some(FsType::Ext4),
            flags: MsFlags::empty(),
            data: None,
        }
    }

    /// Create options for a FAT32 boot partition
    pub fn vfat() -> Self {
        Self {
            fstype: Some(FsType::Vfat),
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
            fstype: Some(FsType::Tmpfs),
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

    /// Add nodiratime flag
    pub fn nodiratime(mut self) -> Self {
        self.flags |= flags::NODIRATIME;
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

/// Mount a filesystem described by `mp`.
pub fn mount(mp: MountPoint) -> Result<()> {
    let source: Option<&Path> = if mp.source.as_os_str().is_empty() {
        None
    } else {
        Some(&mp.source)
    };

    let fstype: Option<&str> = mp.options.fstype.map(|t| t.as_str());
    let data: Option<&str> = mp.options.data.as_deref();

    nix_mount(source, &mp.target, fstype, mp.options.flags, data).map_err(|e| {
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
        fstype.unwrap_or("<none>")
    );

    Ok(())
}

/// Mount a filesystem read-write.
pub fn mount_readwrite(
    source: impl Into<PathBuf>,
    target: impl Into<PathBuf>,
    fstype: FsType,
) -> Result<()> {
    mount(MountPoint::new(
        source,
        target,
        MountOptions {
            fstype: Some(fstype),
            flags: MsFlags::empty(),
            data: None,
        },
    ))
}

/// Mount a tmpfs filesystem.
pub fn mount_tmpfs(target: impl Into<PathBuf>, flags: MsFlags, data: Option<&str>) -> Result<()> {
    mount(MountPoint::new(
        FsType::Tmpfs.as_str(),
        target,
        MountOptions {
            fstype: Some(FsType::Tmpfs),
            flags,
            data: data.map(|s| s.to_string()),
        },
    ))
}

/// Create a bind mount.
pub fn mount_bind(source: impl Into<PathBuf>, target: impl Into<PathBuf>) -> Result<()> {
    mount(MountPoint::new(source, target, MountOptions::bind()))
}

/// Create a private bind mount (propagation isolated).
pub fn mount_bind_private(source: impl Into<PathBuf>, target: impl Into<PathBuf>) -> Result<()> {
    let source = source.into();
    let target = target.into();
    mount(MountPoint::new(
        source,
        target.clone(),
        MountOptions::bind(),
    ))?;
    // Isolate propagation so overlay mounts on top don't bleed into adjacent namespaces.
    nix_mount(
        None::<&str>,
        &target,
        None::<&str>,
        flags::PRIVATE | flags::REC,
        None::<&str>,
    )
    .map_err(|e| FilesystemError::MountFailed {
        src_path: PathBuf::new(),
        target: target.clone(),
        reason: format!("Failed to make mount private: {e}"),
    })?;
    log::debug!("Made {} private", target.display());
    Ok(())
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
    fn test_fstype_as_str() {
        assert_eq!(FsType::Ext4.as_str(), "ext4");
        assert_eq!(FsType::Vfat.as_str(), "vfat");
        assert_eq!(FsType::Tmpfs.as_str(), "tmpfs");
        assert_eq!(FsType::Overlay.as_str(), "overlay");
    }

    #[test]
    fn test_fstype_display() {
        assert_eq!(FsType::Ext4.to_string(), "ext4");
        assert_eq!(FsType::Vfat.to_string(), "vfat");
        assert_eq!(FsType::Tmpfs.to_string(), "tmpfs");
        assert_eq!(FsType::Overlay.to_string(), "overlay");
    }

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
        assert_eq!(opts.fstype, Some(FsType::Ext4));
        assert!(opts.flags.contains(MsFlags::MS_RDONLY));
    }

    #[test]
    fn test_mount_options_builder() {
        let opts = MountOptions::ext4_readwrite()
            .noatime()
            .nosuid()
            .with_data("discard");

        assert_eq!(opts.fstype, Some(FsType::Ext4));
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
        assert_eq!(mp.options.fstype, Some(FsType::Vfat));
    }
}
