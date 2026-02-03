//! Logging infrastructure for initramfs
//!
//! This module provides logging to /dev/kmsg with kernel log levels.

mod kmsg;

pub use self::kmsg::{KmsgLogger, log_direct, log_fatal};
