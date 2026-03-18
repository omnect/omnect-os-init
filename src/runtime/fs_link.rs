//! Filesystem link creation from configuration
//!
//! Creates symbolic links based on fs-link configuration files.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{InitramfsError, Result};

/// Configuration file path for fs-link
const FS_LINK_CONFIG_PATH: &str = "etc/omnect/fs-link.json";

/// Fallback config path
const FS_LINK_CONFIG_PATH_D: &str = "etc/omnect/fs-link.d";

/// Configuration for fs-link
#[derive(Debug, Clone, Deserialize)]
pub struct FsLinkConfig {
    /// List of links to create
    pub links: Vec<LinkEntry>,
}

/// A single link entry
#[derive(Debug, Clone, Deserialize)]
pub struct LinkEntry {
    /// Target of the symlink (what it points to)
    pub target: String,
    /// Path where the symlink is created
    pub link: String,
}

/// Create symbolic links based on fs-link configuration
pub fn create_fs_links(rootfs_dir: &Path) -> Result<()> {
    let config = load_fs_link_config(rootfs_dir)?;

    for entry in &config.links {
        create_link(rootfs_dir, entry)?;
    }

    if !config.links.is_empty() {
        log::info!("Created {} fs-links", config.links.len());
    }

    Ok(())
}

/// Load fs-link configuration from all sources
fn load_fs_link_config(rootfs_dir: &Path) -> Result<FsLinkConfig> {
    let mut all_links = Vec::new();

    // Load main config file
    let main_config_path = rootfs_dir.join(FS_LINK_CONFIG_PATH);
    if main_config_path.exists() {
        let config = load_config_file(&main_config_path)?;
        all_links.extend(config.links);
    }

    // Load config.d directory
    let config_d_path = rootfs_dir.join(FS_LINK_CONFIG_PATH_D);
    if config_d_path.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(&config_d_path)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .collect();

        // Sort for deterministic order
        entries.sort_by_key(|e| e.path());

        for entry in entries {
            let config = load_config_file(&entry.path())?;
            all_links.extend(config.links);
        }
    }

    Ok(FsLinkConfig { links: all_links })
}

/// Load a single config file
fn load_config_file(path: &Path) -> Result<FsLinkConfig> {
    let content = fs::read_to_string(path)?;
    let config: FsLinkConfig = serde_json::from_str(&content).map_err(|e| {
        InitramfsError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Failed to parse fs-link config {}: {}", path.display(), e),
        ))
    })?;

    log::debug!(
        "Loaded {} links from {}",
        config.links.len(),
        path.display()
    );

    Ok(config)
}

/// Validate that a config-supplied path is a safe relative path.
///
/// Rejects absolute paths (Path::join would silently discard rootfs_dir) and
/// paths containing `..` components (directory traversal outside rootfs).
fn validate_relative_path(path: &str) -> Result<()> {
    let p = Path::new(path);
    if p.is_absolute() {
        return Err(InitramfsError::Io(std::io::Error::other(format!(
            "fs-link path must be relative, got absolute path: {path}"
        ))));
    }
    if p.components().any(|c| c == std::path::Component::ParentDir) {
        return Err(InitramfsError::Io(std::io::Error::other(format!(
            "fs-link path must not contain '..': {path}"
        ))));
    }
    Ok(())
}

/// Create a single symbolic link
fn create_link(rootfs_dir: &Path, entry: &LinkEntry) -> Result<()> {
    validate_relative_path(&entry.link)?;
    let link_path = rootfs_dir.join(&entry.link);
    let target = PathBuf::from(&entry.target);

    // Ensure parent directory exists
    if let Some(parent) = link_path.parent()
        && !parent.exists()
    {
        fs::create_dir_all(parent)?;
    }

    // Remove existing link/file if present
    if link_path.exists() || link_path.is_symlink() {
        // A plain directory cannot be replaced by a symlink — flag it clearly.
        // Symlinks pointing to directories are fine and must be replaceable.
        if !link_path.is_symlink() && link_path.is_dir() {
            return Err(InitramfsError::Io(std::io::Error::other(format!(
                "Cannot replace directory with symlink: {}",
                link_path.display()
            ))));
        }
        fs::remove_file(&link_path).map_err(|e| {
            InitramfsError::Io(std::io::Error::other(format!(
                "Failed to remove existing file {}: {}",
                link_path.display(),
                e
            )))
        })?;
    }

    // Create the symlink
    symlink(&target, &link_path).map_err(|e| {
        InitramfsError::Io(std::io::Error::other(format!(
            "Failed to create symlink {} -> {}: {}",
            link_path.display(),
            target.display(),
            e
        )))
    })?;

    log::debug!(
        "Created symlink: {} -> {}",
        link_path.display(),
        target.display()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_link_entry_deserialize() {
        let json = r#"{"target": "/data/app", "link": "opt/app"}"#;
        let entry: LinkEntry = serde_json::from_str(json).unwrap();

        assert_eq!(entry.target, "/data/app");
        assert_eq!(entry.link, "opt/app");
    }

    #[test]
    fn test_fs_link_config_deserialize() {
        let json = r#"{
            "links": [
                {"target": "/data/app", "link": "opt/app"},
                {"target": "/data/config", "link": "etc/app"}
            ]
        }"#;

        let config: FsLinkConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.links.len(), 2);
    }

    #[test]
    fn test_create_link() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("target");
        fs::create_dir_all(&target_dir).unwrap();

        let entry = LinkEntry {
            target: target_dir.to_string_lossy().to_string(),
            link: "link".to_string(),
        };

        create_link(temp.path(), &entry).unwrap();

        let link_path = temp.path().join("link");
        assert!(link_path.is_symlink());
        assert_eq!(fs::read_link(&link_path).unwrap(), target_dir);
    }

    #[test]
    fn test_create_link_replaces_existing() {
        let temp = TempDir::new().unwrap();
        let target1 = temp.path().join("target1");
        let target2 = temp.path().join("target2");
        fs::create_dir_all(&target1).unwrap();
        fs::create_dir_all(&target2).unwrap();

        let link_path = temp.path().join("link");

        // Create first link
        let entry1 = LinkEntry {
            target: target1.to_string_lossy().to_string(),
            link: "link".to_string(),
        };
        create_link(temp.path(), &entry1).unwrap();

        // Replace with second link
        let entry2 = LinkEntry {
            target: target2.to_string_lossy().to_string(),
            link: "link".to_string(),
        };
        create_link(temp.path(), &entry2).unwrap();

        assert_eq!(fs::read_link(&link_path).unwrap(), target2);
    }

    #[test]
    fn test_create_link_nested_path() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("target");
        fs::create_dir_all(&target).unwrap();

        let entry = LinkEntry {
            target: target.to_string_lossy().to_string(),
            link: "nested/deep/link".to_string(),
        };

        create_link(temp.path(), &entry).unwrap();

        let link_path = temp.path().join("nested/deep/link");
        assert!(link_path.is_symlink());
    }

    #[test]
    fn test_load_empty_config() {
        let temp = TempDir::new().unwrap();
        let config = load_fs_link_config(temp.path()).unwrap();
        assert!(config.links.is_empty());
    }

    #[test]
    fn test_load_config_file() {
        let temp = TempDir::new().unwrap();
        let config_path = temp.path().join("config.json");

        let json = r#"{"links": [{"target": "/data", "link": "opt"}]}"#;
        fs::write(&config_path, json).unwrap();

        let config = load_config_file(&config_path).unwrap();
        assert_eq!(config.links.len(), 1);
    }
}
