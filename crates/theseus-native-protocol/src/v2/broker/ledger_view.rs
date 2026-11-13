//! Broker events and the replayed broker ledger view.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrokerEvent {
    EntryPreparationAccepted {
        preparation: EntryPreparation,
    },
    EntryPreparationSettled {
        operation: String,
    },
    EntryProofQuarantined {},
    HelperSutRunCompleted {
        witness: DesktopSutRunWitness,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        definition: Option<crate::v2::DesktopDefinitionKey>,
    },
    HelperTransitionAccepted {
        identity: BrokerOperationIdentity,
        transition: DesktopActivationTransition,
    },
    HelperActionAccepted {
        request: DesktopRequest,
        request_digest: String,
    },
    HelperStateObserved {
        identity: BrokerOperationIdentity,
        state: BrokerState,
    },
    HelperBrokerReleased {
        helper_lease: String,
        broker_lease: String,
    },
    BindAccepted {
        original: BrokerBind,
        request_digest: String,
    },
    BindIntentPublished {
        operation: String,
        scope: IntentPublicationScope,
    },
    BindingCommitted {
        receipt: BrokerBindingReceipt,
    },
    Accepted {
        request: DesktopRequest,
        request_digest: String,
    },
    EffectRecorded {
        operation: String,
        receipt: BrokerEffectReceipt,
    },
    OwnedLifetimeRecorded {
        operation: String,
        state: OwnedLifetimeState,
    },
    NativeLifetimeRecorded {
        operation: String,
        state: NativeLifetimeState,
    },
    LifetimeRecorded {
        operation: String,
        primary: BrokerPrimary,
        descendants: BrokerDescendants,
        cleanup_complete: bool,
    },
    Quarantined {
        operation: String,
    },
}
#[derive(Clone, Debug)]
pub(super) struct Accepted {
    pub(super) request: DesktopRequest,
    pub(super) state: BrokerState,
    receipt: bool,
    quarantined: bool,
}
/// The externally visible state of one accepted request at the current replay
/// point: a RequestClose carries its own effect and quarantine but inherits the
/// lifetime facts of its original activation as they stand now.
fn projected_state(all: &[Accepted], accepted: &Accepted) -> BrokerState {
    let mut state = accepted.state.clone();
    if let DesktopAction::RequestClose {
        original_activation_operation,
        ..
    } = &accepted.request.action
        && let Some(lifetime) = all
            .iter()
            .find(|a| {
                matches!(a.request.action, DesktopAction::Activate { .. })
                    && &a.request.helper_operation == original_activation_operation
            })
            .map(|a| &a.state)
    {
        state.native_lifetime = lifetime.native_lifetime.clone();
        state.owned_lifetime = lifetime.owned_lifetime.clone();
        state.owner = lifetime.owner;
        state.primary = lifetime.primary;
        state.descendants = lifetime.descendants;
        state.cleanup_complete = lifetime.cleanup_complete;
        state.quarantined = accepted.quarantined || lifetime.quarantined;
    }
    state
}
/// A projection over the SAME LedgerStore records. Its in-memory table grants no
/// launch authority: all reopened incomplete native owners are lost.
#[derive(Clone, Debug, Default)]
pub struct BrokerLedgerView {
    pub(super) entry_preparations: Vec<(EntryPreparation, bool)>,
    pub(super) entry_quarantined: bool,
    pub(super) sut_runs: Vec<(
        DesktopSutRunWitness,
        Option<crate::v2::DesktopDefinitionKey>,
    )>,
    pub(super) sut_accepted_at: Vec<(String, u16)>,
    pub(super) helper_retired_at: Vec<(String, u16)>,
    pub(super) transitions: Vec<(BrokerOperationIdentity, DesktopActivationTransition)>,
    pub(super) accepted: Vec<Accepted>,
    pub(super) helper_actions: Vec<(DesktopRequest, Option<BrokerState>)>,
    pub(super) helper_released: bool,
    pub(super) binds: Vec<(
        BrokerBind,
        Option<IntentPublicationScope>,
        Option<BrokerBindingReceipt>,
    )>,
}
/// Verify the helper's original declared desktop ceiling against its complete
/// immutable integration header. An ordinary service-only IntegrationHeader
/// grants no desktop capability. No serialized declaration creates native owners.
pub fn declared_desktop(
    records: &[Record],
) -> Result<Option<(&DesktopManifest, &IntegrationBrokerHeader)>, LedgerError> {
    let declaration = records
        .iter()
        .filter(|r| matches!(r.entry(), Entry::DesktopAuthorityDeclared { .. }))
        .collect::<Vec<_>>();
    if declaration.is_empty() {
        return Ok(None);
    }
    if declaration.len() != 1 || declaration[0].index() != 1 {
        return Err(LedgerError::InvalidTransition);
    }
    let Entry::DesktopAuthorityDeclared { manifest, context } = declaration[0].entry() else {
        unreachable!()
    };
    let Some(Entry::IntegrationHeader {
        binding,
        manifest_digest,
        workspace_marker,
        os,
        provider_epoch,
        core_manifest_digest,
        integration_manifest_digest,
        context_digest,
        ownership_id,
        ..
    }) = records.first().map(Record::entry)
    else {
        return Err(LedgerError::InvalidTransition);
    };
    manifest
        .validate_context(context, *os)
        .map_err(|_| LedgerError::InvalidTransition)?;
    if manifest.core_manifest_digest != core_manifest_digest.to_hex()
        || manifest
            .digest()
            .map_err(|_| LedgerError::InvalidTransition)?
            != *integration_manifest_digest
        || crate::v2::integration_manifest_digest(
            *core_manifest_digest,
            *integration_manifest_digest,
        ) != *manifest_digest
        || context
            .digest()
            .map_err(|_| LedgerError::InvalidTransition)?
            != *context_digest
        || context.binding != binding.to_hex()
        || context.manifest_digest != manifest_digest.to_hex()
        || context.workspace_marker != workspace_marker.to_hex()
        || context.provider_epoch != provider_epoch.to_hex()
        || context.ownership_id != *ownership_id
    {
        return Err(LedgerError::InvalidTransition);
    }
    Ok(Some((
        manifest
            .desktop
            .as_ref()
            .ok_or(LedgerError::InvalidTransition)?,
        context
            .desktop_broker_header
            .as_ref()
            .ok_or(LedgerError::InvalidTransition)?,
    )))
}
impl BrokerLedgerView {
    pub fn entry_preparations(&self) -> &[(EntryPreparation, bool)] {
        &self.entry_preparations
    }
    pub fn entry_blocked(&self) -> bool {
        self.entry_quarantined || self.entry_preparations.iter().any(|(_, settled)| !settled)
    }
    pub fn entry_quarantined(&self) -> bool {
        self.entry_quarantined
    }
    pub fn replay(records: &[Record]) -> Result<Self, LedgerError> {
        let mut result = Self::default();
        let broker_header = match records.first().map(Record::entry) {
            Some(Entry::IntegrationBrokerHeader { header }) => {
                header.validate().map_err(|_| LedgerError::InvalidRecord)?;
                Some(header)
            }
            _ => None,
        };
        let declared = declared_desktop(records)?;
        let qualification = crate::v2::declared_linux_gio_qualification(records)?;
        let helper = matches!(
            records.first().map(Record::entry),
            Some(Entry::IntegrationHeader { .. })
        );
        let lease = records.iter().find_map(|r| match r.entry() {
            Entry::LeaseReserved { lease, .. } => Some(lease.to_hex()),
            _ => None,
        });
        for (index, record) in records.iter().enumerate() {
            if usize::from(record.index()) != index {
                return Err(LedgerError::InvalidTransition);
            }
            let Entry::Broker { event } = record.entry() else {
                if matches!(record.entry(), Entry::LeaseClosed { .. }) && result.entry_blocked() {
                    return Err(LedgerError::InvalidTransition);
                }
                continue;
            };
            let invalid = || LedgerError::InvalidTransition;
            match event {
                BrokerEvent::EntryPreparationAccepted { preparation } => {
                    preparation.validate().map_err(|_| invalid())?;
                    let header = broker_header.ok_or_else(invalid)?;
                    if result.entry_blocked()
                        // A RequestClose inherits its original activation's
                        // lifetime; judge it on the same projection queries use,
                        // not on the snapshot captured when the close was accepted.
                        || result
                            .accepted
                            .iter()
                            .any(|a| !projected_state(&result.accepted, a).original_retired())
                        || lease.as_deref() != Some(&preparation.broker_lease)
                        || header.digest().map_err(|_| invalid())?.to_hex()
                            != preparation.header_digest
                        || result.entry_preparations.len() >= crate::v2::MAX_OPERATIONS
                        || result
                            .entry_preparations
                            .iter()
                            .any(|(p, _)| p.operation == preparation.operation)
                    {
                        return Err(invalid());
                    }
                    result.entry_preparations.push((preparation.clone(), false));
                }
                BrokerEvent::EntryPreparationSettled { operation } => {
                    if broker_header.is_none() || result.entry_quarantined {
                        return Err(invalid());
                    }
                    let (_, settled) = result
                        .entry_preparations
                        .iter_mut()
                        .find(|(p, _)| &p.operation == operation)
                        .ok_or_else(invalid)?;
                    if *settled {
                        return Err(invalid());
                    }
                    *settled = true;
                }
                BrokerEvent::EntryProofQuarantined {} => {
                    if broker_header.is_none() {
                        return Err(invalid());
                    }
                    result.entry_quarantined = true;
                }
                BrokerEvent::HelperSutRunCompleted {
                    witness,
                    definition,
                } => {
                    if !helper || result.helper_released || declared.is_none() {
                        return Err(invalid());
                    }
                    witness
                        .matches_successful_terminal(&records[..index])
                        .map_err(|_| invalid())?;
                    let previous = result.sut_runs.iter().rev().find_map(|(_, key)| *key);
                    if witness.definition_after(previous).map_err(|_| invalid())? != *definition
                        || result
                            .sut_runs
                            .iter()
                            .any(|(old, _)| old.operation == witness.operation)
                    {
                        return Err(invalid());
                    }
                    let accepted=records[..usize::from(record.index())].iter().find(|r|matches!(r.entry(),Entry::OperationAccepted {operation,request_digest,operation_kind:crate::v2::OperationKind::Run,..} if operation.to_hex()==witness.operation && request_digest.to_hex()==witness.request_digest)).ok_or_else(invalid)?;
                    result
                        .sut_accepted_at
                        .push((witness.operation.clone(), accepted.index()));
                    result.sut_runs.push((witness.clone(), *definition));
                }
                BrokerEvent::HelperTransitionAccepted {
                    identity,
                    transition,
                } => {
                    if !helper || result.helper_released {
                        return Err(invalid());
                    }
                    let request = result
                        .helper_actions
                        .iter()
                        .find(|(r, _)| r.identity().as_ref() == Ok(identity))
                        .map(|(r, _)| r)
                        .ok_or_else(invalid)?;
                    if request.transition() != Some(transition)
                        || result
                            .transitions
                            .iter()
                            .any(|(old, _)| old.helper_operation == identity.helper_operation)
                    {
                        return Err(invalid());
                    }
                    result
                        .transitions
                        .push((identity.clone(), transition.clone()));
                }
                BrokerEvent::HelperActionAccepted {
                    request,
                    request_digest,
                } => {
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
                        || request.binding_digest
                            != header.digest().map_err(|_| invalid())?.to_hex()
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
                                    Entry::DesktopAuthorityDeclared { manifest, .. } => {
                                        Some(manifest.as_ref())
                                    }
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
                }
                BrokerEvent::HelperStateObserved { identity, state } => {
                    identity.validate().map_err(|_| invalid())?;
                    state.validate().map_err(|_| invalid())?;
                    if result.helper_actions.iter().any(|(request, _)| {
                        request.identity().as_ref() == Ok(identity)
                            && request.transition().is_some()
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
                        && ((matches!(old.effect, BrokerEffect::Receipt { .. })
                            && old.effect != state.effect)
                            || (old.primary == BrokerPrimary::Exited
                                && state.primary != BrokerPrimary::Exited)
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
                            (None, Some(_))
                                if old.owner == BrokerOwner::Lost || old.quarantined =>
                            {
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
                }
                BrokerEvent::HelperBrokerReleased {
                    helper_lease,
                    broker_lease,
                } => {
                    let receipt = result
                        .binds
                        .first()
                        .and_then(|b| b.2.as_ref())
                        .ok_or_else(invalid)?;
                    if !helper
                        || result.helper_released
                        || &receipt.original.helper_lease != helper_lease
                        || &receipt.broker_lease != broker_lease
                        || result
                            .helper_actions
                            .iter()
                            .any(|(_, s)| s.as_ref().is_none_or(|s| !s.original_retired()))
                    {
                        return Err(invalid());
                    }
                    result.helper_released = true;
                }
                BrokerEvent::BindAccepted {
                    original,
                    request_digest,
                } => {
                    original.validate().map_err(|_| invalid())?;
                    if original.expected_header.linux_gio_qualification.is_some()
                        && qualification.is_none()
                    {
                        return Err(invalid());
                    }
                    let header_matches = matches!(records.first().map(Record::entry), Some(Entry::IntegrationHeader { manifest_digest, provider_epoch, ownership_id, .. })
                        if original.expected_header.manifest_digest == manifest_digest.to_hex() && original.expected_header.provider_epoch == provider_epoch.to_hex() && original.ownership_id == *ownership_id);
                    let declared_matches = declared.is_some_and(|(desktop, header)| {
                        header == &original.expected_header && desktop.slot == original.slot
                    });
                    if !helper
                        || !declared_matches
                        || !header_matches
                        || lease.as_ref() != Some(&original.helper_lease)
                        || original.digest().map_err(|_| invalid())?.to_hex() != *request_digest
                        || !result.binds.is_empty()
                    {
                        return Err(invalid());
                    }
                    result.binds.push((original.clone(), None, None));
                }
                BrokerEvent::BindIntentPublished { operation, scope } => {
                    let entry = result
                        .binds
                        .iter_mut()
                        .find(|(b, s, r)| {
                            b.helper_operation == *operation && s.is_none() && r.is_none()
                        })
                        .ok_or_else(invalid)?;
                    entry.1 = Some(*scope);
                }
                BrokerEvent::BindingCommitted { receipt } => {
                    receipt.validate().map_err(|_| invalid())?;
                    let entry = result
                        .binds
                        .iter_mut()
                        .find(|(b, s, r)| {
                            b == &receipt.original
                                && *s == Some(receipt.publication_scope)
                                && r.is_none()
                        })
                        .ok_or_else(invalid)?;
                    entry.2 = Some(receipt.clone());
                }
                BrokerEvent::Accepted {
                    request,
                    request_digest,
                } => {
                    request.validate().map_err(|_| invalid())?;
                    let header = broker_header.ok_or_else(invalid)?;
                    if lease.as_ref() != Some(&request.broker_lease)
                        || request.binding_digest
                            != header.digest().map_err(|_| invalid())?.to_hex()
                        || request.digest().map_err(|_| invalid())?.to_hex() != *request_digest
                        || result.accepted.len() >= crate::v2::MAX_OPERATIONS
                        || result.accepted.iter().any(|a| {
                            a.request.helper_operation == request.helper_operation || a.quarantined
                        })
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
                                .find(|a| {
                                    &a.request.helper_operation == original_activation_operation
                                })
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
                }
                BrokerEvent::EffectRecorded { operation, receipt } => {
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
                }
                BrokerEvent::OwnedLifetimeRecorded { operation, state } => {
                    state
                        .validate_platform(&broker_header.ok_or_else(invalid)?.os)
                        .map_err(|_| invalid())?;
                    let accepted = result
                        .accepted
                        .iter_mut()
                        .find(|a| a.request.helper_operation == *operation)
                        .ok_or_else(invalid)?;
                    if !matches!(&accepted.request.action, DesktopAction::Activate { mode: DesktopMode::OwnedExecutable, owned_lifetime: Some(definition), .. } if definition == &state.definition)
                    {
                        return Err(invalid());
                    }
                    accepted
                        .state
                        .record_owned_lifetime(state.clone())
                        .map_err(|_| invalid())?;
                    accepted.state.quarantined = accepted.quarantined;
                }
                BrokerEvent::NativeLifetimeRecorded { operation, state } => {
                    state
                        .validate_platform(&broker_header.ok_or_else(invalid)?.os)
                        .map_err(|_| invalid())?;
                    let accepted = result
                        .accepted
                        .iter_mut()
                        .find(|a| a.request.helper_operation == *operation)
                        .ok_or_else(invalid)?;
                    if !matches!(
                        accepted.request.action,
                        DesktopAction::Activate {
                            mode: DesktopMode::NativeActivation,
                            ..
                        }
                    ) || (accepted.quarantined && accepted.state.native_lifetime.is_none())
                        || accepted.state.cleanup_complete
                    {
                        return Err(invalid());
                    }
                    if let Some(old) = &accepted.state.native_lifetime {
                        state.validate_progress_from(old).map_err(|_| invalid())?;
                    }
                    accepted.state.native_lifetime = Some(state.clone());
                    if state.primary == NativePrimaryState::Exited {
                        accepted.state.primary = BrokerPrimary::Exited;
                    }
                    if state.local_retired() {
                        accepted.state.owner = BrokerOwner::Settled;
                    }
                    accepted.state.quarantined = accepted.quarantined;
                }
                BrokerEvent::LifetimeRecorded {
                    operation,
                    primary,
                    descendants,
                    cleanup_complete,
                } => {
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
                            && (!matches!(
                                primary,
                                BrokerPrimary::Exited | BrokerPrimary::NotCreated
                            ) || !matches!(
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
                }
                BrokerEvent::Quarantined { operation } => {
                    let accepted = result
                        .accepted
                        .iter_mut()
                        .find(|a| a.request.helper_operation == *operation)
                        .ok_or_else(invalid)?;
                    if accepted.quarantined {
                        return Err(invalid());
                    }
                    accepted.quarantined = true;
                    accepted.state.quarantined = true;
                }
            }
        }
        let projected: Vec<_> = result
            .accepted
            .iter()
            .map(|accepted| projected_state(&result.accepted, accepted))
            .collect();
        for (accepted, state) in result.accepted.iter_mut().zip(projected) {
            accepted.state = state;
        }
        if helper {
            let mut ids = std::collections::BTreeSet::new();
            for id in result
                .binds
                .iter()
                .map(|b| b.0.helper_operation.clone())
                .chain(
                    result
                        .helper_actions
                        .iter()
                        .map(|(r, _)| r.helper_operation.clone()),
                )
                .chain(records.iter().filter_map(|r| match r.entry() {
                    Entry::OperationAccepted { operation, .. } => Some(operation.to_hex()),
                    _ => None,
                }))
            {
                if !ids.insert(id) || ids.len() > crate::v2::MAX_OPERATIONS {
                    return Err(LedgerError::Capacity);
                }
            }
        }
        Ok(result)
    }
    pub fn query(&self, request: &DesktopRequest) -> Result<BrokerState, Error> {
        request.validate()?;
        let accepted = self
            .accepted
            .iter()
            .find(|a| a.request.helper_operation == request.helper_operation)
            .ok_or(Error::InvalidValue)?;
        if accepted.request != *request {
            return Err(Error::InvalidValue);
        }
        Ok(accepted.state.clone())
    }
    pub fn original(&self, operation: &str) -> Option<&DesktopRequest> {
        self.accepted
            .iter()
            .find(|a| a.request.helper_operation == operation)
            .map(|a| &a.request)
    }
    pub fn accepted_count(&self) -> usize {
        self.accepted.len()
    }
    pub fn requests(&self) -> impl Iterator<Item = &DesktopRequest> {
        self.accepted.iter().map(|a| &a.request)
    }
    pub fn binding(&self, original: &BrokerBind) -> Result<Option<&BrokerBindingReceipt>, Error> {
        let (stored, _, receipt) = self
            .binds
            .iter()
            .find(|(b, _, _)| b.helper_operation == original.helper_operation)
            .ok_or(Error::InvalidValue)?;
        if stored != original {
            return Err(Error::InvalidValue);
        }
        Ok(receipt.as_ref())
    }
    pub fn binding_original(&self) -> Option<&BrokerBind> {
        self.binds.first().map(|b| &b.0)
    }
    pub fn binding_scope(&self, original: &BrokerBind) -> Option<IntentPublicationScope> {
        self.binds
            .iter()
            .find(|(b, _, _)| b == original)
            .and_then(|b| b.1)
    }
    pub fn helper_request(
        &self,
        identity: &BrokerOperationIdentity,
    ) -> Result<Option<&DesktopRequest>, Error> {
        let found = self
            .helper_actions
            .iter()
            .find(|(r, _)| r.helper_operation == identity.helper_operation);
        match found {
            Some((r, _)) if r.identity()? == *identity => Ok(Some(r)),
            Some(_) => Err(Error::InvalidValue),
            None => Ok(None),
        }
    }
    pub fn helper_state(
        &self,
        identity: &BrokerOperationIdentity,
    ) -> Result<Option<&BrokerState>, Error> {
        self.helper_request(identity)?;
        Ok(self
            .helper_actions
            .iter()
            .find(|(r, _)| r.helper_operation == identity.helper_operation)
            .and_then(|(_, s)| s.as_ref()))
    }
    pub fn helper_released(&self) -> bool {
        self.helper_released
    }
    pub fn is_quarantined(&self, operation: &str) -> bool {
        self.accepted
            .iter()
            .any(|a| a.request.helper_operation == operation && a.quarantined)
    }
}
