//! U-Boot bootloader implementation
//!
//! This module provides access to U-Boot bootloader environment variables
//! using `fw_printenv` and `fw_setenv` commands.

use std::process::Command;

use crate::bootloader::{
    Bootloader, FSCK_VAR_PREFIX, Result,
    types::{decode_fsck_output, encode_fsck_output},
};
use crate::error::BootloaderError;

/// Command to read U-Boot environment variables
const FW_PRINTENV_CMD: &str = "/bin/fw_printenv";

/// Command to write U-Boot environment variables
const FW_SETENV_CMD: &str = "/bin/fw_setenv";

/// Sentinel value used when a process exits due to a signal (no numeric exit code available)
const UNKNOWN_EXIT_CODE: i32 = -1;

/// Exit status of a `fw_printenv` invocation.
///
/// Exit code 1 is a normal condition meaning the variable is not set in
/// the U-Boot environment — it must not be treated as an error.
enum FwPrintenvExitStatus {
    /// Variable found and printed (exit code 0).
    Found,
    /// Variable not set in the environment — not an error (exit code 1).
    NotFound,
    /// Unexpected failure; carries the raw exit code for the error message.
    Error(i32),
}

impl FwPrintenvExitStatus {
    fn from_code(code: i32) -> Self {
        match code {
            0 => Self::Found,
            1 => Self::NotFound,
            n => Self::Error(n),
        }
    }
}

/// U-Boot bootloader implementation
///
/// Uses `fw_printenv` and `fw_setenv` to access environment variables.
/// Fsck status is stored as gzip+base64 encoded `"exit_code\noutput"` string
/// via busybox subprocess commands to survive the reboot required after fsck.
pub struct UBootBootloader {
    // No state needed - commands access environment directly
}

impl UBootBootloader {
    /// Create a new U-Boot bootloader instance.
    ///
    /// Returns `Result<Self>` for API symmetry with `GrubBootloader::new()`,
    /// even though this constructor currently cannot fail.
    pub fn new() -> Result<Self> {
        Ok(Self {})
    }

    /// Run fw_printenv to get a variable
    fn run_fw_printenv(&self, var: &str) -> Result<Option<String>> {
        let output = Command::new(FW_PRINTENV_CMD)
            .arg("-n")
            .arg(var)
            .output()
            .map_err(|e| BootloaderError::CommandFailed {
                command: FW_PRINTENV_CMD.to_string(),
                reason: e.to_string(),
            })?;

        // Exit code 1 means the variable was not found — that is a normal condition.
        // Any other non-zero code indicates a real failure (bad /etc/fw_env.config,
        // I/O error, permission denied, etc.) and must be surfaced as an error.
        if !output.status.success() {
            let code = output.status.code().unwrap_or(UNKNOWN_EXIT_CODE);
            match FwPrintenvExitStatus::from_code(code) {
                FwPrintenvExitStatus::NotFound => return Ok(None),
                FwPrintenvExitStatus::Error(c) => {
                    return Err(BootloaderError::CommandExitCode {
                        command: FW_PRINTENV_CMD.to_string(),
                        code: Some(c),
                        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                    });
                }
                FwPrintenvExitStatus::Found => unreachable!("success() was false but code was 0"),
            }
        }

        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if value.is_empty() {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }

    /// Run fw_setenv to set or unset a variable
    fn run_fw_setenv(&self, var: &str, value: Option<&str>) -> Result<()> {
        let mut cmd = Command::new(FW_SETENV_CMD);
        cmd.arg(var);

        if let Some(v) = value {
            cmd.arg(v);
        }

        let output = cmd.output().map_err(|e| BootloaderError::CommandFailed {
            command: FW_SETENV_CMD.to_string(),
            reason: e.to_string(),
        })?;

        if !output.status.success() {
            return Err(BootloaderError::CommandExitCode {
                command: FW_SETENV_CMD.to_string(),
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        Ok(())
    }
}

impl Bootloader for UBootBootloader {
    fn get_env(&self, key: &str) -> Result<Option<String>> {
        self.run_fw_printenv(key)
    }

    fn set_env(&mut self, key: &str, value: Option<&str>) -> Result<()> {
        self.run_fw_setenv(key, value)
    }

    fn save_fsck_status(&mut self, partition: &str, code: i32, output: &str) -> Result<()> {
        let var_name = format!("{}{}", FSCK_VAR_PREFIX, partition);
        self.run_fw_setenv(&var_name, Some(&encode_fsck_output(code, output)))
    }

    fn get_fsck_status(&self, partition: &str) -> Result<Option<(i32, String)>> {
        let var_name = format!("{}{}", FSCK_VAR_PREFIX, partition);
        Ok(self
            .run_fw_printenv(&var_name)?
            .and_then(|v| decode_fsck_output(&v)))
    }

    fn clear_fsck_status(&mut self, partition: &str) -> Result<()> {
        let var_name = format!("{}{}", FSCK_VAR_PREFIX, partition);
        self.run_fw_setenv(&var_name, None)
    }
}
