//! Explicit precommitted generation selection; no owner construction or new authority.
use super::BrokerServerConfig;
use std::io;
use theseus_native_protocol::v2::{self, broker::*};
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn tuple(
    config: &BrokerServerConfig,
    transition: Option<&DesktopActivationTransition>,
) -> io::Result<v2::DesktopDefinition> {
    let key = transition.map_or(v2::DesktopDefinitionKey::CONFIGURED_TARGET, |t| {
        t.definition
    });
    let selected = config
        .manifest
        .desktop_definition(&key)
        .map_err(io::Error::other)?;
    if let Some(t) = transition {
        t.validate().map_err(io::Error::other)?;
        if t.tuple_digest != selected.digest().map_err(io::Error::other)?.to_hex() {
            return Err(invalid(
                "generation tuple differs from immutable full manifest",
            ));
        }
    }
    Ok(selected)
}
pub(super) fn inspection(
    config: &BrokerServerConfig,
    view: &BrokerLedgerView,
    generation: Option<&DesktopActivationTransition>,
    continuity: Option<&BrokerOperationIdentity>,
) -> io::Result<v2::DesktopDefinition> {
    let previous = view
        .requests()
        .filter(|r| matches!(r.action, DesktopAction::Activate { .. }))
        .last();
    let Some(generation) = generation else {
        return tuple(config, previous.and_then(DesktopRequest::transition));
    };
    let selected = tuple(config, Some(generation))?;
    if previous.is_some_and(|old| old.transition() == Some(generation)) {
        return Ok(selected);
    }
    if view.requests().any(|old| {
        old.transition()
            .is_some_and(|t| t.sut.operation == generation.sut.operation)
    }) {
        return Err(invalid(
            "SUT transition already consumed by a different original activation",
        ));
    }
    match (previous, &generation.previous_activation) {
        (None, None) => {}
        (Some(old), Some(identity))
            if old.identity().as_ref() == Ok(identity)
                && view
                    .query(old)
                    .map_err(io::Error::other)?
                    .original_retired()
                && continuity == Some(identity) => {}
        _ => {
            return Err(invalid(
                "original prior generation/retirement continuity unavailable",
            ));
        }
    }
    Ok(selected)
}
pub(super) fn activation(
    config: &BrokerServerConfig,
    view: &BrokerLedgerView,
    request: &DesktopRequest,
    continuity: Option<&BrokerOperationIdentity>,
) -> io::Result<v2::DesktopDefinition> {
    if !config.manifest.desktop_definitions.is_empty() && request.transition().is_none() {
        return Err(invalid(
            "keyed manifest requires exact successful SUT transition",
        ));
    }
    view.validate_broker_generation(request)
        .map_err(io::Error::other)?;
    let selected = inspection(config, view, request.transition(), continuity)?;
    let DesktopAction::Activate {
        mode,
        entry_digest,
        expectation_digest,
        ..
    } = &request.action
    else {
        return Err(invalid("activation required"));
    };
    if !selected.desktop.allowed_modes.contains(mode)
        || selected.desktop.entry_digest != *entry_digest
        || selected.desktop.expectation_digest != *expectation_digest
    {
        return Err(invalid("activation differs from selected admitted tuple"));
    }
    Ok(selected)
}
pub(super) fn inspection_binding(
    base: &str,
    selected: &v2::DesktopDefinition,
    generation: Option<&DesktopActivationTransition>,
) -> io::Result<String> {
    use sha2::{Digest as _, Sha256};
    let Some(generation) = generation else {
        return Ok(base.into());
    };
    let mut hash = Sha256::new();
    hash.update(b"theseus.desktop.original-generation-binding-v1\0");
    hash.update(
        v2::Digest::parse(base)
            .map_err(io::Error::other)?
            .as_bytes(),
    );
    hash.update(selected.digest().map_err(io::Error::other)?.as_bytes());
    hash.update(serde_json::to_vec(generation).map_err(io::Error::other)?);
    Ok(format!("{:x}", hash.finalize()))
}
