mod codes;
mod description;
mod ids;
mod messages;
mod validate;

pub use codes::*;
pub use description::*;
pub use ids::*;
pub use messages::*;
pub(crate) use validate::{validate_outcome, validate_outcome_for_os};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_native_facts_match_the_fixed_os() {
        for os in [Os::Linux, Os::Macos, Os::Windows] {
            for (status, cleanup, windows) in [
                (
                    NativeStatus::PosixExit { code: 255 },
                    Cleanup::PosixLeaderReapedGroupTerminationRequested,
                    false,
                ),
                (
                    NativeStatus::PosixSignal { signal: 255 },
                    Cleanup::PosixLeaderReapedGroupTerminationRequested,
                    false,
                ),
                (
                    NativeStatus::WindowsExit { code: u32::MAX },
                    Cleanup::WindowsProcessExitedJobEmpty,
                    true,
                ),
            ] {
                let outcome = Outcome::RunCompleted {
                    reason: RunReason::Natural,
                    status,
                    cleanup,
                    stdout_bytes: 0,
                    stderr_bytes: 0,
                    truncated: false,
                };
                assert_eq!(
                    validate_outcome_for_os(&outcome, os).is_ok(),
                    (os == Os::Windows) == windows
                );
            }
            for (phase, cleanup, windows) in [
                (
                    LaunchPhase::NeverCreated,
                    SpawnCleanup::NoChildCreated,
                    None,
                ),
                (
                    LaunchPhase::MayRun,
                    SpawnCleanup::PosixLeaderReapedGroupTerminationRequested,
                    Some(false),
                ),
                (
                    LaunchPhase::MayRun,
                    SpawnCleanup::WindowsProcessExitedJobEmpty,
                    Some(true),
                ),
                (
                    LaunchPhase::CreatedSuspended,
                    SpawnCleanup::WindowsProcessExitedJobEmpty,
                    Some(true),
                ),
            ] {
                let outcome = Outcome::LaunchFailed {
                    phase,
                    cleanup,
                    code: FailureCode::NativeFailure,
                };
                assert_eq!(
                    validate_outcome_for_os(&outcome, os).is_ok(),
                    windows.is_none_or(|windows| (os == Os::Windows) == windows)
                );
            }
            assert!(
                validate_outcome_for_os(
                    &Outcome::NotStarted {
                        reason: NotStartedReason::Deadline
                    },
                    os
                )
                .is_ok()
            );
        }
        assert!(
            validate_outcome_for_os(
                &Outcome::RunCompleted {
                    reason: RunReason::Natural,
                    status: NativeStatus::PosixSignal { signal: 0 },
                    cleanup: Cleanup::PosixLeaderReapedGroupTerminationRequested,
                    stdout_bytes: 0,
                    stderr_bytes: 0,
                    truncated: false
                },
                Os::Linux
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod integration_manifest_tests {
    use super::*;
    #[test]
    fn full_integration_manifest_is_domain_separated_and_ordered() {
        let core = Digest::parse(&"02".repeat(32)).unwrap();
        let integration = Digest::parse(&"11".repeat(32)).unwrap();
        assert_eq!(
            integration_manifest_digest(core, integration).to_hex(),
            "cd64ea4df93a58dc89771b1f60cd2b829e3e45e0bdb78650ee6a672cd7971d3c"
        );
        assert_ne!(
            integration_manifest_digest(integration, core),
            integration_manifest_digest(core, integration)
        );
    }
}
