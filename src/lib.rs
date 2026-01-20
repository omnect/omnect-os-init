//! omnect-os-init library
//!
//! This library provides the core functionality for the omnect-os init process.
//! It replaces the bash-based initramfs scripts with a type-safe Rust implementation.

pub mod bootloader;
pub mod config;
pub mod error;
pub mod logging;

// Re-export main types for convenience
pub use bootloader::{Bootloader, BootloaderType, create_bootloader};
pub use config::Config;
pub use error::{InitramfsError, Result};
pub use logging::KmsgLogger;
