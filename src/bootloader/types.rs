//! Common types for bootloader implementations

use std::io::Write as _;
use std::process::{Command, Stdio};

const GZIP_CMD: &str = "/bin/gzip";
const GUNZIP_CMD: &str = "/bin/gunzip";
/// busybox base64 applet lives under /bin, not /usr/bin
const BASE64_CMD: &str = "/bin/base64";

/// Encode fsck result for storage in the bootloader environment.
///
/// Produces `base64(gzip("{code}\n{output}"))` using the busybox `gzip` and
/// `base64` applets that are always present in the initramfs. This matches the
/// legacy bash script encoding so ODS can decode the value identically.
///
/// Returns an empty string if encoding fails (non-fatal; the plain log file
/// on the data partition still captures the output). Note: an empty string
/// stored in the bootloader env is indistinguishable from "no fsck ran" —
/// `get_fsck_status` will return `Ok(None)` for both cases, masking the
/// encoding failure. This is acceptable: the plain log file is the primary
/// diagnostic artifact; the bootloader env value is a lightweight indicator.
pub fn encode_fsck_output(code: i32, output: &str) -> String {
    let raw = format!("{code}\n{output}");

    // Pipe raw text through `gzip -c` to get compressed bytes.
    let gzip_result = (|| -> std::io::Result<Vec<u8>> {
        let mut gzip = Command::new(GZIP_CMD)
            .args(["-c"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;

        let mut stdin = gzip
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("no gzip stdin"))?;
        stdin.write_all(raw.as_bytes())?;
        // Drop stdin to close the pipe; gzip won't flush output until EOF on its input.
        // Without this, wait_with_output() deadlocks if gzip output fills the OS pipe buffer.
        drop(stdin);

        let out = gzip.wait_with_output()?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "gzip exited with status {}",
                out.status
            )));
        }
        Ok(out.stdout)
    })();

    let compressed = match gzip_result {
        Ok(c) => c,
        Err(e) => {
            log::warn!("encode_fsck_output: gzip failed: {e}");
            return String::new();
        }
    };

    // Pipe compressed bytes through `base64 -w 0` (no line wrapping).
    let base64_result = (|| -> std::io::Result<String> {
        let mut b64 = Command::new(BASE64_CMD)
            .args(["-w", "0"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;

        let mut stdin = b64
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("no base64 stdin"))?;
        stdin.write_all(&compressed)?;
        drop(stdin);

        let out = b64.wait_with_output()?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "base64 exited with status {}",
                out.status
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    })();

    match base64_result {
        Ok(s) => s,
        Err(e) => {
            log::warn!("encode_fsck_output: base64 failed: {e}");
            String::new()
        }
    }
}

/// Decode a fsck result previously encoded with [`encode_fsck_output`].
///
/// Returns `(exit_code, output)` on success, or `None` if decoding fails.
pub fn decode_fsck_output(encoded: &str) -> Option<(i32, String)> {
    // Decode base64 → compressed bytes.
    let b64_out = Command::new(BASE64_CMD)
        .args(["-d"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| std::io::Error::other("no stdin"))?;
            stdin.write_all(encoded.as_bytes())?;
            drop(stdin);
            child.wait_with_output()
        })
        .ok()
        .filter(|out| out.status.success())?;

    // Decompress gzip → raw text.
    let gz_out = Command::new(GUNZIP_CMD)
        .args(["-c"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| std::io::Error::other("no stdin"))?;
            stdin.write_all(&b64_out.stdout)?;
            drop(stdin);
            child.wait_with_output()
        })
        .ok()
        .filter(|out| out.status.success())?;

    let raw = String::from_utf8_lossy(&gz_out.stdout);
    let (code_str, output) = raw.split_once('\n')?;
    let code = code_str.trim().parse::<i32>().ok()?;
    Some((code, output.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns true if all required external commands are available at their
    /// expected initramfs paths. Tests that invoke subprocesses are skipped
    /// on developer/CI machines where these may live elsewhere (e.g. /usr/bin).
    fn commands_available() -> bool {
        [GZIP_CMD, GUNZIP_CMD, BASE64_CMD]
            .iter()
            .all(|cmd| std::path::Path::new(cmd).exists())
    }

    #[test]
    fn test_encode_decode_roundtrip() {
        assert!(
            commands_available(),
            "required commands not found at initramfs paths ({}, {}, {}); \
             ensure gzip, gunzip and base64 are installed",
            GZIP_CMD,
            GUNZIP_CMD,
            BASE64_CMD
        );
        let code = 1;
        let output = "Pass 1: Checking inodes, blocks, and sizes\nErrors corrected.";
        let encoded = encode_fsck_output(code, output);
        assert!(!encoded.is_empty(), "encoding should succeed");
        let (dec_code, dec_output) = decode_fsck_output(&encoded).unwrap();
        assert_eq!(dec_code, code);
        assert_eq!(dec_output, output);
    }

    #[test]
    fn test_decode_invalid_returns_none() {
        assert!(decode_fsck_output("not-valid-base64!!!").is_none());
        assert!(decode_fsck_output("").is_none());
    }
}
