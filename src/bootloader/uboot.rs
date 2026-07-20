//! U-Boot boot environment implementation
//!
//! This module provides access to U-Boot bootloader environment variables
//! using `fw_printenv` and `fw_setenv` commands.

use std::process::Command;

use crate::bootloader::{BootEnv, BootEnvKey, Result};
use crate::error::BootEnvError;

/// Command to read U-Boot environment variables
const FW_PRINTENV_CMD: &str = "/bin/fw_printenv";

/// Command to write U-Boot environment variables
const FW_SETENV_CMD: &str = "/bin/fw_setenv";

/// U-Boot boot environment implementation
///
/// Uses `fw_printenv` and `fw_setenv` to access environment variables.
/// Fsck status encoding is handled by the default `BootEnv` trait methods.
pub struct UBootBootEnv {}

impl UBootBootEnv {
    /// Create a new U-Boot boot environment accessor.
    ///
    /// Returns `Result<Self>` for API symmetry with `GrubBootEnv::new()`,
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
            .map_err(|e| BootEnvError::CommandFailed {
                command: FW_PRINTENV_CMD.to_string(),
                reason: e.to_string(),
            })?;

        // Match on exit code directly so every case is explicit.
        // Exit code 1 means "variable not set" — a normal condition in U-Boot env.
        // None means the process was killed by a signal.
        match output.status.code() {
            Some(0) => {}
            Some(1) => return Ok(None),
            code => {
                return Err(BootEnvError::CommandExitCode {
                    command: FW_PRINTENV_CMD.to_string(),
                    code,
                    stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                });
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

        let output = cmd.output().map_err(|e| BootEnvError::CommandFailed {
            command: FW_SETENV_CMD.to_string(),
            reason: e.to_string(),
        })?;

        if !output.status.success() {
            return Err(BootEnvError::CommandExitCode {
                command: FW_SETENV_CMD.to_string(),
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        Ok(())
    }
}

impl BootEnv for UBootBootEnv {
    fn get_env(&self, key: BootEnvKey) -> Result<Option<String>> {
        self.run_fw_printenv(key.as_str().as_ref())
    }

    fn set_env(&mut self, key: BootEnvKey, value: Option<&str>) -> Result<()> {
        self.run_fw_setenv(key.as_str().as_ref(), value)
    }
}
