//! Kernel message logging via /dev/kmsg
//!
//! This module provides a `log` crate compatible logger that writes
//! to /dev/kmsg with proper kernel log levels.

use crate::error::LoggingError;
use log::{Level, LevelFilter, Log, Metadata, Record, SetLoggerError};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

/// Log level prefixes for kernel messages
/// See: https://www.kernel.org/doc/html/latest/core-api/printk-basics.html
const KERN_EMERG: &str = "<0>";   // System is unusable
const KERN_ALERT: &str = "<1>";   // Action must be taken immediately
const KERN_CRIT: &str = "<2>";    // Critical conditions
const KERN_ERR: &str = "<3>";     // Error conditions
const KERN_WARNING: &str = "<4>"; // Warning conditions
const KERN_NOTICE: &str = "<5>";  // Normal but significant condition
const KERN_INFO: &str = "<6>";    // Informational
const KERN_DEBUG: &str = "<7>";   // Debug-level messages

/// Prefix for all log messages
const LOG_PREFIX: &str = "omnect-os-init: ";

/// Logger that writes to /dev/kmsg
pub struct KmsgLogger {
    kmsg: Mutex<File>,
}

impl KmsgLogger {
    /// Create a new KmsgLogger
    fn new() -> Result<Self, LoggingError> {
        let file = OpenOptions::new()
            .write(true)
            .open("/dev/kmsg")
            .map_err(LoggingError::KmsgOpenFailed)?;

        Ok(Self {
            kmsg: Mutex::new(file),
        })
    }

    /// Initialize the global logger
    ///
    /// This should be called once at the start of the program.
    /// Returns an error if the logger is already initialized or
    /// if /dev/kmsg cannot be opened.
    pub fn init() -> Result<(), LoggingError> {
        let logger = Self::new()?;
        Self::init_with_logger(logger)
    }

    /// Initialize with a custom logger instance (useful for testing)
    fn init_with_logger(logger: Self) -> Result<(), LoggingError> {
        log::set_boxed_logger(Box::new(logger))
            .map_err(|e: SetLoggerError| LoggingError::InitFailed(e.to_string()))?;
        log::set_max_level(LevelFilter::Info);
        Ok(())
    }

    /// Convert log level to kernel log level prefix
    fn level_to_kern(level: Level) -> &'static str {
        match level {
            Level::Error => KERN_ERR,
            Level::Warn => KERN_WARNING,
            Level::Info => KERN_INFO,
            Level::Debug => KERN_DEBUG,
            Level::Trace => KERN_DEBUG,
        }
    }

    /// Write a message to kmsg with the given level
    fn write_kmsg(&self, level: Level, message: &str) {
        if let Ok(mut kmsg) = self.kmsg.lock() {
            let kern_level = Self::level_to_kern(level);
            // Write each line separately to kmsg
            for line in message.lines() {
                let _ = writeln!(kmsg, "{}{}{}", kern_level, LOG_PREFIX, line);
            }
        }
    }
}

impl Log for KmsgLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            self.write_kmsg(record.level(), &record.args().to_string());
        }
    }

    fn flush(&self) {
        if let Ok(mut kmsg) = self.kmsg.lock() {
            let _ = kmsg.flush();
        }
    }
}

/// Write a fatal error message directly to kmsg
///
/// This is useful when the logger hasn't been initialized yet
/// or when we need to ensure a message is written before a crash.
pub fn write_fatal(message: &str) {
    if let Ok(mut file) = OpenOptions::new().write(true).open("/dev/kmsg") {
        let _ = writeln!(file, "{}{}FATAL: {}", KERN_CRIT, LOG_PREFIX, message);
    }
}

/// Write an info message directly to kmsg (bypassing the log framework)
pub fn write_info(message: &str) {
    if let Ok(mut file) = OpenOptions::new().write(true).open("/dev/kmsg") {
        let _ = writeln!(file, "{}{}{}", KERN_INFO, LOG_PREFIX, message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_level_to_kern() {
        assert_eq!(KmsgLogger::level_to_kern(Level::Error), KERN_ERR);
        assert_eq!(KmsgLogger::level_to_kern(Level::Warn), KERN_WARNING);
        assert_eq!(KmsgLogger::level_to_kern(Level::Info), KERN_INFO);
        assert_eq!(KmsgLogger::level_to_kern(Level::Debug), KERN_DEBUG);
        assert_eq!(KmsgLogger::level_to_kern(Level::Trace), KERN_DEBUG);
    }

    #[test]
    fn test_log_prefix() {
        assert_eq!(LOG_PREFIX, "omnect-os-init: ");
    }

    #[test]
    fn test_kern_levels() {
        // Verify kernel log level format
        assert!(KERN_ERR.starts_with('<'));
        assert!(KERN_ERR.ends_with('>'));
        assert_eq!(KERN_ERR.len(), 3);
    }
}
