//! Shared native routing ceilings and conservative original-operation projection.
use std::{io, time::Instant};
use theseus_native_protocol::v2::{NativeDesktopDefinition, broker::*};
pub(crate) fn initial_lifetime(
    definition: &NativeDesktopDefinition,
) -> io::Result<NativeLifetimeState> {
    let state = NativeLifetimeState {
        authority: definition.authority(),
        required: definition.lifetime(),
        instance: NativeInstanceState::Pending,
        primary: if definition.authority() == NativeLifetimeAuthority::ManagerInstance {
            NativePrimaryState::Unavailable
        } else {
            NativePrimaryState::Pending
        },
        descendants: NativeTreeState::Unavailable,
        local_resources: NativeLocalState::Retained,
    };
    definition
        .validate_state(&state)
        .map_err(io::Error::other)?;
    Ok(state)
}
pub(crate) fn advance_lifetime(
    old: &NativeLifetimeState,
    next: NativeLifetimeState,
) -> io::Result<NativeLifetimeState> {
    next.validate_progress_from(old).map_err(io::Error::other)?;
    Ok(next)
}
pub(crate) fn native_return_timely(deadline: Instant, returned: Instant, cancelled: bool) -> bool {
    returned < deadline && !cancelled
}
pub(crate) fn validate_static_bootstrap(
    manifest: &theseus_native_protocol::v2::IntegrationManifest,
) -> io::Result<()> {
    let desktop = manifest
        .desktop
        .as_ref()
        .ok_or_else(|| io::Error::other("desktop missing"))?;
    let executable = manifest
        .desktop_executable
        .as_ref()
        .ok_or_else(|| io::Error::other("executable missing"))?;
    validate_bootstrap_parts(desktop, executable)
}
pub(crate) fn validate_bootstrap_tuple(
    tuple: &theseus_native_protocol::v2::DesktopDefinition,
) -> io::Result<()> {
    validate_bootstrap_parts(&tuple.desktop, &tuple.executable)
}
fn validate_bootstrap_parts(
    desktop: &DesktopManifest,
    executable: &theseus_native_protocol::v2::DesktopExecutable,
) -> io::Result<()> {
    let endpoint = |e: &theseus_native_protocol::v2::LocalEndpoint| match e {
        theseus_native_protocol::v2::LocalEndpoint::UnixPath { path }
        | theseus_native_protocol::v2::LocalEndpoint::NamedPipe { path } => path.clone(),
    };
    let flags = [
        (
            "--theseus-bootstrap=",
            "theseus_fixture_ready_bootstrap_v1".to_owned(),
        ),
        ("--theseus-ready=", endpoint(&desktop.ready_endpoint)),
        ("--theseus-close=", endpoint(&desktop.close_endpoint)),
    ];
    for (prefix, expected) in flags {
        let mut values = executable
            .arguments
            .iter()
            .filter_map(|arg| arg.strip_prefix(prefix));
        if values.next() != Some(expected.as_str()) || values.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "exact closed bootstrap/endpoint argv required",
            ));
        }
    }
    if executable.arguments.iter().any(|arg| {
        arg == "--theseus-instance"
            || arg.starts_with("--theseus-instance=")
            || arg == "--theseus-activation-nonce"
            || arg.starts_with("--theseus-activation-nonce=")
    }) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "per-action native argv prohibited",
        ));
    }
    Ok(())
}
pub(crate) fn original_probe_deadline(
    held: &mut Option<([u8; 16], Instant)>,
    nonce: [u8; 16],
    proposed: Instant,
) -> Instant {
    if let Some((original, deadline)) = held
        && *original == nonce
    {
        return *deadline;
    }
    *held = Some((nonce, proposed));
    proposed
}
pub(crate) fn initial_native_sample(
    native: bool,
    current: [u8; 16],
    requested: [u8; 16],
    sample: &Option<theseus_host::services::readiness::ReadinessObservation>,
) -> Option<theseus_host::services::readiness::ReadinessObservation> {
    if native && current == requested {
        sample.clone()
    } else {
        None
    }
}
// This callback boundary lets the existing runtime poll its original retained
// owner without constructing or transferring any native capability.
pub(crate) fn poll_original_activation<T>(
    original: Option<Instant>,
    proposed: Instant,
    poll: impl FnOnce(Instant) -> io::Result<T>,
) -> io::Result<T> {
    let _ = proposed;
    let original = original.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "original activation deadline unavailable",
        )
    })?;
    poll(original)
}
#[cfg(test)]
mod tests {
    use super::*;
    use theseus_native_protocol::v2::{NativeBootstrap, NativeDesktopDefinition};
    fn definition() -> NativeDesktopDefinition {
        NativeDesktopDefinition::WindowsShortcut {
            bootstrap: NativeBootstrap::TheseusFixtureReadyBootstrapV1,
            lifetime: NativeLifetimeRequirement::OwnedPrimaryExited,
        }
    }
    #[test]
    fn native_tree_is_refused_before_preparation() {
        let mut definition = definition();
        assert!(initial_lifetime(&definition).is_ok());
        if let NativeDesktopDefinition::WindowsShortcut { lifetime, .. } = &mut definition {
            *lifetime = NativeLifetimeRequirement::OwnedProcessTreeSettled;
        }
        assert!(initial_lifetime(&definition).is_err());
    }
    #[test]
    fn unknown_lifetime_cannot_become_unavailable_or_success() {
        let mut old = initial_lifetime(&definition()).unwrap();
        old.primary = NativePrimaryState::Unknown;
        let mut next = old.clone();
        next.primary = NativePrimaryState::Unavailable;
        assert!(advance_lifetime(&old, next).is_err());
        let mut next = old.clone();
        next.primary = NativePrimaryState::Exited;
        assert!(advance_lifetime(&old, next).is_err());
    }
    #[test]
    fn local_retirement_does_not_satisfy_required_primary() {
        let mut state = initial_lifetime(&definition()).unwrap();
        state.local_resources = NativeLocalState::Retired;
        assert!(!state.required_lifetime_satisfied());
        assert!(state.local_retired());
    }
    #[test]
    fn late_native_return_stays_unresolved_under_original_deadline() {
        assert!(!native_return_timely(
            std::time::Instant::now(),
            std::time::Instant::now(),
            false
        ));
        let now = std::time::Instant::now();
        assert!(native_return_timely(
            now + std::time::Duration::from_secs(30),
            now,
            false
        ));
        assert!(!native_return_timely(
            now + std::time::Duration::from_secs(30),
            now,
            true
        ));
    }
    #[test]
    fn repeated_native_nonce_retains_original_semantic_deadline() {
        let first = std::time::Instant::now();
        let mut held = None;
        assert_eq!(original_probe_deadline(&mut held, [1; 16], first), first);
        assert_eq!(
            original_probe_deadline(
                &mut held,
                [1; 16],
                first + std::time::Duration::from_secs(5)
            ),
            first
        );
        assert_eq!(
            original_probe_deadline(
                &mut held,
                [2; 16],
                first + std::time::Duration::from_secs(10)
            ),
            first + std::time::Duration::from_secs(10)
        );
    }
    #[test]
    fn initial_native_sample_is_same_nonce_only_and_not_owned_executable() {
        use theseus_host::services::readiness::*;
        let observed = ReadinessObservation {
            state: ReadinessState::Ready,
            peer: None,
            peer_sid: None,
            response: None,
            failure: None,
            authenticated_instance: true,
            instance_binding_digest: [1; 32],
        };
        let sample = Some(observed.clone());
        assert_eq!(
            initial_native_sample(true, [1; 16], [1; 16], &sample),
            Some(observed)
        );
        assert_eq!(initial_native_sample(true, [1; 16], [2; 16], &sample), None);
        assert_eq!(
            initial_native_sample(false, [1; 16], [1; 16], &sample),
            None
        );
    }
    #[test]
    fn unknown_mandatory_lifetime_can_record_local_retirement_independently() {
        let mut old = initial_lifetime(&definition()).unwrap();
        old.primary = NativePrimaryState::Unknown;
        old.instance = NativeInstanceState::Unknown;
        let mut next = old.clone();
        next.local_resources = NativeLocalState::Retired;
        let next = advance_lifetime(&old, next).unwrap();
        assert!(next.local_retired());
        assert!(!next.required_lifetime_satisfied());
    }
    #[test]
    fn reconnect_activate_and_query_poll_same_original_deadline_without_quarantine() {
        // Model the real Host deadline pin at the actual production callback
        // boundary. No SDK/native owner is fabricated by this pure test.
        let original = Instant::now() + std::time::Duration::from_secs(30);
        let mut pinned = None;
        let mut polls = 0;
        let mut poll = |deadline| -> io::Result<Option<bool>> {
            polls += 1;
            if pinned.is_some_and(|old| old != deadline) {
                return Err(io::Error::other("Host rejected changed original deadline"));
            }
            pinned = Some(deadline);
            Ok(None)
        };
        assert_eq!(
            poll_original_activation(Some(original), original, &mut poll).unwrap(),
            None
        );
        // Transport pending state was lost; same accepted Activate is replayed.
        assert_eq!(
            poll_original_activation(
                Some(original),
                original + std::time::Duration::from_secs(5),
                &mut poll
            )
            .unwrap(),
            None
        );
        // Query recovery has a different caller wait budget too.
        assert_eq!(
            poll_original_activation(
                Some(original),
                original + std::time::Duration::from_secs(10),
                &mut poll
            )
            .unwrap(),
            None
        );
        assert_eq!(pinned, Some(original));
        assert_eq!(polls, 3);
    }
    #[test]
    fn replay_budget_cannot_extend_true_expiry_or_disable_cancel_and_terminal_sample_is_exact() {
        let original = Instant::now();
        let proposed = original + std::time::Duration::from_secs(30);
        let expired = poll_original_activation(Some(original), proposed, |deadline| {
            Ok(native_return_timely(deadline, original, false))
        })
        .unwrap();
        assert!(!expired);
        let cancelled = poll_original_activation(
            Some(proposed),
            proposed + std::time::Duration::from_secs(30),
            |deadline| Ok(native_return_timely(deadline, original, true)),
        )
        .unwrap();
        assert!(!cancelled);
        let sample = Some(("same-original-operation", [7u8; 16]));
        let cached = poll_original_activation(Some(original), proposed, |_| Ok(sample)).unwrap();
        assert_eq!(cached, sample);
    }

    #[test]
    fn missing_original_deadline_never_touches_original_native_owner() {
        let mut touched = false;
        let result = poll_original_activation(None, Instant::now(), |_| {
            touched = true;
            Ok(())
        });
        assert!(result.is_err());
        assert!(!touched);
    }
}
