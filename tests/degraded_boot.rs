//! Integration tests for degraded boot classification and OdsStatus JSON output.

use omnect_os_init::bootloader::{BootEnvDecision, BootEnvKey, BootEnvState, classify_boot_env};
use omnect_os_init::error::{BootEnvError, InitramfsError};
use omnect_os_init::recovery::{Action, RecoveryClass, decide};
use omnect_os_init::runtime::OdsStatus;
use omnect_os_init::{MockBootEnv, update_pending_from_env};

fn make_ok() -> Result<Box<dyn omnect_os_init::bootloader::BootEnv>, BootEnvError> {
    Ok(Box::new(omnect_os_init::bootloader::MockBootEnv::new()))
}

fn make_err() -> Result<Box<dyn omnect_os_init::bootloader::BootEnv>, BootEnvError> {
    Err(BootEnvError::CommandFailed {
        command: "grub-editenv".into(),
        reason: "test error".into(),
    })
}

#[test]
fn ok_result_is_not_degraded_regardless_of_image_type() {
    for is_release in [true, false] {
        let decision = classify_boot_env(make_ok(), is_release);
        assert!(
            matches!(
                decision,
                BootEnvDecision::Continue(BootEnvState::Available(_))
            ),
            "is_release={is_release}: expected Available/not-degraded"
        );
    }
}

#[test]
fn err_release_image_is_degraded_continue() {
    let decision = classify_boot_env(make_err(), true);
    // Use is_degraded() to give the method a regression test (M1/M9).
    match decision {
        BootEnvDecision::Continue(ref env) => {
            assert!(env.is_degraded(), "release-image: expected Degraded env");
        }
        _ => panic!("release-image: expected Continue(Degraded)"),
    }
}

#[test]
fn err_debug_image_is_abort_with_cause() {
    let decision = classify_boot_env(make_err(), false);
    // Verify not just the variant shape but also that the cause is the exact
    // BootEnvError we injected, proving #[source] wiring is in place.
    match decision {
        BootEnvDecision::Abort(InitramfsError::DegradedBoot(ref cause)) => {
            assert!(
                matches!(
                    cause,
                    BootEnvError::CommandFailed {
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
fn degraded_ods_status_json_contains_reason() {
    let mut status = OdsStatus::new();
    assert!(
        !serde_json::to_string(&status)
            .unwrap()
            .contains("degraded_boot")
    );
    status.set_degraded_boot("grubenv missing".to_string());
    let json = serde_json::to_string(&status).unwrap();
    assert!(
        json.contains("\"degraded_boot\""),
        "degraded_boot key must be present when set; got: {json}"
    );
    assert!(
        json.contains("\"reason\":\"grubenv missing\""),
        "reason must be present in JSON; got: {json}"
    );
}

#[test]
fn validate_update_set_produces_reboot_on_fatal() {
    // Anti-brick contract: a Fatal error during an unconfirmed OTA boot must
    // produce Action::Reboot so the bootloader can roll back to the known-good
    // slot. Wires update_pending_from_env → decide end-to-end.
    let bl = MockBootEnv::new().with_env(BootEnvKey::ValidateUpdate, "1");
    let env = BootEnvState::Available(Box::new(bl));
    let update_pending = update_pending_from_env(&env);
    assert!(
        update_pending,
        "ValidateUpdate set → update_pending must be true"
    );

    // Rollback takes priority over both halt (release) and shell (debug).
    assert_eq!(
        decide(RecoveryClass::Fatal, true, update_pending),
        Action::Reboot,
        "Fatal + update_pending + release → Reboot"
    );
    assert_eq!(
        decide(RecoveryClass::Fatal, false, update_pending),
        Action::Reboot,
        "Fatal + update_pending + debug → Reboot"
    );
}

#[test]
fn validate_update_absent_produces_halt_on_fatal_release() {
    // Without an in-flight OTA, a Fatal error on a release image halts
    // (no rollback target to jump to).
    let env = BootEnvState::Available(Box::new(MockBootEnv::new()));
    assert_eq!(
        decide(RecoveryClass::Fatal, true, update_pending_from_env(&env)),
        Action::Halt
    );
}
