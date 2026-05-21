//! Integration tests for degraded boot classification and OdsStatus JSON output.

use omnect_os_init::bootloader::{BootloaderDecision, BootloaderEnv, classify_bootloader};
use omnect_os_init::error::{BootloaderError, InitramfsError};
use omnect_os_init::runtime::OdsStatus;

fn make_ok() -> Result<Box<dyn omnect_os_init::bootloader::Bootloader>, BootloaderError> {
    Ok(Box::new(omnect_os_init::bootloader::MockBootloader::new()))
}

fn make_err() -> Result<Box<dyn omnect_os_init::bootloader::Bootloader>, BootloaderError> {
    Err(BootloaderError::CommandFailed {
        command: "grub-editenv".into(),
        reason: "test error".into(),
    })
}

#[test]
fn ok_result_is_not_degraded_regardless_of_image_type() {
    for is_release in [true, false] {
        let decision = classify_bootloader(make_ok(), is_release);
        assert!(
            matches!(
                decision,
                BootloaderDecision::Continue(BootloaderEnv::Available(_), false)
            ),
            "is_release={is_release}: expected Available/not-degraded"
        );
    }
}

#[test]
fn err_release_image_is_degraded_continue() {
    let decision = classify_bootloader(make_err(), true);
    assert!(
        matches!(
            decision,
            BootloaderDecision::Continue(BootloaderEnv::Degraded(_), true)
        ),
        "release-image: expected Degraded/true"
    );
}

#[test]
fn err_debug_image_is_abort_with_cause() {
    let decision = classify_bootloader(make_err(), false);
    // Verify not just the variant shape but also that the cause is the exact
    // BootloaderError we injected, proving #[source] wiring is in place.
    match decision {
        BootloaderDecision::Abort(InitramfsError::DegradedBoot(ref cause)) => {
            assert!(
                matches!(
                    cause,
                    BootloaderError::CommandFailed {
                        command,
                        ..
                    } if command == "grub-editenv"
                ),
                "expected cause CommandFailed(grub-editenv), got: {cause:?}"
            );
        }
        _ => panic!("debug-image: expected Abort(DegradedBoot), got unexpected decision"),
    }
}

#[test]
fn degraded_ods_status_json_contains_flag() {
    let mut status = OdsStatus::new();
    assert!(
        !serde_json::to_string(&status)
            .unwrap()
            .contains("degraded_boot")
    );
    status.set_degraded_boot();
    let json = serde_json::to_string(&status).unwrap();
    assert!(
        json.contains("\"degraded_boot\":true"),
        "expected degraded_boot:true in JSON, got: {json}"
    );
}
