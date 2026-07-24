//! Boot failure & recovery policy.
//!
//! Pure decision logic mapping an error's `RecoveryClass` and the boot
//! context to an `Action`. Action *execution* (reboot/halt/shell) lives
//! in `main.rs`; this module has no I/O.
//!
//! Spec: docs/superpowers/specs/2026-05-27-boot-failure-recovery-policy-design.md

/// How an `InitramfsError` is meant to be recovered from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryClass {
    /// Non-fatal; the caller should warn and proceed.
    ContinueDegraded,
    /// A reboot is expected to change the outcome (e.g. fsck applied a fix, or
    /// extra-bootargs was written on first boot). Not bounded by the bootloader:
    /// a cause that never clears reboot-loops, an accepted residual risk.
    RebootToApply,
    /// Boot cannot proceed.
    Fatal,
}

/// What `handle_fatal_error` should do for a given class + context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Error was already handled by the caller; no further action needed.
    Continue,
    /// Reboot the device. Reasons: OTA-rollback (Fatal + update_pending),
    /// fsck reboot-required, or extra-bootargs applied on first boot. The OTA
    /// case is bounded by the bootloader; fsck and extra-bootargs are
    /// unconditional accepted-risk reboot loops.
    Reboot,
    /// Infinite loop with kmsg logging; never exits PID 1.
    Halt,
    /// Spawn an interactive debug shell.
    Shell,
}

/// Decide the action for a recovery class and boot context.
///
/// `update_pending` is `true` if `omnect_validate_update` was set in the
/// boot env at the time it was opened. When the env is unreadable or the
/// failure occurred before the env was opened, `update_pending` is `false`.
///
/// `is_release` is `true` when the `release-image` feature is compiled in.
#[must_use = "the returned Action must be executed"]
pub fn decide(class: RecoveryClass, is_release: bool, update_pending: bool) -> Action {
    match class {
        RecoveryClass::ContinueDegraded => Action::Continue,
        RecoveryClass::RebootToApply => Action::Reboot,
        RecoveryClass::Fatal => {
            if update_pending {
                Action::Reboot
            } else if is_release {
                Action::Halt
            } else {
                Action::Shell
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continue_degraded_always_continues() {
        for &is_release in &[false, true] {
            for &update in &[false, true] {
                assert_eq!(
                    decide(RecoveryClass::ContinueDegraded, is_release, update),
                    Action::Continue,
                    "ContinueDegraded -> Continue regardless of context (is_release={is_release}, update={update})"
                );
            }
        }
    }

    #[test]
    fn reboot_to_apply_always_reboots() {
        for &is_release in &[false, true] {
            for &update in &[false, true] {
                assert_eq!(
                    decide(RecoveryClass::RebootToApply, is_release, update),
                    Action::Reboot,
                    "RebootToApply -> Reboot regardless of context"
                );
            }
        }
    }

    #[test]
    fn fatal_with_update_pending_reboots() {
        // Anti-brick contract: a Fatal error during an unconfirmed OTA-update
        // boot reboots so the bootloader can roll back to the known-good slot.
        assert_eq!(decide(RecoveryClass::Fatal, true, true), Action::Reboot);
        assert_eq!(decide(RecoveryClass::Fatal, false, true), Action::Reboot);
    }

    #[test]
    fn fatal_release_halts() {
        assert_eq!(decide(RecoveryClass::Fatal, true, false), Action::Halt);
    }

    #[test]
    fn fatal_debug_shells() {
        assert_eq!(decide(RecoveryClass::Fatal, false, false), Action::Shell);
    }
}
