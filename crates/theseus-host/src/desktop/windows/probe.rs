use super::invalid;
use crate::desktop::owned::{CommittedReadinessProbe, ExecutableBinding};
use std::io;
pub(super) fn validate_static_bootstrap(
    arguments: &[String],
    ready: &str,
    close: &str,
) -> io::Result<()> {
    if ready == close {
        return Err(invalid("separate committed native channels required"));
    }
    for key in ["--theseus-instance", "--theseus-activation-nonce"] {
        if arguments
            .iter()
            .any(|arg| arg == key || arg.starts_with(&format!("{key}=")))
        {
            return Err(invalid(
                "per-action native identity belongs in the original-object authenticated challenge, not installed argv",
            ));
        }
    }
    for (key, value) in [
        ("--theseus-bootstrap", "theseus_fixture_ready_bootstrap_v1"),
        ("--theseus-ready", ready),
        ("--theseus-close", close),
    ] {
        let expected = format!("{key}={value}");
        if arguments
            .iter()
            .filter(|arg| arg.as_str() == key || arg.starts_with(&format!("{key}=")))
            .count()
            != 1
            || !arguments.contains(&expected)
        {
            return Err(invalid(
                "native argv lacks exact closed bootstrap/channel commitment",
            ));
        }
    }
    Ok(())
}
pub(super) fn pin_poll_deadline(
    original: &mut Option<std::time::Instant>,
    deadline: std::time::Instant,
) -> io::Result<()> {
    if deadline > std::time::Instant::now() + std::time::Duration::from_secs(30)
        || original.is_some_and(|old| old != deadline)
    {
        return Err(invalid(
            "native probe must preserve its original bounded semantic deadline",
        ));
    }
    *original = Some(deadline);
    Ok(())
}
pub(super) enum MessageTurn {
    Idle,
    Dispatched,
    Quit,
}
pub(super) fn pump_turn(mut next: impl FnMut() -> MessageTurn) -> io::Result<()> {
    for _ in 0..32 {
        match next() {
            MessageTurn::Idle => return Ok(()),
            MessageTurn::Quit => {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "original COM apartment received WM_QUIT",
                ));
            }
            MessageTurn::Dispatched => {}
        }
    }
    Ok(())
}
/// Original-instance authentication and application facts are independent.
/// Preserve Failed readiness verbatim while allowing only its authenticated
/// original instance to receive the separately committed cooperative close.
pub(super) fn authenticate_observed_instance(
    progress: &mut super::ActivationProgress,
    observed: &crate::services::readiness::ReadinessObservation,
) -> io::Result<()> {
    use crate::services::readiness::ReadinessFailure;
    if matches!(
        observed.failure,
        Some(
            ReadinessFailure::WrongPeer
                | ReadinessFailure::WrongNonce
                | ReadinessFailure::WrongInstance
        )
    ) {
        progress.authenticate_new_instance(false)?;
    } else if observed.authenticated_instance {
        progress.authenticate_new_instance(true)?;
    }
    Ok(())
}
pub(super) fn validate_fresh_probe(
    original: &ExecutableBinding,
    next: &CommittedReadinessProbe,
    used: &[[u8; 16]],
) -> io::Result<()> {
    crate::services::readiness::encode_challenge(&next.challenge)?;
    let c = &next.challenge;
    let old = &original.ready;
    if c.protocol != old.protocol
        || c.lease != old.lease
        || c.slot != old.slot
        || c.resource_binding_digest != old.resource_binding_digest
        || c.start_or_activation_operation != old.start_or_activation_operation
        || c.action_digest != old.action_digest
        || c.expectation_digest == [0; 32]
        || c.nonce == [0; 16]
        || c.nonce == original.close.nonce
        || used.contains(&c.nonce)
        || used.len() >= 64
        || next.expectation.version_literal.is_empty()
        || next.expectation.version_literal.len() > 256
        || next.expectation.version_literal.as_bytes().contains(&0)
    {
        return Err(invalid(
            "fresh native probe must preserve original action and use a fresh committed nonce",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod polling_tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[test]
    fn native_polling_windows_static_bootstrap_matches_committed_pipe_argv() {
        let ready = r"\\.\pipe\theseus.ready";
        let close = r"\\.\pipe\theseus.close";
        let mut args = vec![
            "--theseus-bootstrap=theseus_fixture_ready_bootstrap_v1".into(),
            format!("--theseus-ready={ready}"),
            format!("--theseus-close={close}"),
        ];
        validate_static_bootstrap(&args, ready, close).unwrap();
        args.push("--theseus-activation-nonce=action-specific".into());
        assert!(validate_static_bootstrap(&args, ready, close).is_err());
        args.pop();
        args[1] = "--theseus-ready=foreign".into();
        assert!(validate_static_bootstrap(&args, ready, close).is_err());
    }
    #[test]
    fn native_polling_windows_preserves_original_deadline_across_yields() {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut original = None;
        pin_poll_deadline(&mut original, deadline).unwrap();
        assert!(pin_poll_deadline(&mut original, deadline + Duration::from_secs(1)).is_err());
        assert_eq!(original, Some(deadline));
    }
    #[test]
    fn native_polling_sta_turn_is_bounded_and_quit_is_an_error() {
        let mut count = 0;
        pump_turn(|| {
            count += 1;
            MessageTurn::Dispatched
        })
        .unwrap();
        assert_eq!(count, 32);
        assert_eq!(
            pump_turn(|| MessageTurn::Quit).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        let mut idle = 0;
        pump_turn(|| {
            idle += 1;
            MessageTurn::Idle
        })
        .unwrap();
        assert_eq!(idle, 1);
    }
}
