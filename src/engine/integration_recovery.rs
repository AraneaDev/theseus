//! Original-ID integration recovery. Saved evidence never supplies execution authority.
use super::{
    CancellationToken, EngineEvent, NativeRecoveryResult,
    domain::{StoredJournal, StoredRun},
    projector::RunProjector,
    recover_native,
};
use crate::{
    native::{
        integration::model::*,
        launch::LaunchAuthority,
        model::{Disposition, NativeResource},
        transport::NativeSession,
    },
    report::ReportStore,
};
use anyhow::{Result, bail, ensure};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use theseus_native_client::{Action, Event, Poll, RequestId};
use theseus_native_protocol::v2;

// Public schema-specific DTOs deliberately retain the binding-spec variants.
#[allow(clippy::large_enum_variant)]
pub enum SelectedNativeRecoveryResult {
    CatalogV1(NativeRecoveryResult),
    IntegrationV2(IntegrationRecoveryResult),
}
pub struct IntegrationRecoveryResult {
    pub report: NativeIntegrationReport,
    pub journal: NativeIntegrationJournal,
    pub report_path: PathBuf,
    pub prior_local: Vec<Disposition>,
    pub local: Disposition,
    pub remote: Disposition,
    pub resources: Vec<RecoveredResourceDisposition>,
}
#[derive(Debug, Clone)]
pub struct RecoveredResourceDisposition {
    pub ownership_id: String,
    pub slot: ResourceSlot,
    pub original_action_ids: Vec<String>,
    pub local: Disposition,
    pub remote: Disposition,
    pub broker: Disposition,
}
fn complete(state: &Disposition) -> bool {
    matches!(state, Disposition::Complete { .. })
}
#[cfg(test)]
fn settled(reason: &str) -> Disposition {
    Disposition::Complete {
        receipt: reason.into(),
    }
}
fn unknown(reason: &str) -> Disposition {
    Disposition::Unknown {
        reason: reason.into(),
    }
}
impl IntegrationRecoveryResult {
    pub fn complete(&self) -> bool {
        complete(&self.local)
            && complete(&self.remote)
            && self.prior_local.iter().all(complete)
            && self
                .resources
                .iter()
                .all(|r| complete(&r.local) && complete(&r.remote) && complete(&r.broker))
            && self.report.integrations.iter().all(|i| i.complete())
            && self.report.catalog.scenarios.iter().all(|s| {
                [
                    &s.dispositions.local,
                    &s.dispositions.remote,
                    &s.dispositions.release,
                    &s.dispositions.reset,
                ]
                .into_iter()
                .all(complete)
            })
            && self
                .journal
                .operations
                .iter()
                .all(|o| complete(&o.completion))
            && self
                .journal
                .releases
                .iter()
                .all(|r| complete(&r.completion))
            && self
                .journal
                .integration_actions
                .iter()
                .all(|a| complete(&a.completion))
            && self
                .journal
                .integration_ownership
                .iter()
                .all(|r| r.revocation.is_none() && complete(&r.disposition))
            && self
                .journal
                .readiness_probes
                .iter()
                .all(|p| complete(&p.completion))
            && self
                .journal
                .broker_bindings
                .iter()
                .all(|b| complete(&b.disposition))
            && self
                .journal
                .recovery_attempts
                .iter()
                .all(|a| complete(&a.prior_local) && complete(&a.local) && complete(&a.remote))
            && self.journal.integration_recoveries.iter().all(|r| {
                r.prior_local.iter().all(complete)
                    && complete(&r.local)
                    && complete(&r.broker)
                    && complete(&r.provider)
            })
    }
}
impl SelectedNativeRecoveryResult {
    pub fn report_path(&self) -> &Path {
        match self {
            Self::CatalogV1(r) => &r.report_path,
            Self::IntegrationV2(r) => &r.report_path,
        }
    }
    pub fn complete(&self) -> bool {
        match self {
            Self::CatalogV1(r) => r.complete(),
            Self::IntegrationV2(r) => r.complete(),
        }
    }
}
pub fn recover_selected_native(
    store: ReportStore,
    launch: &LaunchAuthority,
    cancel: CancellationToken,
    emit: impl FnMut(EngineEvent),
) -> Result<SelectedNativeRecoveryResult> {
    let (report, journal) = crate::native::storage::load_native_versioned(&store)?;
    match (report, journal) {
        (NativeReportVersion::CatalogV1(_), NativeJournalVersion::CatalogV1(_)) => Ok(
            SelectedNativeRecoveryResult::CatalogV1(recover_native(store, launch, cancel, emit)?),
        ),
        (
            NativeReportVersion::IntegrationV2(report),
            NativeJournalVersion::IntegrationV2(journal),
        ) => {
            validate_pair(&report, &journal)?;
            ensure!(
                journal.launch_fingerprint.as_deref() == Some(launch.view().fingerprint.as_str()),
                "recovery requires unchanged explicit launch fingerprint"
            );
            let (core, integration) = launch.integration_components().ok_or_else(|| {
                anyhow::anyhow!("integration recovery requires explicit integration authority")
            })?;
            ensure!(
                core.to_hex() == journal.core_manifest_digest
                    && integration.to_hex() == journal.integration_manifest_digest
                    && launch.binding().manifest_digest.to_hex() == journal.manifest_digest,
                "recovery integration manifest changed"
            );
            let recovered = super::recovery::recover_stored_native(
                store,
                launch,
                cancel,
                emit,
                StoredRun::NativeIntegration(report),
                StoredJournal::NativeIntegration(journal),
            )?;
            let StoredRun::NativeIntegration(report) = recovered.report else {
                unreachable!()
            };
            let StoredJournal::NativeIntegration(journal) = recovered.journal else {
                unreachable!()
            };
            let resources = resource_dispositions(&report, &journal);
            let mut prior_local = recovered.prior_local;
            for attempt in &journal.integration_recoveries {
                prior_local.extend(attempt.prior_local.iter().cloned());
            }
            Ok(SelectedNativeRecoveryResult::IntegrationV2(
                IntegrationRecoveryResult {
                    report,
                    journal,
                    report_path: recovered.report_path,
                    prior_local,
                    local: recovered.local,
                    remote: recovered.remote,
                    resources,
                },
            ))
        }
        _ => bail!("native report/journal version mismatch"),
    }
}

fn action_reference(action: &IntegrationActionRecord) -> ActionReference {
    ActionReference {
        operation: action.operation.clone(),
        request_digest: action.request_digest.clone(),
    }
}
struct OriginalActionQuery {
    outcome: IntegrationOutcome,
    cached_terminal: bool,
}
/// Query an original canonical identity, cancelling only that accepted owner.
/// The injected operation is the transport boundary; this routine has no API
/// for publishing or replaying a service, catalog or broker effect.
fn query_original_action(
    original: &IntegrationActionRecord,
    cancel: &CancellationToken,
    mut request: impl FnMut(Action<'_>) -> Result<v2::Reply>,
) -> Result<OriginalActionQuery> {
    original.validate_identity()?;
    let lease = v2::LeaseId::parse(&original.lease)?;
    let operation = v2::OperationId::parse(&original.operation)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut cancelled = false;
    loop {
        if Instant::now() >= deadline || cancel.is_cancelled() {
            return Ok(OriginalActionQuery {
                outcome: IntegrationOutcome::Unknown {
                    reason:
                        "original integration settlement unproved at recovery deadline/cancellation"
                            .into(),
                },
                cached_terminal: false,
            });
        }
        let state = match request(Action::Query { lease, operation })? {
            v2::Reply::Operation {
                lease: l,
                operation: o,
                state,
            } if l == lease && o == operation => state,
            v2::Reply::Refused {
                code: v2::RefusalCode::OwnershipPending,
                ..
            } => {
                std::thread::yield_now();
                continue;
            }
            _ => bail!("original integration query identity refused"),
        };
        let cached_terminal = matches!(state, v2::OperationView::Finished { .. });
        let result = match state {
            v2::OperationView::Finished {
                outcome: v2::Outcome::ServiceActionCompleted { outcome },
            } => Some(match outcome {
                v2::ServiceOutcome::Complete { receipt } => {
                    IntegrationOutcome::Complete { receipt }
                }
                v2::ServiceOutcome::Failed { reason } => IntegrationOutcome::Failed { reason },
                v2::ServiceOutcome::Unknown { reason } => IntegrationOutcome::Unknown { reason },
            }),
            v2::OperationView::Finished {
                outcome: v2::Outcome::NotStarted { .. },
            } => Some(IntegrationOutcome::Failed {
                reason: "original integration operation was not started; never replayed".into(),
            }),
            v2::OperationView::Finished { .. } => {
                bail!("original integration terminal outcome kind changed")
            }
            v2::OperationView::Missing => Some(IntegrationOutcome::Unknown {
                reason: "original integration action missing; native owner cannot be reconstructed"
                    .into(),
            }),
            v2::OperationView::Unknown { reason } => Some(IntegrationOutcome::Unknown {
                reason: format!("original integration owner unknown: {reason:?}; never replayed"),
            }),
            v2::OperationView::Accepted {
                operation_kind: v2::OperationKind::ServiceAction,
            } => None,
            v2::OperationView::Accepted { .. } | v2::OperationView::Running => {
                bail!("original integration operation kind changed")
            }
        };
        if let Some(result) = result {
            if let Some(known) = &original.terminal {
                // Any previously cached outcome is immutable, including Unknown.
                ensure!(
                    *known == result,
                    "original integration terminal outcome changed"
                );
            } else {
                ensure!(
                    !matches!(
                        original.completion,
                        Disposition::Complete { .. } | Disposition::Failed { .. }
                    ),
                    "original integration disposition lacks exact terminal evidence"
                );
            }
            return Ok(OriginalActionQuery {
                outcome: result,
                cached_terminal,
            });
        }
        if !cancelled {
            match request(Action::Cancel { lease, operation })? {
                v2::Reply::Operation {
                    lease: l,
                    operation: o,
                    ..
                } if l == lease && o == operation => cancelled = true,
                v2::Reply::Refused {
                    code: v2::RefusalCode::OwnershipPending,
                    ..
                } => {}
                _ => bail!("original integration cancellation identity refused"),
            }
        }
        std::thread::yield_now();
    }
}
fn request(session: &mut NativeSession, action: Action<'_>) -> Result<v2::Reply> {
    let id: RequestId = session.submit(action)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        ensure!(
            Instant::now() < deadline,
            "integration recovery reply unproved at deadline"
        );
        match session.poll()? {
            Poll::Event(Event::Reply { request, reply }) if request == id => return Ok(reply),
            Poll::Event(Event::ConnectionLost | Event::PeerClosed(_)) | Poll::TransportSettled => {
                bail!("integration recovery transport lost; original owner retained")
            }
            _ => std::thread::yield_now(),
        }
    }
}
pub(crate) fn publish_attempt(
    projector: &mut RunProjector<'_>,
    recovery_id: &str,
    owner: &str,
) -> Result<()> {
    let StoredJournal::NativeIntegration(j) = projector.journal() else {
        bail!("schema2 recovery required")
    };
    let original_actions = j
        .integration_actions
        .iter()
        .filter(|a| a.ownership_id == owner)
        .map(action_reference)
        .collect();
    let core_attempt = j
        .recovery_attempts
        .iter()
        .find(|a| a.recovery_id == recovery_id && a.ownership_id == owner)
        .ok_or_else(|| anyhow::anyhow!("integration recovery lacks original core publication"))?;
    let mut prior_local = vec![core_attempt.prior_local.clone()];
    for prior in j
        .integration_recoveries
        .iter()
        .filter(|r| r.ownership_id == owner)
    {
        prior_local.push(prior.local.clone());
        prior_local.extend(prior.prior_local.iter().cloned());
    }
    let mut retained = Vec::new();
    for prior in prior_local {
        if !retained.contains(&prior) {
            retained.push(prior);
        }
    }
    let prior_local = retained;
    let StoredRun::NativeIntegration(r) = projector.report() else {
        bail!("schema2 recovery required")
    };
    let evidence = r
        .integrations
        .iter()
        .find(|i| i.ownership_id == owner)
        .ok_or_else(|| anyhow::anyhow!("integration original scenario missing"))?;
    // The broker is a separate native owner, never reconstructed from a receipt.
    let broker = evidence.dispositions.broker.clone();
    let broker_effect_baselines = original_broker_effect_baselines(j, owner)?;
    projector.publish_integration_recovery(IntegrationRecoveryRecord {
        recovery_id: recovery_id.into(),
        ownership_id: owner.into(),
        original_actions,
        prior_local,
        local: unknown("integration recovery published; new local settlement unproved"),
        broker,
        provider: unknown("original provider disposition not queried"),
        broker_effect_baselines,
        queries: vec![],
        broker_observations: vec![],
        broker_release: None,
    })
}
#[cfg(test)]
fn query_original_desktop(
    original: &IntegrationActionRecord,
    cancel: &CancellationToken,
    request: impl FnMut(Action<'_>) -> Result<v2::Reply>,
) -> Result<OriginalActionQuery> {
    query_original_desktop_observing(original, cancel, request, |_| Ok(()))
}
fn desktop_effect_query(
    original: &IntegrationActionRecord,
    state: Option<&v2::broker::BrokerState>,
) -> Result<OriginalActionQuery> {
    use v2::broker::BrokerEffect;
    let (outcome, cached_terminal) = match state.map(|s| &s.effect) {
        Some(BrokerEffect::Receipt { receipt }) if receipt.acknowledged => (
            IntegrationOutcome::Complete {
                receipt: receipt.receipt_digest.clone(),
            },
            true,
        ),
        Some(BrokerEffect::Receipt { .. }) => (
            IntegrationOutcome::Failed {
                reason: "original desktop effect was not acknowledged".into(),
            },
            true,
        ),
        _ => (
            IntegrationOutcome::Unknown {
                reason:
                    "original broker effect remains unproved; no activation or close reconstructed"
                        .into(),
            },
            false,
        ),
    };
    if let Some(terminal) = &original.terminal {
        ensure!(
            terminal == &outcome,
            "original desktop cached terminal changed"
        );
    }
    Ok(OriginalActionQuery {
        outcome,
        cached_terminal,
    })
}
fn query_original_desktop_observing(
    original: &IntegrationActionRecord,
    cancel: &CancellationToken,
    mut request: impl FnMut(Action<'_>) -> Result<v2::Reply>,
    mut observe: impl FnMut(RecoveredBrokerObservation) -> Result<()>,
) -> Result<OriginalActionQuery> {
    use v2::broker::{BrokerOwner, DesktopAction};
    original.validate_identity()?;
    let IntegrationRequest::Desktop {
        request: canonical, ..
    } = &original.request
    else {
        bail!("original desktop request missing")
    };
    let identity = canonical.identity()?;
    let end = Instant::now() + Duration::from_secs(30);
    let mut last = None;
    loop {
        if cancel.is_cancelled() || Instant::now() >= end {
            return desktop_effect_query(original, last.as_ref());
        }
        let state = match request(Action::QueryDesktop {
            identity: identity.clone(),
        })? {
            v2::Reply::DesktopState {
                identity: observed,
                state,
            } if *observed == identity => *state,
            v2::Reply::Refused { .. } => return desktop_effect_query(original, last.as_ref()),
            _ => bail!("original desktop query identity changed"),
        };
        validate_original_broker_effect(original, &state)?;
        if let Some(previous) = &last {
            validate_broker_progress(previous, &state)?;
        }
        observe(RecoveredBrokerObservation {
            original: action_reference(original),
            identity: identity.clone(),
            source: BrokerObservationSource::Query,
            state: state.clone(),
        })?;
        last = Some(state.clone());
        if cancel.is_cancelled() || Instant::now() >= end {
            return desktop_effect_query(original, last.as_ref());
        }
        if state.owner == BrokerOwner::Retained
            && !state.cleanup_complete
            && matches!(canonical.action, DesktopAction::Activate { .. })
        {
            match request(Action::CancelDesktop {
                identity: identity.clone(),
            })? {
                v2::Reply::DesktopState {
                    identity: observed,
                    state: progress,
                } if *observed == identity => {
                    validate_original_broker_effect(original, &progress)?;
                    validate_broker_progress(&state, &progress)?;
                    observe(RecoveredBrokerObservation {
                        original: action_reference(original),
                        identity: identity.clone(),
                        source: BrokerObservationSource::Cancel,
                        state: *progress.clone(),
                    })?;
                    last = Some(*progress);
                }
                v2::Reply::Refused { .. } => {}
                _ => bail!("original desktop Cancel identity changed"),
            }
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }
        return desktop_effect_query(original, Some(&state));
    }
}
/// A closed helper lease is authenticated knowledge about the original release
/// barrier. It grants no authority to recreate a broker intent or application.
pub(crate) fn reconcile_broker_release(
    projector: &mut RunProjector<'_>,
    owner: &NativeResource,
    lease: v2::LeaseId,
    state: &v2::LeaseView,
    source: BrokerReleaseSource,
) -> Result<()> {
    let v2::LeaseView::AwaitingExternalReset { intent } = state else {
        return Ok(());
    };
    let StoredJournal::NativeIntegration(j) = projector.journal() else {
        return Ok(());
    };
    let bindings = j
        .broker_bindings
        .iter()
        .filter(|b| b.ownership_id == owner.ownership_id)
        .cloned()
        .collect::<Vec<_>>();
    if bindings.is_empty() {
        return Ok(());
    }
    let recovery_id = j
        .integration_recoveries
        .iter()
        .rev()
        .find(|r| r.ownership_id == owner.ownership_id)
        .ok_or_else(|| anyhow::anyhow!("original broker recovery publication missing"))?
        .recovery_id
        .clone();
    for binding in bindings {
        ensure!(
            binding.helper_lease == lease.to_hex(),
            "original broker helper lease changed"
        );
        let broker_lease = binding
            .broker_lease
            .ok_or_else(|| anyhow::anyhow!("original broker mapping unresolved"))?;
        projector.record_broker_recovery_release(
            &recovery_id,
            RecoveredBrokerRelease {
                helper_lease: lease.to_hex(),
                broker_lease,
                helper_intent: intent.to_hex(),
                source,
            },
        )?;
    }
    Ok(())
}
pub(crate) fn recover_actions(
    projector: &mut RunProjector<'_>,
    session: &mut NativeSession,
    owner: &NativeResource,
    lease: v2::LeaseId,
    cancel: &CancellationToken,
) -> Result<()> {
    let StoredJournal::NativeIntegration(j) = projector.journal() else {
        bail!("schema2 recovery required")
    };
    let recovery_id = j
        .integration_recoveries
        .iter()
        .rev()
        .find(|r| r.ownership_id == owner.ownership_id)
        .ok_or_else(|| anyhow::anyhow!("integration recovery not published"))?
        .recovery_id
        .clone();
    let bindings = j
        .broker_bindings
        .iter()
        .filter(|b| b.ownership_id == owner.ownership_id)
        .cloned()
        .collect::<Vec<_>>();
    for binding in bindings {
        let reply = request(
            session,
            Action::QueryBrokerBind {
                original: binding.original.clone(),
            },
        )?;
        if let v2::Reply::BrokerBinding { receipt } = reply {
            ensure!(
                receipt.original == binding.original,
                "original broker mapping query changed"
            );
            projector.record_broker_binding(&receipt)?;
        } else {
            bail!("original broker mapping unresolved; no Acquire or intent recreated");
        }
    }
    let StoredJournal::NativeIntegration(j) = projector.journal() else {
        bail!("schema2 recovery required")
    };
    let originals = j
        .integration_actions
        .iter()
        .filter(|a| a.ownership_id == owner.ownership_id)
        .cloned()
        .collect::<Vec<_>>();
    for original in originals {
        ensure!(
            original.lease == lease.to_hex(),
            "integration original lease changed"
        );
        let query = if matches!(original.request, IntegrationRequest::Desktop { .. }) {
            query_original_desktop_observing(
                &original,
                cancel,
                |action| request(session, action),
                |observation| {
                    projector.record_broker_recovery_observation(&recovery_id, observation)
                },
            )?
        } else {
            query_original_action(&original, cancel, |action| request(session, action))?
        };
        let outcome = query.outcome;
        projector.record_integration_query(
            &recovery_id,
            OriginalQueryOutcome {
                original: action_reference(&original),
                view: if query.cached_terminal {
                    OriginalQueryView::CachedTerminal
                } else {
                    OriginalQueryView::NonterminalUnknown
                },
                outcome: outcome.clone(),
            },
        )?;
        if original.terminal.is_none() && query.cached_terminal {
            let completion = match &outcome {
                IntegrationOutcome::Complete { receipt } => Disposition::Complete {
                    receipt: receipt.clone(),
                },
                IntegrationOutcome::Unknown { reason } => Disposition::Unknown {
                    reason: reason.clone(),
                },
                IntegrationOutcome::Failed { reason } => Disposition::Failed {
                    reason: reason.clone(),
                },
            };
            projector.record_integration_completion(
                &original.lease,
                &original.operation,
                completion,
                outcome,
            )?;
        }
    }
    Ok(())
}
pub(crate) fn finish_attempt(
    projector: &mut RunProjector<'_>,
    recovery_id: &str,
    local: Disposition,
    provider: Disposition,
) -> Result<()> {
    let StoredJournal::NativeIntegration(j) = projector.journal() else {
        bail!("schema2 recovery required")
    };
    let record = j
        .integration_recoveries
        .iter()
        .find(|r| r.recovery_id == recovery_id)
        .ok_or_else(|| anyhow::anyhow!("integration recovery not published"))?;
    let broker = record.broker.clone();
    projector.finish_integration_recovery(recovery_id, local, broker, provider)?;
    // This seam cannot choose a schema or replace an outer envelope. Provider
    // disposal changes only its own independent catalog disposition.
    projector.with_catalog_evidence_mut(|catalog, _core| {
        catalog.outcome =
            crate::model::RunOutcome::aggregate(catalog.scenarios.iter().map(|s| s.outcome));
        Ok(())
    })?;
    projector.save_report()
}
fn resource_dispositions(
    report: &NativeIntegrationReport,
    journal: &NativeIntegrationJournal,
) -> Vec<RecoveredResourceDisposition> {
    journal.integration_ownership.iter().map(|resource| {
        let id = &resource.key.ownership_id;
        let evidence = report.integrations.iter().find(|i| &i.ownership_id == id);
        let actions = journal.integration_actions.iter().filter(|a| &a.ownership_id == id && a.slot == resource.key.slot).collect::<Vec<_>>();
        let core = journal.operations.iter().filter(|operation| &operation.ownership_id == id
            && resource.binding_history.iter().any(|binding| binding.original_action.as_ref() == Some(&operation.operation_id))).collect::<Vec<_>>();
        let latest = journal.integration_recoveries.iter().rev().find(|r| &r.ownership_id == id);
        let local = latest.filter(|r| r.prior_local.iter().all(complete))
            .map(|r| r.local.clone()).unwrap_or_else(|| unknown("original resource controller/recovery local owner remains unproved"));
        let mut mandatory = vec![&resource.disposition];
        mandatory.extend(actions.iter().map(|a| &a.completion));
        mandatory.extend(core.iter().map(|o| &o.completion));
        if let Some(evidence) = evidence {
            mandatory.extend([&evidence.dispositions.helper_worker,
                &evidence.dispositions.registration, &evidence.dispositions.manager_action]);
        }
        let remote = if resource.revocation.is_some() || evidence.is_none() {
            unknown("original resource authority revoked or helper evidence missing; provider disposal does not clear it")
        } else if let Some(state) = mandatory.iter().find(|d| matches!(d, Disposition::Unknown { .. })) {
            (*state).clone()
        } else if mandatory.iter().any(|d| matches!(d, Disposition::NotStarted)) {
            unknown("original mandatory native disposition was not started or proved")
        } else if let Some(state) = mandatory.iter().find(|d| matches!(d, Disposition::Failed { .. })) {
            (*state).clone()
        } else {
            resource.disposition.clone()
        };
        let mut original_action_ids = Vec::new();
        for operation in core.iter().map(|o| &o.operation_id).chain(actions.iter().map(|a| &a.operation)) {
            if !original_action_ids.contains(operation) { original_action_ids.push(operation.clone()); }
        }
        RecoveredResourceDisposition {
            ownership_id: id.clone(), slot: resource.key.slot.clone(), original_action_ids, local, remote,
            broker: evidence.map(|i| i.dispositions.broker.clone()).unwrap_or_else(|| unknown("original broker disposition missing")),
        }
    }).collect()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod desktop_progress_review_tests;
