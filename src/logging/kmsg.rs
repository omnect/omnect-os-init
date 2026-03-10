//! Kernel message buffer (kmsg) logging
//!
//! This module provides a logger that writes to /dev/kmsg with proper
//! kernel log level prefixes.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

use log::{Level, Log, Metadata, Record, SetLoggerError};

/// Kernel log level prefixes (see kernel Documentation/admin-guide/serial-console.rst)
mod kernel_level {
    pub const CRIT: &str = "<2>";
    pub const ERR: &str = "<3>";
    pub const WARNING: &str = "<4>";
    pub const INFO: &str = "<6>";
    pub const DEBUG: &str = "<7>";
}

/// Log message prefix for all omnect-os-init messages
const LOG_PREFIX: &str = "omnect-os-initramfs: ";

/// Path to kernel message buffer
const KMSG_PATH: &str = "/dev/kmsg";

/// Logger that writes to /dev/kmsg
pub struct KmsgLogger {
    kmsg: Mutex<File>,
}

impl KmsgLogger {
    /// Create a new kmsg logger
    ///
    /// # Errors
    /// Returns an error if /dev/kmsg cannot be opened for writing
    pub fn new() -> std::io::Result<Self> {
        let file = OpenOptions::new().write(true).open(KMSG_PATH)?;

        Ok(Self {
            kmsg: Mutex::new(file),
        })
    }

    /// Initialize the global logger with kmsg output
    ///
    /// Convenience method that creates a new logger and sets it as global.
    ///
    /// # Errors
    /// Returns an error if /dev/kmsg cannot be opened or a logger is already set
    pub fn init_global() -> std::result::Result<(), String> {
        let logger = Self::new().map_err(|e| format!("Failed to open kmsg: {}", e))?;
        logger
            .init()
            .map_err(|e| format!("Failed to set logger: {}", e))
    }

    /// Initialize this logger as the global logger
    ///
    /// # Errors
    /// Returns an error if a logger has already been set
    pub fn init(self) -> std::result::Result<(), SetLoggerError> {
        log::set_max_level(log::LevelFilter::Debug);
        log::set_boxed_logger(Box::new(self))
    }

    fn level_to_kernel_prefix(level: Level) -> &'static str {
        match level {
            Level::Error => kernel_level::ERR,
            Level::Warn => kernel_level::WARNING,
            Level::Info => kernel_level::INFO,
            Level::Debug => kernel_level::DEBUG,
            Level::Trace => kernel_level::DEBUG,
        }
    }
}

impl Log for KmsgLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let prefix = Self::level_to_kernel_prefix(record.level());
        let message = format!("{}{}{}\n", prefix, LOG_PREFIX, record.args());

        if let Ok(mut kmsg) = self.kmsg.lock() {
            // Ignore write errors - nothing we can do if kmsg fails
            let _ = kmsg.write_all(message.as_bytes());
        }
    }

    fn flush(&self) {
        if let Ok(mut kmsg) = self.kmsg.lock() {
            let _ = kmsg.flush();
        }
    }
}

/// Write a fatal message to kmsg and prepare for system halt
///
/// This function is used when a fatal error occurs and we need to
/// log before potentially halting the system.
pub fn log_fatal(message: &str) {
    if let Ok(mut file) = OpenOptions::new().write(true).open(KMSG_PATH) {
        let _ = writeln!(
            file,
            "{}{}FATAL: {}",
            kernel_level::CRIT,
            LOG_PREFIX,
            message
        );
    }
}

/// Write directly to kmsg without going through the logger
///
/// Useful for early initialization before the logger is set up.
pub fn log_direct(message: &str) {
    if let Ok(mut file) = OpenOptions::new().write(true).open(KMSG_PATH) {
        let _ = writeln!(file, "{}{}{}", kernel_level::INFO, LOG_PREFIX, message);
    }
}
