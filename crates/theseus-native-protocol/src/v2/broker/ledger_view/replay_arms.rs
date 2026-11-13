//! Larger broker-event arms of the ledger replay.
use super::*;

/// Shared, immutable replay inputs used by the larger broker-event arms of
/// [`BrokerLedgerView::replay`].
#[derive(Clone, Copy)]
pub(super) struct ReplayContext<'a> {
    pub(super) records: &'a [Record],
    pub(super) broker_header: Option<&'a IntegrationBrokerHeader>,
    pub(super) declared: Option<(&'a DesktopManifest, &'a IntegrationBrokerHeader)>,
    pub(super) helper: bool,
    pub(super) lease: &'a Option<String>,
}
impl ReplayContext<'_> {
    pub(super) fn helper_action_accepted(
        &self,
        result: &mut BrokerLedgerView,
        request: &DesktopRequest,
        request_digest: &str,
    ) -> Result<(), LedgerError> {
        let Self {
            records,
            declared,
            helper,
            ..
        } = *self;
        let invalid = || LedgerError::InvalidTransition;
        request.validate().map_err(|_| invalid())?;
        let (desktop, header) = declared.ok_or_else(invalid)?;
        let receipt = result
            .binds
            .first()
            .and_then(|b| b.2.as_ref())
            .ok_or_else(invalid)?;
        if !helper
            || result.helper_released
            || request.slot != desktop.slot
            || request.helper_lease != receipt.original.helper_lease
            || request.broker_lease != receipt.broker_lease
            || request.binding_digest != header.digest().map_err(|_| invalid())?.to_hex()
            || request.digest().map_err(|_| invalid())?.to_hex() != *request_digest
            || result
                .helper_actions
                .iter()
                .any(|(r, _)| r.helper_operation == request.helper_operation)
        {
            return Err(invalid());
        }
        match &request.action {
            DesktopAction::Activate { .. } => {
                let manifest = records
                    .iter()
                    .find_map(|r| match r.entry() {
                        Entry::DesktopAuthorityDeclared { manifest, .. } => Some(manifest.as_ref()),
                        _ => None,
                    })
                    .ok_or_else(invalid)?;
                result
                    .validate_helper_generation(manifest, request)
                    .map_err(|_| invalid())?;
            }
            DesktopAction::RequestClose { .. }
                if result
                    .helper_actions
                    .iter()
                    .any(|(r, _)| r.close_request().as_ref() == Ok(request)) => {}
            _ => return Err(invalid()),
        }
        let initial = match &request.action {
            DesktopAction::Activate {
                owned_lifetime: Some(definition),
                ..
            } => {
                definition
                    .validate_platform(&header.os)
                    .map_err(|_| invalid())?;
                Some(BrokerState::accepted_owned(*definition))
            }
            DesktopAction::RequestClose {
                original_activation_operation,
                ..
            } => result
                .helper_actions
                .iter()
                .find(|(r, _)| &r.helper_operation == original_activation_operation)
                .and_then(|(_, state)| state.as_ref())
                .filter(|state| state.owned_lifetime.is_some())
                .map(|original| {
                    let owned = original
                        .owned_lifetime
                        .as_ref()
                        .ok_or(Error::InvalidValue)?;
                    let mut state = BrokerState::accepted_owned(owned.definition);
                    state.record_owned_lifetime(owned.clone())?;
                    state.owner = original.owner;
                    state.quarantined = original.quarantined;
                    Ok::<_, Error>(state)
                })
                .transpose()
                .map_err(|_| invalid())?,
            _ => None,
        };
        result.helper_actions.push((request.clone(), initial));
        Ok(())
    }
    pub(super) fn helper_state_observed(
        &self,
        result: &mut BrokerLedgerView,
        record: &Record,
        identity: &BrokerOperationIdentity,
        state: &BrokerState,
    ) -> Result<(), LedgerError> {
        let Self {
            records,
            declared,
            helper,
            ..
        } = *self;
        let invalid = || LedgerError::InvalidTransition;
        identity.validate().map_err(|_| invalid())?;
        state.validate().map_err(|_| invalid())?;
        if result.helper_actions.iter().any(|(request, _)| {
            request.identity().as_ref() == Ok(identity) && request.transition().is_some()
        }) && result.helper_transition(identity).is_none()
        {
            return Err(invalid());
        }
        if !helper || result.helper_released {
            return Err(invalid());
        }
        let entry = result
            .helper_actions
            .iter_mut()
            .find(|(r, _)| r.identity().as_ref() == Ok(identity))
            .ok_or_else(invalid)?;
        if let Some(old) = &entry.1
            && ((matches!(old.effect, BrokerEffect::Receipt { .. }) && old.effect != state.effect)
                || (old.primary == BrokerPrimary::Exited && state.primary != BrokerPrimary::Exited)
                || (old.descendants == BrokerDescendants::Joined
                    && state.descendants != BrokerDescendants::Joined)
                || (old.cleanup_complete && !state.cleanup_complete)
                || (old.quarantined && !state.quarantined))
        {
            return Err(invalid());
        }
        match (
            entry.1.as_ref().and_then(|s| s.owned_lifetime.as_ref()),
            &state.owned_lifetime,
        ) {
            (Some(old), Some(next)) => {
                next.validate_platform(&declared.ok_or_else(invalid)?.1.os)
                    .map_err(|_| invalid())?;
                next.validate_progress_from(old).map_err(|_| invalid())?;
                if entry
                    .1
                    .as_ref()
                    .is_some_and(|s| s.owner == BrokerOwner::Lost)
                    && state.owner != BrokerOwner::Lost
                {
                    return Err(invalid());
                }
            }
            (None, None) => {}
            _ => return Err(invalid()),
        }
        if let Some(native) = &state.native_lifetime {
            let manifest = records
                .iter()
                .find_map(|r| match r.entry() {
                    Entry::DesktopAuthorityDeclared { manifest, .. } => Some(manifest),
                    _ => None,
                })
                .ok_or_else(invalid)?;
            let key = entry.0.transition().map_or(
                crate::v2::DesktopDefinitionKey::CONFIGURED_TARGET,
                |transition| transition.definition,
            );
            manifest
                .desktop_definition(&key)
                .map_err(|_| invalid())?
                .native
                .as_ref()
                .ok_or_else(invalid)?
                .validate_state(native)
                .map_err(|_| invalid())?;
        }
        if let Some(old) = &entry.1 {
            match (&old.native_lifetime, &state.native_lifetime) {
                (Some(prior), Some(next)) => {
                    next.validate_progress_from(prior).map_err(|_| invalid())?
                }
                (Some(_), None) => return Err(invalid()),
                (None, Some(_)) if old.owner == BrokerOwner::Lost || old.quarantined => {
                    return Err(invalid());
                }
                _ => {}
            }
        }
        if state.original_retired()
            && !result
                .helper_retired_at
                .iter()
                .any(|(operation, _)| operation == &identity.helper_operation)
        {
            result
                .helper_retired_at
                .push((identity.helper_operation.clone(), record.index()));
        }
        entry.1 = Some(state.clone());
        Ok(())
    }
    pub(super) fn accepted(
        &self,
        result: &mut BrokerLedgerView,
        request: &DesktopRequest,
        request_digest: &str,
    ) -> Result<(), LedgerError> {
        let Self {
            broker_header,
            lease,
            ..
        } = *self;
        let invalid = || LedgerError::InvalidTransition;
        request.validate().map_err(|_| invalid())?;
        let header = broker_header.ok_or_else(invalid)?;
        if lease.as_ref() != Some(&request.broker_lease)
            || request.binding_digest != header.digest().map_err(|_| invalid())?.to_hex()
            || request.digest().map_err(|_| invalid())?.to_hex() != *request_digest
            || result.accepted.len() >= crate::v2::MAX_OPERATIONS
            || result
                .accepted
                .iter()
                .any(|a| a.request.helper_operation == request.helper_operation || a.quarantined)
        {
            return Err(invalid());
        }
        match &request.action {
            DesktopAction::Activate {
                mode,
                owned_lifetime,
                ..
            } => {
                if let Some(definition) = owned_lifetime {
                    definition
                        .validate_platform(&header.os)
                        .map_err(|_| invalid())?;
                }
                if !header.allowed_modes.contains(mode)
                    || result.validate_broker_generation(request).is_err()
                {
                    return Err(invalid());
                }
            }
            DesktopAction::RequestClose {
                original_activation_operation,
                ..
            } => {
                if !result.accepted.iter().any(|a| {
                    a.request.helper_operation == *original_activation_operation
                        && a.request.close_request().as_ref() == Ok(request)
                }) {
                    return Err(invalid());
                }
            }
        }
        let state = match &request.action {
            DesktopAction::Activate {
                owned_lifetime: Some(definition),
                ..
            } => BrokerState::accepted_owned(*definition),
            DesktopAction::RequestClose {
                original_activation_operation,
                ..
            } => {
                let original = result
                    .accepted
                    .iter()
                    .find(|a| &a.request.helper_operation == original_activation_operation)
                    .ok_or_else(invalid)?;
                if let Some(owned) = &original.state.owned_lifetime {
                    let mut state = BrokerState::accepted_owned(owned.definition);
                    state
                        .record_owned_lifetime(owned.clone())
                        .map_err(|_| invalid())?;
                    state.quarantined = original.state.quarantined;
                    state
                } else {
                    BrokerState::unknown()
                }
            }
            _ => BrokerState::unknown(),
        };
        result.accepted.push(Accepted {
            request: request.clone(),
            state,
            receipt: false,
            quarantined: false,
        });
        Ok(())
    }
    pub(super) fn effect_recorded(
        &self,
        result: &mut BrokerLedgerView,
        operation: &str,
        receipt: &BrokerEffectReceipt,
    ) -> Result<(), LedgerError> {
        #[cfg(feature = "test-support")]
        let Self { broker_header, .. } = *self;
        let invalid = || LedgerError::InvalidTransition;
        Digest::parse(&receipt.receipt_digest).map_err(|_| invalid())?;
        let original_native = result
            .accepted
            .iter()
            .find(|a| a.request.helper_operation == *operation)
            .and_then(|a| match &a.request.action {
                DesktopAction::RequestClose {
                    original_activation_operation,
                    ..
                } => Some(original_activation_operation),
                _ => None,
            })
            .is_some_and(|original| {
                result.accepted.iter().any(|a| {
                    a.request.helper_operation == *original
                        && matches!(
                            a.request.action,
                            DesktopAction::Activate {
                                mode: DesktopMode::NativeActivation,
                                ..
                            }
                        )
                })
            });
        let accepted = result
            .accepted
            .iter_mut()
            .find(|a| a.request.helper_operation == *operation)
            .ok_or_else(invalid)?;
        let evidence_matches = match &accepted.request.action {
            DesktopAction::Activate {
                mode: DesktopMode::OwnedExecutable,
                ..
            } => {
                #[cfg(feature = "test-support")]
                let source = broker_header
                    .is_some_and(|h| h.display == SuppliedDisplay::SourceOwnedHeadless);
                #[cfg(not(feature = "test-support"))]
                let source = false;
                receipt.evidence
                    == if source {
                        DesktopEvidence::SourceOwnedExecutable
                    } else {
                        DesktopEvidence::OwnedExecutable
                    }
            }
            DesktopAction::Activate {
                mode: DesktopMode::NativeActivation,
                ..
            } => receipt.evidence == DesktopEvidence::NativeActivation,
            DesktopAction::RequestClose { .. } if original_native => {
                receipt.evidence == DesktopEvidence::NativeActivation
            }
            DesktopAction::RequestClose { .. } => {
                #[cfg(feature = "test-support")]
                let source = broker_header
                    .is_some_and(|h| h.display == SuppliedDisplay::SourceOwnedHeadless);
                #[cfg(not(feature = "test-support"))]
                let source = false;
                receipt.evidence
                    == if source {
                        DesktopEvidence::SourceOwnedExecutable
                    } else {
                        DesktopEvidence::OwnedExecutable
                    }
            }
        };
        if accepted.receipt || accepted.quarantined || !evidence_matches {
            return Err(invalid());
        }
        accepted.receipt = true;
        accepted.state.effect = BrokerEffect::Receipt {
            receipt: receipt.clone(),
        };
        Ok(())
    }
    pub(super) fn lifetime_recorded(
        result: &mut BrokerLedgerView,
        operation: &str,
        primary: &BrokerPrimary,
        descendants: &BrokerDescendants,
        cleanup_complete: &bool,
    ) -> Result<(), LedgerError> {
        let invalid = || LedgerError::InvalidTransition;
        let accepted = result
            .accepted
            .iter_mut()
            .find(|a| a.request.helper_operation == *operation)
            .ok_or_else(invalid)?;
        if !matches!(
            accepted.request.action,
            DesktopAction::Activate {
                mode: DesktopMode::OwnedExecutable,
                ..
            }
        ) || accepted.state.owned_lifetime.is_some()
            || (accepted.state.cleanup_complete && !cleanup_complete)
            || (accepted.state.primary == BrokerPrimary::Exited
                && *primary != BrokerPrimary::Exited)
            || (accepted.state.primary == BrokerPrimary::NotCreated
                && *primary != BrokerPrimary::NotCreated)
            || (accepted.state.descendants == BrokerDescendants::Joined
                && *descendants != BrokerDescendants::Joined)
            || (matches!(primary, BrokerPrimary::NotCreated)
                != matches!(descendants, BrokerDescendants::NotCreated))
            || (*cleanup_complete
                && (!matches!(primary, BrokerPrimary::Exited | BrokerPrimary::NotCreated)
                    || !matches!(
                        descendants,
                        BrokerDescendants::Joined | BrokerDescendants::NotCreated
                    )))
        {
            return Err(invalid());
        }
        // Irreversible observations retain their original meaning;
        // they do not reconstruct a live native owner on restart.
        if matches!(primary, BrokerPrimary::Exited | BrokerPrimary::NotCreated) {
            accepted.state.primary = *primary;
        }
        if matches!(
            descendants,
            BrokerDescendants::Joined | BrokerDescendants::NotCreated
        ) {
            accepted.state.descendants = *descendants;
        }
        if *cleanup_complete {
            accepted.state.owner = BrokerOwner::Settled;
            accepted.state.primary = *primary;
            accepted.state.descendants = *descendants;
            accepted.state.cleanup_complete = true;
            accepted.state.quarantined = accepted.quarantined;
        }
        Ok(())
    }
}
