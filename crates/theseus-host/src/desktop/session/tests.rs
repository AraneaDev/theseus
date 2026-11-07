use super::*;
pub(crate) fn authority() -> SessionAuthority {
    SessionAuthority {
        kind: DesktopKind::Linux,
        binding: SessionBinding {
            target_id: "source-target".into(),
            ownership_id: [1; 16],
            provider_epoch: [2; 32],
            session_id: "source-session".into(),
            session_epoch: [3; 32],
            broker_header_digest: [4; 32],
            manifest_digest: [5; 32],
            broker_endpoint: BrokerEndpoint::Unix(
                UnixPath::new("/tmp/theseus-source/broker.sock".into()).unwrap(),
            ),
            broker_principal: CurrentPrincipal::Posix {
                effective_uid: 1001,
                effective_gid: 1001,
            },
            helper_principal: CurrentPrincipal::Posix {
                effective_uid: 1001,
                effective_gid: 1001,
            },
            display: SuppliedDisplay::SourceOwnedHeadless,
        },
        allowed_modes: vec![DesktopMode::OwnedExecutable],
    }
}
#[test]
fn explicit_source_mode_cannot_claim_native_activation_or_interactive_display() {
    let a = authority();
    let s = BoundSession::admit(a.clone(), &a.binding).unwrap();
    assert!(s.is_source_owned_headless());
    assert_eq!(
        s.authorize(DesktopMode::NativeActivation),
        Err(SessionRefusal::ModeDenied)
    );
    let mut a = authority();
    a.allowed_modes.push(DesktopMode::NativeActivation);
    assert!(a.validate().is_err());
}
#[test]
fn wrong_principal_epoch_session_endpoint_and_logout_permanently_revoke_original() {
    let a = authority();
    let mut changes = vec![];
    let mut b = a.binding.clone();
    b.provider_epoch = [44; 32];
    changes.push(b);
    let mut b = a.binding.clone();
    b.session_epoch = [44; 32];
    changes.push(b);
    let mut b = a.binding.clone();
    b.broker_principal = CurrentPrincipal::Posix {
        effective_uid: 22,
        effective_gid: 22,
    };
    changes.push(b);
    let mut b = a.binding.clone();
    b.session_id = "replacement".into();
    changes.push(b);
    let mut b = a.binding.clone();
    b.broker_endpoint = BrokerEndpoint::Unix(UnixPath::new("/tmp/replaced.sock".into()).unwrap());
    changes.push(b);
    for changed in changes {
        assert!(BoundSession::admit(a.clone(), &changed).is_err());
        let mut s = BoundSession::admit(a.clone(), &a.binding).unwrap();
        assert!(s.observe(Some(&changed)).is_err());
        assert_eq!(s.observe(Some(&a.binding)), Err(SessionRefusal::Revoked));
        assert_eq!(
            s.authorize(DesktopMode::OwnedExecutable),
            Err(SessionRefusal::Revoked)
        );
    }
    let mut s = BoundSession::admit(a.clone(), &a.binding).unwrap();
    assert!(s.observe(None).is_err());
    assert!(s.is_revoked());
}
#[test]
fn session_zero_and_missing_display_are_refused_without_discovery() {
    let mut a = authority();
    a.kind = DesktopKind::Windows;
    a.binding.broker_endpoint =
        BrokerEndpoint::Pipe(NamedPipe::new(r"\\.\pipe\theseus.source".into()).unwrap());
    a.binding.broker_principal = CurrentPrincipal::Windows {
        token_user_sid: "S-1-5-18".into(),
    };
    a.binding.helper_principal = a.binding.broker_principal.clone();
    a.binding.display = SuppliedDisplay::WindowsInteractive {
        session_id: 0,
        window_station: "WinSta0".into(),
        desktop: "Default".into(),
    };
    assert_eq!(a.validate(), Err(SessionRefusal::SessionZero));
    let mut a = authority();
    a.binding.display = SuppliedDisplay::X11 {
        display: "".into(),
        authority_file: "/tmp/source/auth".into(),
        bus: UnixPath::new("/tmp/source/bus.sock".into()).unwrap(),
    };
    assert!(a.validate().is_err());
}

#[test]
fn native_adapter_gate_requires_interactive_matching_allowed_unrevoked_session() {
    let headless = authority();
    let s = BoundSession::admit(headless.clone(), &headless.binding).unwrap();
    assert!(s.authorize_native_adapter(DesktopKind::Linux).is_err());
    let mut a = authority();
    a.binding.display = SuppliedDisplay::X11 {
        display: ":92".into(),
        authority_file: "/private/auth".into(),
        bus: UnixPath::new("/private/bus".into()).unwrap(),
    };
    let s = BoundSession::admit(a.clone(), &a.binding).unwrap();
    assert_eq!(
        s.authorize_native_adapter(DesktopKind::Linux),
        Err(SessionRefusal::ModeDenied)
    );
    a.allowed_modes.push(DesktopMode::NativeActivation);
    let mut s = BoundSession::admit(a.clone(), &a.binding).unwrap();
    assert_eq!(s.authorize_native_adapter(DesktopKind::Linux), Ok(()));
    assert_eq!(
        s.authorize_native_adapter(DesktopKind::MacOs),
        Err(SessionRefusal::WrongPlatform)
    );
    assert_eq!(
        s.authorize(DesktopMode::NativeActivation),
        Err(SessionRefusal::NativeActivationUnavailable)
    );
    assert!(s.observe(None).is_err());
    assert_eq!(
        s.authorize_native_adapter(DesktopKind::Linux),
        Err(SessionRefusal::Revoked)
    );
}
