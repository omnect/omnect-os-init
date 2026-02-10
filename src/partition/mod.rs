//! Partition detection and management
//!
//! This module handles:
//! - Root device detection using stat(2) + sysfs
//! - Partition table type detection (GPT vs DOS)
//! - Creation of /dev/omnect/* symlinks

mod device;
mod layout;
mod symlinks;

pub use self::device::{RootDevice, detect_root_device};
pub use self::layout::{PartitionLayout, PartitionTableType};
pub use self::symlinks::create_omnect_symlinks;

use crate::error::PartitionError;

pub type Result<T> = std::result::Result<T, PartitionError>;
