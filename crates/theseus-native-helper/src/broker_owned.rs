//! Original direct-owned declaration/proof mapping; wire facts never mint an owner.
use super::RetainedActivation;
use std::io;
use theseus_native_protocol::v2::{self, broker::*};
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
pub(crate) fn validate_definition(
    expected: Option<OwnedDesktopLifetimeDefinition>,
    actual: Option<OwnedDesktopLifetimeDefinition>,
) -> io::Result<()> {
    if expected != actual {
        return Err(invalid(
            "original owned declaration differs from selected full manifest tuple",
        ));
    }
    Ok(())
}
pub(crate) fn validate_platform(
    definition: Option<OwnedDesktopLifetimeDefinition>,
    os: &str,
) -> io::Result<()> {
    if let Some(definition) = definition {
        definition.validate_platform(os).map_err(io::Error::other)?;
        definition
            .validate_platform(std::env::consts::OS)
            .map_err(io::Error::other)?;
    }
    Ok(())
}
pub(crate) fn validate_backend(
    definition: Option<OwnedDesktopLifetimeDefinition>,
    session: &theseus_host::desktop::session::BoundSession,
    os: &str,
) -> io::Result<()> {
    validate_platform(definition, os)?;
    if definition.is_some() {
        theseus_host::desktop::owned::OwnedExecutable::validate_original_primary_backend(session)?;
    }
    Ok(())
}
fn preserve_history(
    old: &OwnedLifetimeState,
    mut observed: OwnedLifetimeState,
) -> io::Result<OwnedLifetimeState> {
    observed.validate().map_err(io::Error::other)?;
    if matches!(
        old.primary,
        OwnedPrimaryState::Exited | OwnedPrimaryState::Unknown
    ) {
        observed.primary = old.primary;
    }
    if old.descendants == OwnedTreeState::Unknown {
        observed.descendants = old.descendants;
    }
    if matches!(
        old.local_resources,
        OwnedLocalState::Retired | OwnedLocalState::Unknown
    ) {
        observed.local_resources = old.local_resources;
    }
    observed
        .validate_progress_from(old)
        .map_err(io::Error::other)?;
    Ok(observed)
}
pub(super) fn observe(
    original: &DesktopRequest,
    owner: &RetainedActivation,
    old: &OwnedLifetimeState,
) -> io::Result<OwnedLifetimeState> {
    validate_definition(Some(old.definition), original.owned_lifetime_definition())?;
    let RetainedActivation::Owned(owner) = owner else {
        return Err(invalid(
            "explicit owned state requires same original direct-owned owner",
        ));
    };
    let proof = owner.owned_lifetime_proof()?;
    let binding = proof.binding();
    if binding.ready.start_or_activation_operation != original.helper_operation
        || binding.ready.action_digest != *original.digest().map_err(io::Error::other)?.as_bytes()
        || binding.ready.lease != original.broker_lease
        || binding.ready.slot != original.slot.as_str()
        || binding.ready.nonce
            != *v2::BindingId::parse(&original.activation_nonce)
                .map_err(io::Error::other)?
                .as_bytes()
        || binding.ready.resource_binding_digest
            != *v2::Digest::parse(&original.prior_binding_digest)
                .map_err(io::Error::other)?
                .as_bytes()
        || proof.session().broker_header_digest
            != *v2::Digest::parse(&original.binding_digest)
                .map_err(io::Error::other)?
                .as_bytes()
    {
        return Err(invalid(
            "owned proof differs from original executable/session/action binding",
        ));
    }
    let uncertain = proof.uncertain();
    let state = OwnedLifetimeState {
        definition: old.definition,
        primary: match proof.primary() {
            theseus_host::desktop::owned::OriginalOwnedPrimary::Running => {
                OwnedPrimaryState::Running
            }
            theseus_host::desktop::owned::OriginalOwnedPrimary::Exited => OwnedPrimaryState::Exited,
            theseus_host::desktop::owned::OriginalOwnedPrimary::Unknown => {
                OwnedPrimaryState::Unknown
            }
        },
        descendants: if uncertain {
            OwnedTreeState::Unknown
        } else {
            OwnedTreeState::Unavailable
        },
        local_resources: if uncertain {
            OwnedLocalState::Unknown
        } else if proof.local_retired() {
            OwnedLocalState::Retired
        } else {
            OwnedLocalState::Retained
        },
    };
    preserve_history(old, state)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_owned_definition_must_equal_original_action_before_forwarding() {
        let primary = Some(OwnedDesktopLifetimeDefinition::MacosOriginalPrimaryV1);
        assert!(validate_definition(primary, primary).is_ok());
        assert!(validate_definition(None, None).is_ok());
        assert!(validate_definition(primary, None).is_err());
        assert!(validate_definition(None, primary).is_err());
    }
    #[test]
    fn mac_owned_declaration_requires_actual_compiled_platform_even_with_mac_header() {
        let primary = Some(OwnedDesktopLifetimeDefinition::MacosOriginalPrimaryV1);
        assert_eq!(
            validate_platform(primary, "macos").is_ok(),
            cfg!(target_os = "macos")
        );
        assert!(validate_platform(primary, "linux").is_err());
        assert!(validate_platform(primary, "windows").is_err());
        for platform in ["linux", "macos", "windows"] {
            assert!(validate_platform(None, platform).is_ok());
        }
    }
    #[test]
    fn original_owned_projection_keeps_terminal_unknown_and_local_retirement_sticky() {
        let definition = OwnedDesktopLifetimeDefinition::MacosOriginalPrimaryV1;
        let exited = OwnedLifetimeState {
            definition,
            primary: OwnedPrimaryState::Exited,
            descendants: OwnedTreeState::Unavailable,
            local_resources: OwnedLocalState::Retired,
        };
        assert_eq!(
            preserve_history(&OwnedLifetimeState::initial(definition), exited.clone()).unwrap(),
            exited
        );
        let old = OwnedLifetimeState {
            definition,
            primary: OwnedPrimaryState::Unknown,
            descendants: OwnedTreeState::Unknown,
            local_resources: OwnedLocalState::Unknown,
        };
        let newly_observed = OwnedLifetimeState {
            definition,
            primary: OwnedPrimaryState::Exited,
            descendants: OwnedTreeState::Unavailable,
            local_resources: OwnedLocalState::Retired,
        };
        assert_eq!(preserve_history(&old, newly_observed).unwrap(), old);
        let old = OwnedLifetimeState {
            definition,
            primary: OwnedPrimaryState::Exited,
            descendants: OwnedTreeState::Unavailable,
            local_resources: OwnedLocalState::Retired,
        };
        let uncertain = OwnedLifetimeState {
            definition,
            primary: OwnedPrimaryState::Unknown,
            descendants: OwnedTreeState::Unknown,
            local_resources: OwnedLocalState::Unknown,
        };
        let next = preserve_history(&old, uncertain).unwrap();
        assert_eq!(next.primary, OwnedPrimaryState::Exited);
        assert_eq!(next.local_resources, OwnedLocalState::Retired);
        assert_eq!(next.descendants, OwnedTreeState::Unknown);
    }
}
