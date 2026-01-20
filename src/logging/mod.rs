//! Logging module for omnect-os-init
//!
//! Provides logging to /dev/kmsg with proper kernel log levels.
//! All messages are prefixed with "omnect-os-init:" to make them
//! identifiable in the kernel log.

mod kmsg;

pub use kmsg::KmsgLogger;
