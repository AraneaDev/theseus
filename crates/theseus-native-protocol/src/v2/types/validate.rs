//! Local validation of messages and outcomes.
use super::super::{Error, MAX_FILE_BYTES};
use super::{
    AcquireView, Cleanup, LaunchPhase, LeaseView, MAX_NATIVE_FILE_BYTES, Message, NativeStatus,
    OperationView, Os, Outcome, Principal, ProgramId, Reply, Request, SpawnCleanup,
};
fn bounded_length(length: u16) -> Result<(), Error> {
    if usize::from(length) > MAX_FILE_BYTES {
        Err(Error::LimitExceeded)
    } else {
        Ok(())
    }
}
fn checked_end(offset: u64, length: usize) -> Result<(), Error> {
    offset
        .checked_add(length as u64)
        .map(|_| ())
        .ok_or(Error::InvalidValue)
}
pub(super) fn validate_message(v: &Message) -> Result<(), Error> {
    match v {
        Message::Request {
            request: Request::PrepareOwnedEntry { preparation },
        } => preparation.validate(),
        Message::Request {
            request: Request::InspectOwnedEntry {
                generation, sut, ..
            },
        } => {
            if let Some(sut) = sut {
                sut.validate()?;
            }
            if let Some(generation) = generation {
                generation.validate()?;
                if Some(&generation.sut) != sut.as_deref() {
                    return Err(Error::InvalidValue);
                }
            }
            Ok(())
        }
        Message::Request {
            request: Request::BrokerBind { original } | Request::QueryBrokerBind { original },
        } => original.validate(),
        Message::Request {
            request: Request::DesktopAction { request },
        } => request.validate(),
        Message::Request {
            request: Request::QueryDesktop { identity } | Request::CancelDesktop { identity },
        } => identity.validate(),
        Message::Request {
            request:
                Request::ProbeDesktopReadiness {
                    identity,
                    nonce,
                    deadline_ms,
                },
        } => {
            identity.validate()?;
            if nonce.as_bytes() == &[0; 16] || !(1..=30000).contains(deadline_ms) {
                return Err(Error::InvalidValue);
            }
            Ok(())
        }
        Message::Request {
            request: Request::ReadFile { offset, length, .. },
        } => {
            bounded_length(*length)?;
            if *length == 0 {
                return Err(Error::InvalidValue);
            }
            checked_end(*offset, usize::from(*length))
        }
        Message::Request {
            request:
                Request::ProbeReadiness {
                    deadline_ms,
                    definition_role,
                    artifact_role,
                    ..
                },
        } => {
            if !(1..=30_000).contains(deadline_ms)
                || !["base", "configured", "target"].contains(&definition_role.as_str())
                || !["base", "target"].contains(&artifact_role.as_str())
            {
                return Err(Error::InvalidValue);
            }
            Ok(())
        }
        Message::Request {
            request:
                Request::InspectDesktop {
                    generation: Some(generation),
                    ..
                },
        } => generation.validate(),
        Message::Response { result, .. } => match result {
            Reply::OwnedEntryPrepared { preparation } => preparation.validate(),
            Reply::BrokerDescription { header } => header.validate(),
            Reply::BrokerBinding { receipt } => receipt.validate(),
            Reply::DesktopState { identity, state } => {
                identity.validate()?;
                state.validate()
            }
            Reply::DesktopObservation { observation, .. } => observation.validate(),
            Reply::DesktopReadiness {
                identity,
                observation,
            } => observation.validate_desktop(identity),
            Reply::Description {
                os,
                helper_principal,
                ..
            } => match (os, helper_principal) {
                (Os::Windows, Principal::Windows { .. })
                | (Os::Linux | Os::Macos, Principal::Posix { .. }) => Ok(()),
                _ => Err(Error::InvalidValue),
            },
            Reply::Acquired {
                intent,
                state: AcquireView::Lease { state, .. },
                ..
            } => {
                let nested_intent = match state {
                    LeaseView::Held { intent }
                    | LeaseView::AwaitingExternalReset { intent }
                    | LeaseView::Quarantined { intent, .. } => intent,
                };
                if intent != nested_intent {
                    return Err(Error::InvalidValue);
                }
                Ok(())
            }
            Reply::Operation {
                state: OperationView::Finished { outcome },
                ..
            } => validate_outcome(outcome),
            Reply::FileBytes {
                offset, bytes, eof, ..
            } => {
                if bytes.as_slice().is_empty() && !eof {
                    return Err(Error::InvalidValue);
                }
                checked_end(*offset, bytes.as_slice().len())
            }
            Reply::NativeDescription { description } => {
                const IDS: [&str; 8] = [
                    "check_base",
                    "check_data",
                    "check_target",
                    "configure",
                    "exercise",
                    "install_base",
                    "install_target",
                    "uninstall",
                ];
                if !description
                    .programs
                    .as_slice()
                    .iter()
                    .map(ProgramId::as_str)
                    .eq(IDS)
                {
                    return Err(Error::InvalidValue);
                }
                match (&description.os, &description.helper_principal) {
                    (Os::Windows, Principal::Windows { .. })
                    | (Os::Linux | Os::Macos, Principal::Posix { .. }) => Ok(()),
                    _ => Err(Error::InvalidValue),
                }
            }
            Reply::NativeFileInfo { length, .. } if *length > MAX_NATIVE_FILE_BYTES => {
                Err(Error::LimitExceeded)
            }
            Reply::FileInfo { length, .. } => bounded_length(*length),
            _ => Ok(()),
        },
        Message::Output { offset, bytes, .. } => {
            if bytes.as_slice().is_empty() {
                return Err(Error::InvalidValue);
            }
            checked_end(*offset, bytes.as_slice().len())
        }
        _ => Ok(()),
    }
}
/// Checks only facts carried by the outcome, without assuming a session OS.
pub(crate) fn validate_outcome(v: &Outcome) -> Result<(), Error> {
    // Infer an OS family only to reuse local validation. This establishes no session OS.
    let os = match v {
        Outcome::RunCompleted {
            status: NativeStatus::WindowsExit { .. },
            ..
        } => Os::Windows,
        Outcome::LaunchFailed {
            cleanup: SpawnCleanup::WindowsProcessExitedJobEmpty,
            ..
        } => Os::Windows,
        _ => Os::Linux,
    };
    validate_outcome_for_os(v, os)
}

/// Checks local outcome facts and their agreement with the independently fixed OS.
pub(crate) fn validate_outcome_for_os(v: &Outcome, os: Os) -> Result<(), Error> {
    let native_windows = match v {
        Outcome::RunCompleted { status, .. } => {
            Some(matches!(status, NativeStatus::WindowsExit { .. }))
        }
        Outcome::LaunchFailed {
            cleanup: SpawnCleanup::NoChildCreated,
            ..
        } => None,
        Outcome::LaunchFailed { cleanup, .. } => Some(matches!(
            cleanup,
            SpawnCleanup::WindowsProcessExitedJobEmpty
        )),
        _ => None,
    };
    if native_windows.is_some_and(|windows| windows != (os == Os::Windows)) {
        return Err(Error::InvalidValue);
    }
    match v {
        Outcome::FileCreated { length, .. } => bounded_length(*length),
        Outcome::RunCompleted {
            status, cleanup, ..
        } => {
            match status {
                NativeStatus::PosixSignal { signal } if !(1..=255).contains(signal) => {
                    return Err(Error::InvalidValue);
                }
                _ => {}
            }
            match (status, cleanup) {
                (NativeStatus::WindowsExit { .. }, Cleanup::WindowsProcessExitedJobEmpty)
                | (
                    NativeStatus::PosixExit { .. } | NativeStatus::PosixSignal { .. },
                    Cleanup::PosixLeaderReapedGroupTerminationRequested,
                ) => Ok(()),
                _ => Err(Error::InvalidValue),
            }
        }
        Outcome::LaunchFailed { phase, cleanup, .. } => match (phase, cleanup) {
            (LaunchPhase::NeverCreated, SpawnCleanup::NoChildCreated)
            | (LaunchPhase::CreatedSuspended, SpawnCleanup::WindowsProcessExitedJobEmpty)
            | (
                LaunchPhase::MayRun,
                SpawnCleanup::PosixLeaderReapedGroupTerminationRequested
                | SpawnCleanup::WindowsProcessExitedJobEmpty,
            ) => Ok(()),
            _ => Err(Error::InvalidValue),
        },
        _ => Ok(()),
    }
}
