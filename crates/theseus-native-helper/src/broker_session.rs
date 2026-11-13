//! Admitted broker session validation and host-to-protocol projections.
use super::*;
pub(super) fn aggregate_cleanup(
    observed: &theseus_host::desktop::owned::LifetimeObservation,
) -> bool {
    observed.local_cleanup_complete
        && (observed.primary_exited || observed.descendants == DescendantScope::NotCreated)
        && matches!(
            observed.descendants,
            DescendantScope::Joined | DescendantScope::NotCreated
        )
}
pub(super) fn descendants(scope: DescendantScope) -> BrokerDescendants {
    match scope {
        DescendantScope::NotCreated => BrokerDescendants::NotCreated,
        DescendantScope::Pending => BrokerDescendants::Pending,
        DescendantScope::Joined => BrokerDescendants::Joined,
        DescendantScope::Unknown => BrokerDescendants::Unknown,
    }
}
pub(super) fn principal(value: &theseus_host::principal::CurrentPrincipal) -> BrokerPrincipal {
    match value {
        theseus_host::principal::CurrentPrincipal::Posix {
            effective_uid,
            effective_gid,
        } => BrokerPrincipal::Posix {
            effective_uid: *effective_uid,
            effective_gid: *effective_gid,
        },
        theseus_host::principal::CurrentPrincipal::Windows { token_user_sid } => {
            BrokerPrincipal::Windows {
                token_user_sid: token_user_sid.clone(),
            }
        }
    }
}
pub(super) fn validate_session(
    header: &IntegrationBrokerHeader,
    session: &BoundSession,
) -> io::Result<()> {
    session
        .authorize(theseus_host::desktop::session::DesktopMode::OwnedExecutable)
        .map_err(io::Error::from)?;
    validate_session_binding(header, session.binding())
}
/// Protocol projection of the host session's broker endpoint, compared
/// exactly with the admitted header endpoint.
pub(super) fn session_endpoint(
    endpoint: &theseus_host::desktop::session::BrokerEndpoint,
) -> io::Result<v2::LocalEndpoint> {
    use theseus_host::desktop::session::BrokerEndpoint;
    Ok(match endpoint {
        BrokerEndpoint::Unix(path) => v2::LocalEndpoint::UnixPath {
            path: path.as_str().into(),
        },
        // The protocol's canonical NamedPipe path is the full closed
        // `\\.\pipe\theseus.<name>` form (see `validate_endpoint`), so the
        // header compares against the full host pipe name, never its suffix.
        BrokerEndpoint::Pipe(name) => {
            let endpoint = v2::LocalEndpoint::NamedPipe {
                path: name.as_str().into(),
            };
            v2::broker::validate_endpoint(&endpoint)
                .map_err(|_| invalid("invalid closed broker pipe"))?;
            endpoint
        }
    })
}
pub(super) fn validate_session_binding(
    header: &IntegrationBrokerHeader,
    b: &SessionBinding,
) -> io::Result<()> {
    use theseus_host::desktop::session::SuppliedDisplay as HostDisplay;
    let endpoint = session_endpoint(&b.broker_endpoint)?;
    // A workspace dependency can unify Host's test-support independently of
    // this crate. Such source-only authority must still refuse shipping code.
    #[allow(unreachable_patterns)]
    let display = match &b.display {
        HostDisplay::X11 {
            display,
            authority_file,
            bus,
        } => SuppliedDisplay::X11 {
            display: display.clone(),
            authority_file: authority_file.clone(),
            bus: bus.as_str().into(),
        },
        HostDisplay::Wayland {
            socket,
            runtime_directory,
            bus,
        } => SuppliedDisplay::Wayland {
            socket: socket.as_str().into(),
            runtime_directory: runtime_directory.clone(),
            bus: bus.as_str().into(),
        },
        HostDisplay::MacOsGui { domain } => SuppliedDisplay::MacOsGui {
            domain: domain.clone(),
        },
        HostDisplay::WindowsInteractive {
            session_id,
            window_station,
            desktop,
        } => SuppliedDisplay::WindowsInteractive {
            session_id: *session_id,
            window_station: window_station.clone(),
            desktop: desktop.clone(),
        },
        #[cfg(feature = "test-support")]
        HostDisplay::SourceOwnedHeadless => SuppliedDisplay::SourceOwnedHeadless,
        #[cfg(not(feature = "test-support"))]
        _ => {
            return Err(invalid(
                "source-only headless session is not shipping authority",
            ));
        }
    };
    if b.target_id != header.target_id
        || b.ownership_id
            != *v2::BindingId::parse(&header.ownership_id)
                .map_err(io::Error::other)?
                .as_bytes()
        || b.provider_epoch
            != *v2::Digest::parse(&header.provider_epoch)
                .map_err(io::Error::other)?
                .as_bytes()
        || b.session_id != header.session_id
        || b.session_epoch
            != *v2::Digest::parse(&header.session_epoch)
                .map_err(io::Error::other)?
                .as_bytes()
        || b.broker_header_digest != *header.digest().map_err(io::Error::other)?.as_bytes()
        || b.manifest_digest
            != *v2::Digest::parse(&header.manifest_digest)
                .map_err(io::Error::other)?
                .as_bytes()
        || endpoint != header.broker_endpoint
        || principal(&b.broker_principal) != header.broker_principal
        || principal(&b.helper_principal) != header.helper_principal
        || display != header.display
    {
        return Err(invalid("supplied broker session mismatch"));
    }
    Ok(())
}
