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
mod tests {
    use super::*;
    use theseus_native_protocol::v2;

    fn original() -> IntegrationActionRecord {
        let request = IntegrationRequest::Service {
            lease: "01".repeat(16),
            operation: "02".repeat(16),
            slot: ResourceSlot::new("service").unwrap(),
            prior_binding_digest: "03".repeat(32),
            control: ServiceAction::Start,
        };
        IntegrationActionRecord {
            ownership_id: "owner".into(),
            lease: "01".repeat(16),
            slot: ResourceSlot::new("service").unwrap(),
            operation: "02".repeat(16),
            request_digest: v2::request_digest(&request.wire().unwrap())
                .unwrap()
                .to_hex(),
            request,
            prior_binding_digest: "03".repeat(32),
            broker_identity: None,
            completion: Disposition::Unknown {
                reason: "reply lost".into(),
            },
            terminal: None,
        }
    }
    #[test]
    fn pending_original_control_is_cancelled_and_queried_without_replay() {
        let original = original();
        let lease = v2::LeaseId::parse(&original.lease).unwrap();
        let operation = v2::OperationId::parse(&original.operation).unwrap();
        let mut trace = vec![];
        let result = query_original_action(&original, &CancellationToken::default(), |action| {
            let state = match action {
                theseus_native_client::Action::Query {
                    lease: l,
                    operation: o,
                } => {
                    assert_eq!((l, o), (lease, operation));
                    trace.push("query");
                    if trace.len() == 1 {
                        v2::OperationView::Accepted {
                            operation_kind: v2::OperationKind::ServiceAction,
                        }
                    } else {
                        v2::OperationView::Finished {
                            outcome: v2::Outcome::ServiceActionCompleted {
                                outcome: v2::ServiceOutcome::Complete {
                                    receipt: "04".repeat(32),
                                },
                            },
                        }
                    }
                }
                theseus_native_client::Action::Cancel {
                    lease: l,
                    operation: o,
                } => {
                    assert_eq!((l, o), (lease, operation));
                    trace.push("cancel");
                    v2::OperationView::Accepted {
                        operation_kind: v2::OperationKind::ServiceAction,
                    }
                }
                _ => panic!("recovery submitted a fresh effect"),
            };
            Ok(v2::Reply::Operation {
                lease,
                operation,
                state,
            })
        })
        .unwrap();
        assert_eq!(trace, ["query", "cancel", "query"]);
        assert_eq!(
            result.outcome,
            IntegrationOutcome::Complete {
                receipt: "04".repeat(32)
            }
        );
        assert!(result.cached_terminal);
    }
    #[test]
    fn lost_native_owner_stays_unknown_without_cancel_or_replacement() {
        let original = original();
        let mut count = 0;
        let result = query_original_action(&original, &CancellationToken::default(), |action| {
            assert!(matches!(
                action,
                theseus_native_client::Action::Query { .. }
            ));
            count += 1;
            Ok(v2::Reply::Operation {
                lease: v2::LeaseId::parse(&original.lease).unwrap(),
                operation: v2::OperationId::parse(&original.operation).unwrap(),
                state: v2::OperationView::Unknown {
                    reason: v2::UnknownReason::HelperRestartedWithUnfinishedOperation,
                },
            })
        })
        .unwrap();
        assert_eq!(count, 1);
        assert!(matches!(result.outcome, IntegrationOutcome::Unknown { .. }));
        assert!(
            !result.cached_terminal,
            "lost owner must not fabricate a cached terminal outcome"
        );
    }
    #[test]
    fn queried_terminal_receipt_cannot_replace_original_terminal() {
        let mut original = original();
        original.completion = Disposition::Complete {
            receipt: "04".repeat(32),
        };
        original.terminal = Some(IntegrationOutcome::Complete {
            receipt: "04".repeat(32),
        });
        let result = query_original_action(&original, &CancellationToken::default(), |_| {
            Ok(v2::Reply::Operation {
                lease: v2::LeaseId::parse(&original.lease).unwrap(),
                operation: v2::OperationId::parse(&original.operation).unwrap(),
                state: v2::OperationView::Finished {
                    outcome: v2::Outcome::ServiceActionCompleted {
                        outcome: v2::ServiceOutcome::Complete {
                            receipt: "05".repeat(32),
                        },
                    },
                },
            })
        });
        assert!(result.is_err(), "changed cached outcome must refuse");
    }
    #[test]
    fn provider_reset_does_not_erase_prior_local_or_resource_uncertainty() {
        let report = crate::native::integration::view::tests::report();
        let recipe = crate::native::integration::ValidatedIntegrationRecipe::decode(
            include_bytes!("../../examples/native/service-recipe.example.json"),
            "recipe.json".into(),
        )
        .unwrap();
        let journal = NativeIntegrationJournal::new(
            "run".into(),
            &recipe,
            vec![crate::model::ScenarioKind::Clean],
        )
        .unwrap();
        let complete = || Disposition::Complete {
            receipt: "settled".into(),
        };
        let mut result = IntegrationRecoveryResult {
            report,
            journal,
            report_path: "native-report.json".into(),
            prior_local: vec![Disposition::Unknown {
                reason: "lost prior transport owner".into(),
            }],
            local: complete(),
            remote: complete(),
            resources: vec![RecoveredResourceDisposition {
                ownership_id: "owner".into(),
                slot: ResourceSlot::new("service").unwrap(),
                original_action_ids: vec!["02".repeat(16)],
                local: complete(),
                remote: Disposition::Unknown {
                    reason: "manager restarted".into(),
                },
                broker: complete(),
            }],
        };
        assert!(!result.complete());
        result.prior_local = vec![complete()];
        assert!(
            !result.complete(),
            "provider disposal cannot erase manager uncertainty"
        );
    }

    #[test]
    fn changed_original_identity_refuses_before_transport_submission() {
        let mut original = original();
        original.operation = "09".repeat(16);
        let mut calls = 0;
        let result = query_original_action(&original, &CancellationToken::default(), |_| {
            calls += 1;
            anyhow::bail!("transport must not be called")
        });
        assert!(result.is_err());
        assert_eq!(
            calls, 0,
            "record operation must match canonical request before runtime"
        );
    }

    #[cfg(unix)]
    fn disposed_run() -> (tempfile::TempDir, ReportStore, LaunchAuthority) {
        use crate::native::model::*;
        let report = crate::native::integration::view::tests::report();
        let mut input: serde_json::Value =
            serde_json::from_str(include_str!("../../examples/native/launch.example.json"))
                .unwrap();
        input["schema"] = 2.into();
        input["helper_profile"] = "native_integration_v1".into();
        input["core_manifest_digest"] = report.core_manifest_digest.clone().into();
        input["integration_manifest_digest"] = report.integration_manifest_digest.clone().into();
        input["manifest_digest"] = report.manifest_digest.clone().into();
        input["service_authority"] = serde_json::json!({
            "kind":"systemd","slot":"fixture_service","scope":"user","bus_endpoint":{"kind":"unix_path","path":"/nonexistent/recovery-private/bus"},
            "qualification":{"manager":"systemd","version":"262","api_profile":"systemd_dbus_private_v1","features":["unit_properties","start_stop","manager_reload"],"receipt_sha256":"03".repeat(32)},
            "namespace":{"kind":"exclusive_owned_registration","slot":"fixture_service","unit_load_roots":["/nonexistent/recovery-private/units"],"dropins":"forbidden","provider_context":"private-fixture"},
            "manager_context":{"kind":"whole_private","slot":"manager","sut_allowed_actions":["daemon_reload"],"rehearsal_allowed_actions":[],"provider_receipt_required":true},
            "expected_peer_uid":1000,"expected_manager_uid":1000,"definition_root":"/nonexistent/recovery-private/units","unit":"theseus-fixture.service","managed_by":"installer",
            "sut_effects":["registration","configuration","removal","daemon_reload"],"allowed_actions":["inspect","start","stop"]
        });
        input["desktop_authority"] = serde_json::Value::Null;
        let launch = LaunchAuthority::decode(&serde_json::to_vec(&input).unwrap()).unwrap();
        let root = tempfile::tempdir().unwrap();
        let store = crate::report::create(&root.path().join("run")).unwrap();
        let mut intents = store.intent_store().unwrap();
        let prepared = intents
            .prepare(theseus_native_helper_store::IntentRecord {
                intent: v2::IntentId::from_bytes([11; 16]),
                binding: launch.binding().binding,
                manifest_digest: launch.binding().manifest_digest,
                workspace_marker: launch.binding().workspace_marker,
            })
            .unwrap();
        let owner = NativeResource {
            scenario_id: "clean".into(),
            ownership_id: "owner".into(),
            catalog_authority: NativeCatalogAuthority::Valid,
            ownership: NativeOwnership::Acquired {
                lease: "01".repeat(16),
                prepared: NativeIntent {
                    intent: prepared.record().intent.to_hex(),
                    binding: launch.binding().binding.to_hex(),
                    manifest_digest: report.manifest_digest.clone(),
                    workspace_marker: launch.binding().workspace_marker.to_hex(),
                    reset_epoch: launch.provider_epoch().to_hex(),
                    publication_scope: "file_and_directory_synced".into(),
                },
            },
        };
        let disposal = ProviderInvocation {
            ownership_id: "owner".into(),
            provider: launch.provider_name().into(),
            invocation_id: "original-dispose".into(),
            action: ProviderAction::Dispose,
            target_id: launch.view().target_id.clone(),
            baseline_label: launch.baseline_label().into(),
            launch_fingerprint: launch.view().fingerprint.clone(),
            prior_resource: owner.clone(),
        };
        let receipt = ProviderReceipt {
            provider: disposal.provider.clone(),
            invocation_id: disposal.invocation_id.clone(),
            invocation_digest: disposal.digest().unwrap(),
            binding: launch.binding().binding.to_hex(),
            manifest_digest: report.manifest_digest.clone(),
            workspace_marker: launch.binding().workspace_marker.to_hex(),
            reset_epoch: "05".repeat(32),
            receipt: "already-disposed-original-target".into(),
        };
        let binding = ResourceBindingSnapshot {
            digest: "03".repeat(32),
            basis: BindingBasis::VacantRegistration,
            evidence_ids: vec!["original-baseline-absence".into()],
            definition: None,
            artifact_commitment: "06".repeat(32),
            namespace_or_session_receipt_digest: "07".repeat(32),
            original_action: None,
        };
        let mut action = original();
        action.slot = ResourceSlot::new("fixture_service").unwrap();
        if let IntegrationRequest::Service { slot, .. } = &mut action.request {
            *slot = action.slot.clone();
        }
        action.request_digest = v2::request_digest(&action.request.wire().unwrap())
            .unwrap()
            .to_hex();
        let journal = NativeIntegrationJournal {
            kind: "theseus_native_journal".into(),
            schema: 2,
            profile: "native_integration_v1".into(),
            run_id: report.run_id.clone(),
            launch_fingerprint: Some(launch.view().fingerprint.clone()),
            recipe_digest: report.recipe_digest.clone(),
            core_recipe_digest: report.core_recipe_digest.clone(),
            core_manifest_digest: report.core_manifest_digest.clone(),
            integration_manifest_digest: report.integration_manifest_digest.clone(),
            manifest_digest: report.manifest_digest.clone(),
            requirements: report.requirements.clone(),
            compiled_requirements_digest: report.compiled_requirements_digest.clone(),
            selected_scenarios: report.selected_scenarios.clone(),
            ownership: vec![owner],
            operations: vec![],
            provider_calls: vec![NativeProviderCall {
                invocation: disposal,
                receipt: Some(receipt),
                completion: settled("original dispose receipt"),
            }],
            releases: vec![],
            recovery_attempts: vec![],
            failures: vec![],
            integration_ownership: vec![IntegrationResourceRecord {
                key: IntegrationResourceKey {
                    scenario_id: "clean".into(),
                    ownership_id: "owner".into(),
                    slot: action.slot.clone(),
                },
                kind: IntegrationResourceKind::Service,
                manifest_digest: report.manifest_digest.clone(),
                initial_binding: binding.clone(),
                binding_history: vec![binding.clone()],
                current_binding_digest: binding.digest,
                revocation: Some(AuthorityRevocation::ManagerRestarted),
                disposition: unknown("original native handle lost"),
            }],
            integration_actions: vec![action],
            broker_bindings: vec![],
            readiness_probes: vec![],
            integration_recoveries: vec![],
        };
        validate_pair(&report, &journal).unwrap();
        store.save_native_integration_journal(&journal).unwrap();
        store.save_native_integration_report(&report).unwrap();
        (root, store, launch)
    }
    #[cfg(unix)]
    #[test]
    fn schema2_disposal_recovers_typed_original_resource_without_transport_or_new_effects() {
        let (_root, store, launch) = disposed_run();
        let original = crate::native::storage::load_native_versioned(&store).unwrap();
        let NativeJournalVersion::IntegrationV2(before) = original.1 else {
            panic!("schema2 fixture")
        };
        let report_path = store.native_report_path();
        let result =
            recover_selected_native(store, &launch, CancellationToken::default(), |_| {}).unwrap();
        let SelectedNativeRecoveryResult::IntegrationV2(result) = result else {
            panic!("typed integration result")
        };
        assert!(!result.complete());
        assert!(complete(&result.local));
        assert!(complete(&result.remote));
        assert!(result.prior_local.iter().any(|p| !complete(p)));
        assert_eq!(result.resources.len(), 1);
        assert_eq!(result.resources[0].original_action_ids, ["02".repeat(16)]);
        assert!(!complete(&result.resources[0].remote));
        assert_eq!(
            result.journal.integration_actions,
            before.integration_actions
        );
        assert_eq!(result.journal.provider_calls, before.provider_calls);
        assert_eq!(result.journal.ownership, before.ownership);
        assert!(result.journal.operations.is_empty());
        assert!(result.journal.releases.is_empty());
        assert_eq!(result.journal.integration_recoveries.len(), 1);
        assert!(result.journal.integration_recoveries[0].queries.is_empty());
        assert!(
            crate::native::model::NativeReport::decode(&std::fs::read(&report_path).unwrap())
                .is_err()
        );
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(report_path).unwrap()).unwrap();
        assert_eq!(saved["schema"], 2);
    }
    #[cfg(unix)]
    #[test]
    fn missing_original_intent_refuses_before_events_and_keeps_schema2_files_unchanged() {
        let (_root, store, launch) = disposed_run();
        let before_report = std::fs::read(store.native_report_path()).unwrap();
        let before_journal =
            std::fs::read(store.root().join(crate::native::model::JOURNAL_FILENAME)).unwrap();
        let NativeJournalVersion::IntegrationV2(journal) =
            crate::native::storage::load_native_versioned(&store)
                .unwrap()
                .1
        else {
            panic!("schema2")
        };
        let crate::native::model::NativeOwnership::Acquired { prepared, .. } =
            &journal.ownership[0].ownership
        else {
            panic!("acquired")
        };
        std::fs::remove_file(
            store
                .root()
                .join("native-intents")
                .join(format!("{}.json", prepared.intent)),
        )
        .unwrap();
        let path = store.native_report_path();
        let journal_path = store.root().join(crate::native::model::JOURNAL_FILENAME);
        let mut events = 0;
        assert!(
            recover_selected_native(store, &launch, CancellationToken::default(), |_| events +=
                1)
            .is_err()
        );
        assert_eq!(events, 0);
        assert_eq!(std::fs::read(path).unwrap(), before_report);
        assert_eq!(std::fs::read(journal_path).unwrap(), before_journal);
    }

    #[cfg(unix)]
    #[test]
    fn saved_schema2_recovery_history_rejects_fabricated_original_query_identity() {
        let (_root, store, launch) = disposed_run();
        let SelectedNativeRecoveryResult::IntegrationV2(result) =
            recover_selected_native(store, &launch, CancellationToken::default(), |_| {}).unwrap()
        else {
            panic!("schema2")
        };
        let mut journal = result.journal;
        assert!(
            journal.encode().is_ok(),
            "legitimate original recovery history must round-trip"
        );
        journal.integration_recoveries[0]
            .queries
            .push(OriginalQueryOutcome {
                original: ActionReference {
                    operation: "09".repeat(16),
                    request_digest: "10".repeat(32),
                },
                view: OriginalQueryView::CachedTerminal,
                outcome: IntegrationOutcome::Complete {
                    receipt: "11".repeat(32),
                },
            });
        assert!(
            journal.encode().is_err(),
            "strict stored readers must refuse invented recovery query identities"
        );
    }

    #[cfg(unix)]
    #[test]
    fn saved_schema2_rejects_action_identity_different_from_canonical_request() {
        let (_root, store, _) = disposed_run();
        let NativeJournalVersion::IntegrationV2(mut journal) =
            crate::native::storage::load_native_versioned(&store)
                .unwrap()
                .1
        else {
            panic!("schema2")
        };
        journal.integration_actions[0].operation = "09".repeat(16);
        assert!(
            journal.encode().is_err(),
            "saved action metadata cannot target a different original operation"
        );
    }
    #[cfg(unix)]
    #[test]
    fn repeated_schema2_recovery_preserves_every_prior_uncertainty_and_original_identity() {
        let (_root, store, launch) = disposed_run();
        let root = store.root().to_path_buf();
        let SelectedNativeRecoveryResult::IntegrationV2(first) =
            recover_selected_native(store, &launch, CancellationToken::default(), |_| {}).unwrap()
        else {
            panic!("schema2")
        };
        let SelectedNativeRecoveryResult::IntegrationV2(second) = recover_selected_native(
            crate::report::open_existing(&root).unwrap(),
            &launch,
            CancellationToken::default(),
            |_| {},
        )
        .unwrap() else {
            panic!("schema2")
        };
        assert!(!second.complete());
        assert_eq!(
            second.journal.integration_actions,
            first.journal.integration_actions
        );
        assert_eq!(second.journal.provider_calls, first.journal.provider_calls);
        assert_eq!(second.journal.integration_recoveries.len(), 2);
        assert_eq!(
            second.journal.integration_recoveries[0],
            first.journal.integration_recoveries[0]
        );
        let latest = &second.journal.integration_recoveries[1];
        assert!(
            first.journal.integration_recoveries[0]
                .prior_local
                .iter()
                .all(|p| latest.prior_local.contains(p))
        );
        assert!(
            latest
                .prior_local
                .contains(&first.journal.integration_recoveries[0].local)
        );
        assert!(
            second
                .prior_local
                .iter()
                .any(|p| matches!(p, Disposition::Unknown { .. }))
        );
        let mut forged = second.journal;
        forged.integration_recoveries[1].prior_local = vec![settled("new recovery joined")];
        assert!(
            forged.encode().is_err(),
            "new completion cannot erase prior local uncertainty"
        );
    }
    #[test]
    fn original_unknown_sut_run_prevents_complete_even_after_provider_settlement() {
        use crate::native::model::*;
        let mut report = crate::native::integration::view::tests::report();
        let done = || settled("proved independent settlement");
        for i in &mut report.integrations {
            i.dispositions = IntegrationDispositions {
                helper_worker: done(),
                registration: done(),
                manager_action: done(),
                broker: done(),
                primary: done(),
                descendant_scope: done(),
            };
        }
        for s in &mut report.catalog.scenarios {
            s.dispositions = NativeDispositions {
                local: done(),
                remote: done(),
                release: done(),
                reset: done(),
            };
        }
        let recipe = crate::native::integration::ValidatedIntegrationRecipe::decode(
            include_bytes!("../../examples/native/service-recipe.example.json"),
            "recipe.json".into(),
        )
        .unwrap();
        let mut journal = NativeIntegrationJournal::new(
            "run".into(),
            &recipe,
            vec![crate::model::ScenarioKind::Clean],
        )
        .unwrap();
        let mut result = IntegrationRecoveryResult {
            report,
            journal: journal.clone(),
            report_path: "native-report.json".into(),
            prior_local: vec![done()],
            local: done(),
            remote: done(),
            resources: vec![],
        };
        assert!(
            result.complete(),
            "all mandatory settlement fields are proved in control case"
        );
        let operation = NativeOperation {
            ownership_id: "owner".into(),
            lease: "01".repeat(16),
            operation_id: "02".repeat(16),
            request_digest: "03".repeat(32),
            command: CatalogCommand {
                id: "install_target".into(),
                program: CatalogProgram::InstallTarget,
                expected_status: NativeStatus::PosixExit { code: 0 },
                stdout_literal: None,
            },
            completion: unknown("original SUT Run was accepted before helper restart"),
        };
        journal.operations.push(operation);
        result.journal = journal;
        assert!(
            !result.complete(),
            "external reset cannot erase original SUT Run uncertainty"
        );
    }
    #[cfg(unix)]
    fn sut_probe_fixture() -> (NativeIntegrationReport, NativeIntegrationJournal) {
        use crate::{
            model::{StageOutcome, StepName},
            native::model::*,
        };
        let (_root, store, _) = disposed_run();
        let (
            NativeReportVersion::IntegrationV2(mut report),
            NativeJournalVersion::IntegrationV2(mut journal),
        ) = crate::native::storage::load_native_versioned(&store).unwrap()
        else {
            panic!("schema2")
        };
        report
            .requirements
            .service
            .as_mut()
            .unwrap()
            .controls
            .configured_activation = ActivationCause::Sut;
        report.compiled_requirements_digest = report.requirements.digest().unwrap();
        journal.requirements = report.requirements.clone();
        journal.compiled_requirements_digest = report.compiled_requirements_digest.clone();
        let mut operation = NativeOperation {
            ownership_id: "owner".into(),
            lease: "01".repeat(16),
            operation_id: "12".repeat(16),
            request_digest: String::new(),
            command: CatalogCommand {
                id: "configure".into(),
                program: CatalogProgram::Configure,
                expected_status: NativeStatus::PosixExit { code: 0 },
                stdout_literal: None,
            },
            completion: settled("original Configure Run completed"),
        };
        operation.request_digest = v2::request_digest(&operation.request().unwrap())
            .unwrap()
            .to_hex();
        report.catalog.scenarios[0].stages.push(NativeStage {
            name: "configure".into(),
            step: Some(StepName::Configure),
            checkpoint: None,
            outcome: StageOutcome::Passed,
            commands: vec![NativeCommandEvidence {
                command: operation.command.clone(),
                operation_id: operation.operation_id.clone(),
                request_digest: operation.request_digest.clone(),
                status: Some(NativeStatus::PosixExit { code: 0 }),
                stdout: String::new(),
                stdout_complete: true,
                stdout_utf8: true,
                stderr_complete: true,
                stdout_log: None,
                stderr_log: None,
                failure: None,
            }],
            checks: vec![],
            native_files: vec![],
            failures: vec![],
        });
        let resource = &mut journal.integration_ownership[0];
        resource.revocation = None;
        let mut binding = resource.initial_binding.clone();
        binding.digest = "13".repeat(32);
        binding.basis = BindingBasis::ExclusiveOwnedRegistration;
        binding.original_action = Some(operation.operation_id.clone());
        binding.definition = Some(DefinitionKey {
            role: DefinitionRole::Configured,
            artifact: ArtifactRole::Target,
        });
        resource.current_binding_digest = binding.digest.clone();
        resource.binding_history.push(binding.clone());
        let expectation = ReadinessExpectation {
            role: DefinitionRole::Configured,
            artifact: ArtifactRole::Target,
            artifact_length: 1,
            configuration_digest: "14".repeat(32),
            artifact_digest: "15".repeat(32),
            version: "fixture-v1".into(),
            data_digest: "16".repeat(32),
            initialized: true,
        };
        let expectation_digest = expectation.digest().unwrap();
        journal.readiness_probes.push(ReadinessProbeRecord {
            challenge: ReadinessChallenge {
                resource_binding_digest: binding.digest,
                action_digest: operation.request_digest.clone(),
                expectation_digest: expectation_digest.clone(),
                ownership_id: operation.ownership_id.clone(),
                lease: operation.lease.clone(),
                slot: resource.key.slot.clone(),
                operation: operation.operation_id.clone(),
                nonce: "17".repeat(16),
                deadline_ms: 1000,
            },
            expectation,
            expectation_digest,
            observation_ids: vec![],
            completion: unknown("published exact SUT activation readiness probe"),
        });
        journal.operations.push(operation);
        (report, journal)
    }
    #[cfg(unix)]
    #[test]
    fn sut_readiness_requires_exact_successful_original_activation_run() {
        use crate::native::model::*;
        let (report, journal) = sut_probe_fixture();
        validate_pair(&report, &journal)
            .expect("exact successful SUT Configure is an admitted readiness activation cause");
        let mut journal_ahead = journal.clone();
        journal_ahead.readiness_probes[0].observation_ids =
            vec!["reply-observed-before-report-barrier".into()];
        journal_ahead.readiness_probes[0].completion = settled("private-response-receipt");
        validate_pair(&report, &journal_ahead).expect(
            "crash between private completion and public report preserves incomplete evidence",
        );
        let mut wrong = journal.clone();
        wrong.operations[0].command.program = CatalogProgram::CheckData;
        wrong.operations[0].request_digest =
            v2::request_digest(&wrong.operations[0].request().unwrap())
                .unwrap()
                .to_hex();
        wrong.readiness_probes[0].challenge.action_digest =
            wrong.operations[0].request_digest.clone();
        assert!(
            wrong.encode().is_err(),
            "an arbitrary successful Run cannot become activation authority"
        );
        let mut failed = report.clone();
        failed.catalog.scenarios[0].stages[0].outcome = crate::model::StageOutcome::Failed;
        failed.catalog.scenarios[0].stages[0].commands[0].status =
            Some(NativeStatus::PosixExit { code: 7 });
        assert!(
            validate_pair(&failed, &journal).is_err(),
            "terminal completion alone does not prove successful SUT activation"
        );
        let mut wrong = journal.clone();
        wrong.readiness_probes[0].expectation.role = DefinitionRole::Target;
        wrong.readiness_probes[0].expectation_digest =
            wrong.readiness_probes[0].expectation.digest().unwrap();
        wrong.readiness_probes[0].challenge.expectation_digest =
            wrong.readiness_probes[0].expectation_digest.clone();
        assert!(
            wrong.encode().is_err(),
            "activation binding definition must match the exact readiness expectation"
        );
    }
    #[cfg(unix)]
    #[test]
    fn recovered_resource_preserves_known_failure_until_actual_uncertainty_dominates() {
        let (_root, store, _) = disposed_run();
        let (
            NativeReportVersion::IntegrationV2(mut report),
            NativeJournalVersion::IntegrationV2(mut journal),
        ) = crate::native::storage::load_native_versioned(&store).unwrap()
        else {
            panic!("schema2")
        };
        let done = || settled("proved native disposition");
        report.integrations[0].dispositions = IntegrationDispositions {
            helper_worker: done(),
            registration: done(),
            manager_action: done(),
            broker: done(),
            primary: done(),
            descendant_scope: done(),
        };
        journal.integration_ownership[0].revocation = None;
        journal.integration_ownership[0].disposition = done();
        journal.integration_actions[0].completion = Disposition::Failed {
            reason: "original manager Start rejected".into(),
        };
        journal.integration_actions[0].terminal = Some(IntegrationOutcome::Failed {
            reason: "original manager Start rejected".into(),
        });
        let row = resource_dispositions(&report, &journal).remove(0);
        assert_eq!(
            row.remote,
            Disposition::Failed {
                reason: "original manager Start rejected".into()
            }
        );
        let failed_action = journal.integration_actions[0].clone();
        report.integrations[0].dispositions.helper_worker =
            unknown("original worker settlement lost");
        let row = resource_dispositions(&report, &journal).remove(0);
        assert!(matches!(row.remote, Disposition::Unknown { .. }));
        assert_eq!(
            journal.integration_actions[0], failed_action,
            "aggregate uncertainty cannot rewrite exact known failure"
        );
        report.integrations[0].dispositions.helper_worker = done();
        journal.integration_actions[0].completion = done();
        journal.integration_actions[0].terminal = Some(IntegrationOutcome::Complete {
            receipt: "proved native disposition".into(),
        });
        journal.integration_ownership[0].revocation = Some(AuthorityRevocation::ManagerRestarted);
        assert!(matches!(
            resource_dispositions(&report, &journal)[0].remote,
            Disposition::Unknown { .. }
        ));
        journal.integration_ownership[0].revocation = None;
        assert!(complete(
            &resource_dispositions(&report, &journal)[0].remote
        ));
        journal.integration_ownership[0].disposition = Disposition::Failed {
            reason: "SUT left original registration present".into(),
        };
        assert_eq!(
            resource_dispositions(&report, &journal)[0].remote,
            Disposition::Failed {
                reason: "SUT left original registration present".into()
            }
        );
    }
    #[cfg(unix)]
    #[test]
    fn persisted_nonterminal_query_history_allows_later_original_cached_terminal() {
        use crate::native::model::NativeRecoveryAttempt;
        for terminal in [
            IntegrationOutcome::Complete {
                receipt: "21".repeat(32),
            },
            IntegrationOutcome::Failed {
                reason: "original manager rejected request".into(),
            },
            IntegrationOutcome::Unknown {
                reason: "cached original native uncertainty".into(),
            },
        ] {
            let (_root, store, _) = disposed_run();
            let (
                NativeReportVersion::IntegrationV2(report),
                NativeJournalVersion::IntegrationV2(journal),
            ) = crate::native::storage::load_native_versioned(&store).unwrap()
            else {
                panic!("schema2")
            };
            let original = journal.integration_actions[0].clone();
            let mut projector = RunProjector::new(
                &store,
                StoredRun::NativeIntegration(report),
                StoredJournal::NativeIntegration(journal),
            )
            .unwrap();
            let prior = unknown("original controller owner lost");
            let first = "18".repeat(16);
            projector
                .record_recovery_attempt(NativeRecoveryAttempt {
                    recovery_id: first.clone(),
                    ownership_id: "owner".into(),
                    prior_local: prior.clone(),
                    local: unknown("recovery interrupted before local join"),
                    remote: unknown("provider not settled"),
                })
                .unwrap();
            publish_attempt(&mut projector, &first, "owner").unwrap();
            let cancel = CancellationToken::default();
            cancel.cancel();
            let interrupted = query_original_action(&original, &cancel, |_| {
                panic!("cancelled recovery must not submit a new action")
            })
            .unwrap();
            assert!(!interrupted.cached_terminal);
            projector
                .record_integration_query(
                    &first,
                    OriginalQueryOutcome {
                        original: action_reference(&original),
                        view: OriginalQueryView::NonterminalUnknown,
                        outcome: interrupted.outcome,
                    },
                )
                .unwrap();
            drop(projector);
            let (
                NativeReportVersion::IntegrationV2(report),
                NativeJournalVersion::IntegrationV2(journal),
            ) = crate::native::storage::load_native_versioned(&store).unwrap()
            else {
                panic!("schema2")
            };
            let retained_first = journal.integration_recoveries[0].clone();
            assert!(journal.integration_actions[0].terminal.is_none());
            let mut projector = RunProjector::new(
                &store,
                StoredRun::NativeIntegration(report),
                StoredJournal::NativeIntegration(journal),
            )
            .unwrap();
            let second = "19".repeat(16);
            projector
                .record_recovery_attempt(NativeRecoveryAttempt {
                    recovery_id: second.clone(),
                    ownership_id: "owner".into(),
                    prior_local: prior.clone(),
                    local: unknown("new local settlement pending"),
                    remote: unknown("provider not settled"),
                })
                .unwrap();
            publish_attempt(&mut projector, &second, "owner").unwrap();
            let queried =
                query_original_action(&original, &CancellationToken::default(), |action| {
                    assert!(
                        matches!(action, Action::Query { .. }),
                        "only the original cached action may be queried"
                    );
                    let outcome = match &terminal {
                        IntegrationOutcome::Complete { receipt } => v2::ServiceOutcome::Complete {
                            receipt: receipt.clone(),
                        },
                        IntegrationOutcome::Failed { reason } => v2::ServiceOutcome::Failed {
                            reason: reason.clone(),
                        },
                        IntegrationOutcome::Unknown { reason } => v2::ServiceOutcome::Unknown {
                            reason: reason.clone(),
                        },
                    };
                    Ok(v2::Reply::Operation {
                        lease: v2::LeaseId::parse(&original.lease).unwrap(),
                        operation: v2::OperationId::parse(&original.operation).unwrap(),
                        state: v2::OperationView::Finished {
                            outcome: v2::Outcome::ServiceActionCompleted { outcome },
                        },
                    })
                })
                .unwrap();
            assert!(queried.cached_terminal);
            projector
                .record_integration_query(
                    &second,
                    OriginalQueryOutcome {
                        original: action_reference(&original),
                        view: OriginalQueryView::CachedTerminal,
                        outcome: queried.outcome.clone(),
                    },
                )
                .unwrap();
            if matches!(terminal, IntegrationOutcome::Unknown { .. }) {
                assert!(
                    projector
                        .record_integration_completion(
                            &original.lease,
                            &original.operation,
                            settled("contradictory cached receipt"),
                            IntegrationOutcome::Complete {
                                receipt: "22".repeat(32)
                            }
                        )
                        .is_err()
                );
                let StoredJournal::NativeIntegration(saved) = projector.journal() else {
                    panic!("schema2")
                };
                saved
                    .encode()
                    .expect("contradictory terminal rejection must preserve a valid owned journal");
            }
            let completion = match &queried.outcome {
                IntegrationOutcome::Complete { receipt } => Disposition::Complete {
                    receipt: receipt.clone(),
                },
                IntegrationOutcome::Failed { reason } => Disposition::Failed {
                    reason: reason.clone(),
                },
                IntegrationOutcome::Unknown { reason } => Disposition::Unknown {
                    reason: reason.clone(),
                },
            };
            projector.record_integration_completion(&original.lease, &original.operation, completion, queried.outcome)
                .expect("earlier nonterminal uncertainty cannot block a later exact original cached terminal");
            let StoredJournal::NativeIntegration(saved) = projector.journal() else {
                panic!("schema2")
            };
            let decoded = NativeIntegrationJournal::decode(&saved.encode().unwrap()).unwrap();
            assert_eq!(decoded.integration_recoveries[0], retained_first);
            assert_eq!(
                decoded.integration_actions[0].terminal.as_ref(),
                Some(&terminal)
            );
            assert!(
                decoded.integration_recoveries[1]
                    .prior_local
                    .contains(&prior)
            );
            assert!(
                decoded.integration_recoveries[1]
                    .prior_local
                    .iter()
                    .any(|p| !complete(p))
            );
            if matches!(terminal, IntegrationOutcome::Unknown { .. }) {
                assert!(
                    projector
                        .record_integration_completion(
                            &original.lease,
                            &original.operation,
                            settled("different cached receipt"),
                            IntegrationOutcome::Complete {
                                receipt: "22".repeat(32)
                            }
                        )
                        .is_err(),
                    "a genuinely cached terminal Unknown remains immutable"
                );
            }
        }
    }
    #[cfg(unix)]
    #[test]
    fn failed_outcome_stays_failed_after_clean_recovery_and_unknown_remains_independent() {
        use crate::{
            model::{CheckOutcome, RunOutcome},
            native::model::*,
        };
        for resource_failure in [false, true] {
            for actual_unknown in [false, true] {
                let (_root, store, launch) = disposed_run();
                let (
                    NativeReportVersion::IntegrationV2(mut report),
                    NativeJournalVersion::IntegrationV2(mut journal),
                ) = crate::native::storage::load_native_versioned(&store).unwrap()
                else {
                    panic!("schema2")
                };
                let done = || settled("original independent owner proved settled");
                report.catalog.scenarios[0].outcome = RunOutcome::Failed;
                report.catalog.scenarios[0].dispositions = NativeDispositions {
                    local: done(),
                    remote: done(),
                    release: done(),
                    reset: done(),
                };
                if actual_unknown && resource_failure {
                    report.catalog.scenarios[0].dispositions.local =
                        unknown("original controller local owner remains unproved");
                }
                report.catalog.outcome = RunOutcome::Failed;
                report.outcome = RunOutcome::Failed;
                report.integrations[0].checks[0].outcome = CheckOutcome::Failed;
                report.integrations[0].checks[0].detail =
                    "decisive original SUT readiness mismatch".into();
                let original_failure = report.integrations[0].checks[0].clone();
                report.integrations[0].dispositions = IntegrationDispositions {
                    helper_worker: done(),
                    registration: done(),
                    manager_action: done(),
                    broker: done(),
                    primary: done(),
                    descendant_scope: done(),
                };
                journal.integration_ownership[0].revocation = None;
                journal.integration_ownership[0].disposition = if actual_unknown {
                    unknown("original manager owner lost")
                } else if resource_failure {
                    Disposition::Failed {
                        reason: "SUT left original registration present".into(),
                    }
                } else {
                    done()
                };
                journal.integration_actions[0].completion = done();
                journal.integration_actions[0].terminal = Some(IntegrationOutcome::Complete {
                    receipt: "original independent owner proved settled".into(),
                });
                validate_pair(&report, &journal).unwrap();
                store.save_native_integration_journal(&journal).unwrap();
                store.save_native_integration_report(&report).unwrap();
                let path = store.native_report_path();
                let SelectedNativeRecoveryResult::IntegrationV2(result) =
                    recover_selected_native(store, &launch, CancellationToken::default(), |_| {})
                        .unwrap()
                else {
                    panic!("schema2")
                };
                assert_eq!(result.report.catalog.outcome, RunOutcome::Failed);
                assert_eq!(result.report.integrations[0].checks[0], original_failure);
                assert_eq!(
                    result.report.outcome,
                    if actual_unknown {
                        RunOutcome::Incomplete
                    } else {
                        RunOutcome::Failed
                    },
                    "known Failed is separate from actual cleanup uncertainty"
                );
                assert_eq!(
                    result.complete(),
                    !actual_unknown && !resource_failure,
                    "cleanup completion is independent from decisive SUT failure"
                );
                let saved = NativeIntegrationReport::decode(&std::fs::read(path).unwrap()).unwrap();
                assert_eq!(saved.outcome, result.report.outcome);
                if resource_failure && !actual_unknown {
                    assert!(matches!(
                        result.resources[0].remote,
                        Disposition::Failed { .. }
                    ));
                }
            }
        }
    }
    #[cfg(unix)]
    #[test]
    fn failed_source_retains_unpublished_core_release_uncertainty_after_disposal() {
        use crate::{
            model::{CheckOutcome, RunOutcome},
            native::model::*,
        };
        for release in [
            unknown("original Release was never proved"),
            Disposition::NotStarted,
            settled("original release proved"),
            Disposition::Failed {
                reason: "original release definitely failed".into(),
            },
        ] {
            let uncertain = matches!(
                release,
                Disposition::Unknown { .. } | Disposition::NotStarted
            );
            let release_complete = complete(&release);
            let (_root, store, launch) = disposed_run();
            let (
                NativeReportVersion::IntegrationV2(mut report),
                NativeJournalVersion::IntegrationV2(mut journal),
            ) = crate::native::storage::load_native_versioned(&store).unwrap()
            else {
                panic!("schema2")
            };
            let done = || settled("original independent owner proved settled");
            report.catalog.scenarios[0].outcome = RunOutcome::Failed;
            report.catalog.scenarios[0].dispositions = NativeDispositions {
                local: done(),
                remote: done(),
                release: release.clone(),
                reset: done(),
            };
            report.catalog.outcome = RunOutcome::Failed;
            report.outcome = RunOutcome::Failed;
            report.integrations[0].checks[0].outcome = CheckOutcome::Failed;
            report.integrations[0].checks[0].detail =
                "decisive original SUT failure is retained".into();
            let original_failure = report.integrations[0].checks[0].clone();
            report.integrations[0].dispositions = IntegrationDispositions {
                helper_worker: done(),
                registration: done(),
                manager_action: done(),
                broker: done(),
                primary: done(),
                descendant_scope: done(),
            };
            journal.integration_ownership[0].revocation = None;
            journal.integration_ownership[0].disposition = done();
            journal.integration_actions[0].completion = done();
            journal.integration_actions[0].terminal = Some(IntegrationOutcome::Complete {
                receipt: "original independent owner proved settled".into(),
            });
            assert!(
                journal.releases.is_empty(),
                "fixture has no release publication to aggregate"
            );
            validate_pair(&report, &journal).unwrap();
            store.save_native_integration_journal(&journal).unwrap();
            store.save_native_integration_report(&report).unwrap();
            let path = store.native_report_path();
            let SelectedNativeRecoveryResult::IntegrationV2(result) =
                recover_selected_native(store, &launch, CancellationToken::default(), |_| {})
                    .unwrap()
            else {
                panic!("schema2")
            };
            assert_eq!(
                result.report.catalog.scenarios[0].outcome,
                RunOutcome::Failed
            );
            assert_eq!(result.report.catalog.outcome, RunOutcome::Failed);
            assert_eq!(result.report.integrations[0].checks[0], original_failure);
            assert_eq!(
                result.report.catalog.scenarios[0].dispositions.release,
                release
            );
            assert!(
                result.journal.releases.is_empty(),
                "matching disposal cannot invent or prove an original Release"
            );
            assert_eq!(
                result.report.outcome,
                if uncertain {
                    RunOutcome::Incomplete
                } else {
                    RunOutcome::Failed
                },
                "all retained core disposition fields count even without journal rows"
            );
            assert_eq!(result.complete(), release_complete);
            let persisted = NativeIntegrationReport::decode(&std::fs::read(path).unwrap()).unwrap();
            assert_eq!(persisted.outcome, result.report.outcome);
            assert_eq!(persisted.catalog.scenarios[0].dispositions.release, release);
        }
    }
}

#[cfg(test)]
mod desktop_progress_review_tests {
    use super::*;
    use v2::broker::{
        BrokerDescendants, BrokerEffect, BrokerEffectReceipt, BrokerOwner, BrokerPrimary,
        BrokerState, DesktopAction, DesktopEvidence as BrokerEvidence, DesktopRequest,
    };
    fn original() -> IntegrationActionRecord {
        let mut request:DesktopRequest=serde_json::from_value(serde_json::json!({"helper_lease":"01".repeat(16),"helper_operation":"02".repeat(16),"broker_lease":"03".repeat(16),"binding_digest":"04".repeat(32),"activation_nonce":"05".repeat(16),"slot":"desktop","prior_binding_digest":"06".repeat(32),"action":{"kind":"activate","mode":"owned_executable","entry_digest":"07".repeat(32),"expectation_digest":"08".repeat(32),"close_binding":{"operation":"09".repeat(16),"action_digest":"00".repeat(32),"nonce":"0a".repeat(16)}}})).unwrap();
        let close = request.close_request().unwrap().digest().unwrap().to_hex();
        if let DesktopAction::Activate { close_binding, .. } = &mut request.action {
            close_binding.action_digest = close;
        }
        let identity = request.identity().unwrap();
        let wire = v2::Request::DesktopAction {
            request: Box::new(request.clone()),
        };
        IntegrationActionRecord {
            ownership_id: "owner".into(),
            lease: request.helper_lease.clone(),
            slot: ResourceSlot::new("desktop").unwrap(),
            operation: request.helper_operation.clone(),
            request: IntegrationRequest::Desktop {
                lease: request.helper_lease.clone(),
                operation: request.helper_operation.clone(),
                slot: ResourceSlot::new("desktop").unwrap(),
                prior_binding_digest: request.prior_binding_digest.clone(),
                mode: DesktopMode::OwnedExecutable,
                request: Box::new(request),
            },
            request_digest: v2::request_digest(&wire).unwrap().to_hex(),
            prior_binding_digest: "06".repeat(32),
            broker_identity: Some(
                serde_json::from_value(serde_json::to_value(identity).unwrap()).unwrap(),
            ),
            completion: unknown("reply lost"),
            terminal: None,
        }
    }
    fn pending() -> BrokerState {
        BrokerState {
            owned_lifetime: None,
            native_lifetime: None,
            effect: BrokerEffect::Receipt {
                receipt: BrokerEffectReceipt {
                    evidence: BrokerEvidence::SourceOwnedExecutable,
                    acknowledged: true,
                    receipt_digest: "0b".repeat(32),
                },
            },
            owner: BrokerOwner::Retained,
            primary: BrokerPrimary::Running,
            descendants: BrokerDescendants::Pending,
            cleanup_complete: false,
            quarantined: false,
        }
    }
    fn settled() -> BrokerState {
        BrokerState {
            owner: BrokerOwner::Settled,
            primary: BrokerPrimary::Exited,
            descendants: BrokerDescendants::Joined,
            cleanup_complete: true,
            ..pending()
        }
    }
    fn saved_desktop(store: &ReportStore) -> (NativeIntegrationReport, NativeIntegrationJournal) {
        use crate::native::model::*;
        let mut report = crate::native::integration::view::tests::report();
        let owner = "0c".repeat(16);
        let slot = ResourceSlot::new("desktop").unwrap();
        report.requirements = IntegrationRequirements {
            service: None,
            desktop: Some(DesktopRequirement {
                resource: slot.clone(),
                modes: vec![DesktopMode::OwnedExecutable],
                readiness: ReadinessRequirement {
                    kind: ReadyKind::LocalChallenge,
                    protocol: ReadyProtocol::TheseusFixtureReadyV1,
                    deadline_ms: 1000,
                },
                lifetime: LifetimeRequirement::OwnedProcessTreeSettled,
            }),
        };
        report.compiled_requirements_digest = report.requirements.digest().unwrap();
        report.catalog.scenarios[0].ownership_id = owner.clone();
        report.catalog.scenarios[0].dispositions.local =
            unknown("historical controller owner lost");
        let evidence = &mut report.integrations[0];
        evidence.ownership_id = owner.clone();
        evidence.services.clear();
        evidence.desktops = vec![DesktopEvidence {
            slot: slot.clone(),
            observations: vec![],
            actions: vec![],
        }];
        evidence.checks[0].slot = slot.clone();
        evidence.checks[0].predicate = IntegrationPredicate::ApplicationReady;
        let header:v2::broker::IntegrationBrokerHeader=serde_json::from_value(serde_json::json!({"profile":"native_integration_broker_v1","binding":"11".repeat(16),"manifest_digest":report.manifest_digest,"workspace_marker":"12".repeat(16),"os":"linux","target_id":"original-target","ownership_id":owner,"provider_epoch":"13".repeat(32),"session_id":"supplied-session","session_epoch":"14".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"x11","display":":92","authority_file":"/fixture/auth","bus":"/fixture/bus.sock"},"allowed_modes":["owned_executable"]})).unwrap();
        let mut action = original();
        action.ownership_id = owner.clone();
        let IntegrationRequest::Desktop { request, .. } = &mut action.request else {
            unreachable!()
        };
        request.binding_digest = header.digest().unwrap().to_hex();
        let close_digest = request.close_request().unwrap().digest().unwrap().to_hex();
        if let DesktopAction::Activate { close_binding, .. } = &mut request.action {
            close_binding.action_digest = close_digest;
        }
        action.broker_identity = Some(
            serde_json::from_value(serde_json::to_value(request.identity().unwrap()).unwrap())
                .unwrap(),
        );
        action.request_digest = v2::request_digest(&action.request.wire().unwrap())
            .unwrap()
            .to_hex();
        let binding = ResourceBindingSnapshot {
            digest: action.prior_binding_digest.clone(),
            basis: BindingBasis::VacantRegistration,
            evidence_ids: vec!["original-entry-absence".into()],
            definition: None,
            artifact_commitment: "15".repeat(32),
            namespace_or_session_receipt_digest: "16".repeat(32),
            original_action: None,
        };
        let native = NativeResource {
            scenario_id: "clean".into(),
            ownership_id: owner.clone(),
            catalog_authority: NativeCatalogAuthority::Valid,
            ownership: NativeOwnership::Acquired {
                lease: action.lease.clone(),
                prepared: NativeIntent {
                    intent: "17".repeat(16),
                    binding: "18".repeat(16),
                    manifest_digest: report.manifest_digest.clone(),
                    workspace_marker: "19".repeat(16),
                    reset_epoch: "1a".repeat(32),
                    publication_scope: "file_and_directory_synced".into(),
                },
            },
        };
        let original_bind = v2::broker::BrokerBind {
            ownership_id: owner.clone(),
            helper_lease: action.lease.clone(),
            helper_operation: "1b".repeat(16),
            slot: v2::ResourceSlot::new("desktop").unwrap(),
            broker_intent: "1c".repeat(16),
            expected_header: header.clone(),
        };
        let broker = BrokerBindingRecord {
            original: original_bind.clone(),
            ownership_id: owner.clone(),
            helper_lease: action.lease.clone(),
            slot: slot.clone(),
            original_intent: BrokerIntentRecord {
                intent: original_bind.broker_intent.clone(),
                binding: header.binding.clone(),
                manifest_digest: header.manifest_digest.clone(),
                workspace_marker: header.workspace_marker.clone(),
            },
            publication_scope: Some(v2::broker::IntentPublicationScope::FileAndDirectorySynced),
            original_intent_reference: format!("{}.json", original_bind.broker_intent),
            expected_header_digest: header.digest().unwrap().to_hex(),
            session_id: header.session_id.clone(),
            session_epoch: header.session_epoch.clone(),
            broker_lease: Some("03".repeat(16)),
            disposition: settled_disposition("original durable mapping"),
        };
        let journal = NativeIntegrationJournal {
            kind: "theseus_native_journal".into(),
            schema: 2,
            profile: "native_integration_v1".into(),
            run_id: report.run_id.clone(),
            launch_fingerprint: Some("1d".repeat(32)),
            recipe_digest: report.recipe_digest.clone(),
            core_recipe_digest: report.core_recipe_digest.clone(),
            core_manifest_digest: report.core_manifest_digest.clone(),
            integration_manifest_digest: report.integration_manifest_digest.clone(),
            manifest_digest: report.manifest_digest.clone(),
            requirements: report.requirements.clone(),
            compiled_requirements_digest: report.compiled_requirements_digest.clone(),
            selected_scenarios: report.selected_scenarios.clone(),
            ownership: vec![native],
            operations: vec![],
            provider_calls: vec![],
            releases: vec![],
            recovery_attempts: vec![],
            failures: vec![],
            integration_ownership: vec![IntegrationResourceRecord {
                key: IntegrationResourceKey {
                    scenario_id: "clean".into(),
                    ownership_id: owner,
                    slot,
                },
                kind: IntegrationResourceKind::Desktop,
                manifest_digest: report.manifest_digest.clone(),
                initial_binding: binding.clone(),
                binding_history: vec![binding.clone()],
                current_binding_digest: binding.digest,
                revocation: None,
                disposition: unknown("original lifetime unproved"),
            }],
            integration_actions: vec![action],
            broker_bindings: vec![broker],
            readiness_probes: vec![],
            integration_recoveries: vec![],
        };
        validate_pair(&report, &journal).unwrap();
        store.save_native_integration_report(&report).unwrap();
        store.save_native_integration_journal(&journal).unwrap();
        (report, journal)
    }
    fn settled_disposition(receipt: &str) -> Disposition {
        Disposition::Complete {
            receipt: receipt.into(),
        }
    }
    fn publish_recovery(projector: &mut RunProjector<'_>, owner: &str) -> String {
        let id = "1e".repeat(16);
        projector
            .record_recovery_attempt(crate::native::model::NativeRecoveryAttempt {
                recovery_id: id.clone(),
                ownership_id: owner.into(),
                prior_local: unknown("historical controller owner lost"),
                local: unknown("current local owner pending"),
                remote: unknown("provider not queried"),
            })
            .unwrap();
        publish_attempt(projector, &id, owner).unwrap();
        id
    }
    fn stored_bytes(store: &ReportStore) -> (Vec<u8>, Vec<u8>) {
        (
            std::fs::read(store.root().join(crate::native::model::JOURNAL_FILENAME)).unwrap(),
            std::fs::read(store.native_report_path()).unwrap(),
        )
    }
    fn bound_original_state(
        report: &NativeIntegrationReport,
        original: &IntegrationActionRecord,
        state: BrokerState,
    ) -> BoundDesktopObservation {
        let evidence = match &state.effect {
            BrokerEffect::Receipt { receipt } => Some(receipt.evidence),
            _ => None,
        };
        let unavailable = || IntegrationProvenance::Unavailable {
            reason: "not sampled by this exact original lifetime observation".into(),
        };
        BoundDesktopObservation {
            generation: None,
            entry_valid: None,
            broker_state: Some(state),
            readiness: None,
            evidence,
            binding: ObservationBinding {
                id: "original-before-recovery".into(),
                run_id: report.run_id.clone(),
                scenario_id: "clean".into(),
                ownership_id: original.ownership_id.clone(),
                lease: original.lease.clone(),
                slot: original.slot.clone(),
                request_id: "original-query".into(),
                action: Some(original.operation.clone()),
                binding_digest: original.prior_binding_digest.clone(),
                source_profile: "native_integration_broker_v1".into(),
                source_version: "1".into(),
                phase: IntegrationPhase::BeforeUninstall,
                definition: None,
                sequence: 1,
                started_ms: 0,
                finished_ms: 1,
                complete: true,
            },
            entry_present: unavailable(),
            activated: unavailable(),
            primary_exited: unavailable(),
            descendant_scope_settled: unavailable(),
        }
    }
    #[test]
    fn first_recovered_effect_rejects_before_publication_and_cancel_then_correct_query_succeeds() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::report::create(&root.path().join("run")).unwrap();
        let (report, mut journal) = saved_desktop(&store);
        let receipt = "0b".repeat(32);
        journal.integration_actions[0].completion = settled_disposition(&receipt);
        journal.integration_actions[0].terminal = Some(IntegrationOutcome::Complete { receipt });
        let original = journal.integration_actions[0].clone();
        let owner = journal.ownership[0].clone();
        let mut projector = RunProjector::new(
            &store,
            StoredRun::NativeIntegration(report),
            StoredJournal::NativeIntegration(journal),
        )
        .unwrap();
        let recovery_id = publish_recovery(&mut projector, &owner.ownership_id);
        let before = stored_bytes(&store);
        let mut changed = pending();
        if let BrokerEffect::Receipt { receipt } = &mut changed.effect {
            receipt.receipt_digest = "ff".repeat(32);
        }
        let mut rejected_ack = pending();
        if let BrokerEffect::Receipt { receipt } = &mut rejected_ack.effect {
            receipt.acknowledged = false;
        }
        let unknown = BrokerState {
            effect: BrokerEffect::Unknown,
            ..pending()
        };
        for bad in [changed, rejected_ack, unknown] {
            let mut cancels = 0;
            let result = query_original_desktop_observing(
                &original,
                &CancellationToken::default(),
                |action| {
                    let identity = match action {
                        Action::QueryDesktop { identity } => identity,
                        Action::CancelDesktop { identity } => {
                            cancels += 1;
                            identity
                        }
                        _ => panic!("only exact original query permitted"),
                    };
                    let state = if cancels == 0 {
                        bad.clone()
                    } else {
                        BrokerState {
                            owner: BrokerOwner::Settled,
                            primary: BrokerPrimary::Exited,
                            descendants: BrokerDescendants::Joined,
                            cleanup_complete: true,
                            ..bad.clone()
                        }
                    };
                    Ok(v2::Reply::DesktopState {
                        identity: Box::new(identity),
                        state: Box::new(state),
                    })
                },
                |observation| {
                    projector.record_broker_recovery_observation(&recovery_id, observation)
                },
            );
            assert!(result.is_err());
            assert_eq!(
                cancels, 0,
                "contradictory first effect must not authorize subsequent original Cancel"
            );
            assert_eq!(
                stored_bytes(&store),
                before,
                "rejection must preserve journal/report bytes"
            );
            let StoredJournal::NativeIntegration(saved) = projector.journal() else {
                unreachable!()
            };
            let mut forged = saved.clone();
            let IntegrationRequest::Desktop { request, .. } = &original.request else {
                unreachable!()
            };
            let observation = RecoveredBrokerObservation {
                original: action_reference(&original),
                identity: request.identity().unwrap(),
                source: BrokerObservationSource::Query,
                state: bad,
            };
            assert!(
                projector
                    .record_broker_recovery_observation(&recovery_id, observation.clone())
                    .is_err(),
                "projector must independently reject the first contradictory effect"
            );
            assert_eq!(stored_bytes(&store), before);
            forged.integration_recoveries[0]
                .broker_observations
                .push(observation);
            assert!(
                NativeIntegrationJournal::decode(&serde_json::to_vec(&forged).unwrap()).is_err(),
                "saved contradictory first effect must refuse"
            );
        }
        let mut cancels = 0;
        let query = query_original_desktop_observing(
            &original,
            &CancellationToken::default(),
            |action| {
                let identity = match action {
                    Action::QueryDesktop { identity } => identity,
                    Action::CancelDesktop { identity } => {
                        cancels += 1;
                        identity
                    }
                    _ => panic!("no fresh effect"),
                };
                Ok(v2::Reply::DesktopState {
                    identity: Box::new(identity),
                    state: Box::new(if cancels == 0 { pending() } else { settled() }),
                })
            },
            |observation| projector.record_broker_recovery_observation(&recovery_id, observation),
        )
        .unwrap();
        assert_eq!(cancels, 1);
        assert_eq!(Some(query.outcome), original.terminal);
        drop(projector);
        let (_, NativeJournalVersion::IntegrationV2(saved)) =
            crate::native::storage::load_native_versioned(&store).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(saved.integration_actions[0], original);
        assert_eq!(saved.integration_recoveries[0].broker_observations.len(), 2);
    }
    #[test]
    fn first_recovered_lifetime_cannot_contradict_bound_pre_recovery_not_created() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::report::create(&root.path().join("run")).unwrap();
        let (mut report, mut journal) = saved_desktop(&store);
        let mut not_created = BrokerState {
            owner: BrokerOwner::Settled,
            primary: BrokerPrimary::NotCreated,
            descendants: BrokerDescendants::NotCreated,
            cleanup_complete: true,
            ..pending()
        };
        if let BrokerEffect::Receipt { receipt } = &mut not_created.effect {
            receipt.acknowledged = false;
        }
        journal.integration_actions[0].completion = Disposition::Failed {
            reason: "original desktop effect was not acknowledged".into(),
        };
        journal.integration_actions[0].terminal = Some(IntegrationOutcome::Failed {
            reason: "original desktop effect was not acknowledged".into(),
        });
        let original = journal.integration_actions[0].clone();
        let owner = journal.ownership[0].clone();
        let bound = bound_original_state(&report, &original, not_created.clone());
        report.integrations[0].desktops[0].observations.push(bound);
        let mut projector = RunProjector::new(
            &store,
            StoredRun::NativeIntegration(report.clone()),
            StoredJournal::NativeIntegration(journal),
        )
        .unwrap();
        let recovery_id = publish_recovery(&mut projector, &owner.ownership_id);
        let before = stored_bytes(&store);
        let IntegrationRequest::Desktop { request, .. } = &original.request else {
            unreachable!()
        };
        let identity = request.identity().unwrap();
        for (primary, descendants, owner, cleanup_complete) in [
            (
                BrokerPrimary::Running,
                BrokerDescendants::Pending,
                BrokerOwner::Retained,
                false,
            ),
            (
                BrokerPrimary::Exited,
                BrokerDescendants::Joined,
                BrokerOwner::Settled,
                true,
            ),
        ] {
            let bad = BrokerState {
                primary,
                descendants,
                owner,
                cleanup_complete,
                ..not_created.clone()
            };
            let observation = RecoveredBrokerObservation {
                original: action_reference(&original),
                identity: identity.clone(),
                source: BrokerObservationSource::Query,
                state: bad.clone(),
            };
            assert!(
                projector
                    .record_broker_recovery_observation(&recovery_id, observation.clone())
                    .is_err()
            );
            assert_eq!(stored_bytes(&store), before);
            let StoredJournal::NativeIntegration(saved) = projector.journal() else {
                unreachable!()
            };
            let mut forged = saved.clone();
            forged.integration_recoveries[0]
                .broker_observations
                .push(observation);
            assert!(
                validate_pair(&report, &forged).is_err(),
                "reload must compare recovery with the original bound report lifetime"
            );
            forged.integration_recoveries[0].broker_observations.insert(
                0,
                RecoveredBrokerObservation {
                    original: action_reference(&original),
                    identity: identity.clone(),
                    source: BrokerObservationSource::Query,
                    state: not_created.clone(),
                },
            );
            assert!(
                NativeIntegrationJournal::decode(&serde_json::to_vec(&forged).unwrap()).is_err(),
                "journal-only history must keep NotCreated irreversible"
            );
        }
        projector
            .record_broker_recovery_observation(
                &recovery_id,
                RecoveredBrokerObservation {
                    original: action_reference(&original),
                    identity,
                    source: BrokerObservationSource::Query,
                    state: not_created,
                },
            )
            .unwrap();
        assert_ne!(stored_bytes(&store), before);
        drop(projector);
        crate::native::storage::load_native_versioned(&store).unwrap();
    }
    #[test]
    fn not_created_primary_and_scope_each_remain_irreversible() {
        for primary in [BrokerPrimary::Running, BrokerPrimary::Exited] {
            let previous = BrokerState {
                primary: BrokerPrimary::NotCreated,
                ..BrokerState::unknown()
            };
            let next = BrokerState {
                primary,
                ..previous.clone()
            };
            assert!(validate_broker_progress(&previous, &next).is_err());
        }
        for descendants in [BrokerDescendants::Pending, BrokerDescendants::Joined] {
            let previous = BrokerState {
                descendants: BrokerDescendants::NotCreated,
                ..BrokerState::unknown()
            };
            let next = BrokerState {
                descendants,
                ..previous.clone()
            };
            assert!(validate_broker_progress(&previous, &next).is_err());
        }
    }
    #[test]
    fn earlier_unknown_observation_remains_saved_when_original_receipt_first_becomes_known() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::report::create(&root.path().join("run")).unwrap();
        let (report, journal) = saved_desktop(&store);
        let original = journal.integration_actions[0].clone();
        let owner = journal.ownership[0].clone();
        let mut projector = RunProjector::new(
            &store,
            StoredRun::NativeIntegration(report),
            StoredJournal::NativeIntegration(journal),
        )
        .unwrap();
        let recovery_id = publish_recovery(&mut projector, &owner.ownership_id);
        let mut cancels = 0;
        let queried = query_original_desktop_observing(
            &original,
            &CancellationToken::default(),
            |action| {
                let identity = match action {
                    Action::QueryDesktop { identity } => identity,
                    Action::CancelDesktop { identity } => {
                        cancels += 1;
                        identity
                    }
                    _ => panic!("only original control"),
                };
                let state = match cancels {
                    0 => BrokerState {
                        effect: BrokerEffect::Unknown,
                        ..pending()
                    },
                    1 => pending(),
                    _ => settled(),
                };
                Ok(v2::Reply::DesktopState {
                    identity: Box::new(identity),
                    state: Box::new(state),
                })
            },
            |observation| projector.record_broker_recovery_observation(&recovery_id, observation),
        )
        .unwrap();
        projector
            .record_integration_query(
                &recovery_id,
                OriginalQueryOutcome {
                    original: action_reference(&original),
                    view: OriginalQueryView::CachedTerminal,
                    outcome: queried.outcome.clone(),
                },
            )
            .unwrap();
        let IntegrationOutcome::Complete { receipt } = &queried.outcome else {
            panic!("original receipt")
        };
        projector
            .record_integration_completion(
                &original.lease,
                &original.operation,
                settled_disposition(receipt),
                queried.outcome,
            )
            .expect(
                "later first receipt must not retroactively invalidate earlier Unknown knowledge",
            );
        drop(projector);
        let (
            NativeReportVersion::IntegrationV2(report),
            NativeJournalVersion::IntegrationV2(saved),
        ) = crate::native::storage::load_native_versioned(&store).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(saved.integration_recoveries[0].broker_observations.len(), 3);
        assert!(matches!(
            saved.integration_recoveries[0].broker_observations[0]
                .state
                .effect,
            BrokerEffect::Unknown
        ));
        assert!(matches!(
            saved.integration_actions[0].terminal,
            Some(IntegrationOutcome::Complete { .. })
        ));
        assert!(
            saved.integration_recoveries[0]
                .prior_local
                .iter()
                .any(|p| !complete(p))
        );
        assert_eq!(
            saved.integration_recoveries[0]
                .broker_effect_baselines
                .len(),
            1
        );
        assert!(
            saved.integration_recoveries[0].broker_effect_baselines[0]
                .terminal
                .is_none()
        );
        let mut legacy = serde_json::to_value(&saved).unwrap();
        legacy["integration_recoveries"][0]
            .as_object_mut()
            .unwrap()
            .remove("broker_effect_baselines");
        let legacy=NativeIntegrationJournal::decode(&serde_json::to_vec(&legacy).unwrap()).expect("legacy omitted baseline must infer first terminal from original cached query provenance");
        assert!(
            legacy.integration_recoveries[0]
                .broker_effect_baselines
                .is_empty()
        );
        let mut invented = saved.clone();
        invented.integration_recoveries[0].queries.clear();
        assert!(
            NativeIntegrationJournal::decode(&serde_json::to_vec(&invented).unwrap()).is_err(),
            "a new terminal needs original cached query provenance"
        );
        let mut changed_baseline = saved.clone();
        changed_baseline.integration_recoveries[0].broker_effect_baselines[0].terminal =
            saved.integration_actions[0].terminal.clone();
        assert!(
            NativeIntegrationJournal::decode(&serde_json::to_vec(&changed_baseline).unwrap())
                .is_err(),
            "later knowledge cannot be inserted before the earlier Unknown sample"
        );
        let mut changed_query = saved.clone();
        changed_query.integration_actions[0].terminal = None;
        changed_query.integration_actions[0].completion =
            unknown("terminal barrier not yet written");
        changed_query.integration_recoveries[0].queries[0].outcome = IntegrationOutcome::Complete {
            receipt: "ff".repeat(32),
        };
        assert!(
            NativeIntegrationJournal::decode(&serde_json::to_vec(&changed_query).unwrap()).is_err(),
            "cached query provenance cannot change an already observed original receipt"
        );
        let terminal = saved.integration_actions[0].terminal.clone();
        let mut projector = RunProjector::new(
            &store,
            StoredRun::NativeIntegration(report),
            StoredJournal::NativeIntegration(saved),
        )
        .unwrap();
        let next = "20".repeat(16);
        projector
            .record_recovery_attempt(crate::native::model::NativeRecoveryAttempt {
                recovery_id: next.clone(),
                ownership_id: owner.ownership_id.clone(),
                prior_local: unknown("historical controller owner lost"),
                local: unknown("new local owner pending"),
                remote: unknown("provider not queried"),
            })
            .unwrap();
        publish_attempt(&mut projector, &next, &owner.ownership_id).unwrap();
        let StoredJournal::NativeIntegration(saved) = projector.journal() else {
            unreachable!()
        };
        assert_eq!(
            saved.integration_recoveries[1].broker_effect_baselines[0].terminal,
            terminal
        );
        let mut regressed = saved.clone();
        regressed.integration_recoveries[1].broker_effect_baselines[0].terminal = None;
        assert!(
            NativeIntegrationJournal::decode(&serde_json::to_vec(&regressed).unwrap()).is_err(),
            "later attempt cannot forget prior original query terminal knowledge"
        );
    }
    fn ready_desktop_pair(
        store: &ReportStore,
    ) -> (NativeIntegrationReport, NativeIntegrationJournal) {
        let (mut report, mut journal) = saved_desktop(store);
        let expectation = ReadinessExpectation {
            role: DefinitionRole::Configured,
            artifact: ArtifactRole::Target,
            artifact_length: 123,
            configuration_digest: "21".repeat(32),
            artifact_digest: "22".repeat(32),
            version: "target-v1".into(),
            data_digest: "23".repeat(32),
            initialized: true,
        };
        let expectation_digest = expectation.digest().unwrap();
        let action = &mut journal.integration_actions[0];
        let IntegrationRequest::Desktop { request, .. } = &mut action.request else {
            unreachable!()
        };
        if let DesktopAction::Activate {
            expectation_digest: original,
            ..
        } = &mut request.action
        {
            *original = expectation_digest.clone();
        }
        let close = request.close_request().unwrap().digest().unwrap().to_hex();
        if let DesktopAction::Activate { close_binding, .. } = &mut request.action {
            close_binding.action_digest = close;
        }
        let identity = request.identity().unwrap();
        action.broker_identity =
            Some(serde_json::from_value(serde_json::to_value(&identity).unwrap()).unwrap());
        action.request_digest = v2::request_digest(&action.request.wire().unwrap())
            .unwrap()
            .to_hex();
        action.completion = settled_disposition(&"0b".repeat(32));
        action.terminal = Some(IntegrationOutcome::Complete {
            receipt: "0b".repeat(32),
        });
        let action = action.clone();
        let challenge = ReadinessChallenge {
            resource_binding_digest: action.prior_binding_digest.clone(),
            action_digest: identity.request_digest.clone(),
            expectation_digest: expectation_digest.clone(),
            ownership_id: action.ownership_id.clone(),
            lease: identity.broker_lease.clone(),
            slot: action.slot.clone(),
            operation: action.operation.clone(),
            nonce: identity.activation_nonce.clone(),
            deadline_ms: 1000,
        };
        let ready = v2::ReadinessObservation {
            peer_sid: None,
            expectation: expectation.wire(),
            slot: v2::ResourceSlot::new(action.slot.as_str()).unwrap(),
            operation: action.operation.clone(),
            action_digest: identity.request_digest.clone(),
            resource_binding_digest: action.prior_binding_digest.clone(),
            challenge_digest: challenge.digest().unwrap(),
            expectation_digest: expectation_digest.clone(),
            state: v2::ReadinessState::Ready,
            authenticated_instance: true,
            instance_binding_digest: identity.request_digest,
            peer_uid: Some(1000),
            peer_gid: Some(1000),
            artifact_sha256: Some(expectation.artifact_digest.clone()),
            version_literal: Some(expectation.version.clone()),
            data_sha256: Some(expectation.data_digest.clone()),
            configuration_digest: Some(expectation.configuration_digest.clone()),
            initialized: Some(true),
            response_digest: Some("24".repeat(32)),
            failure: None,
        };
        let mut bound = bound_original_state(&report, &action, pending());
        bound.binding.id = "desktop-ready-original".into();
        bound.binding.phase = IntegrationPhase::Configured;
        bound.readiness = Some(ready);
        journal.readiness_probes.push(ReadinessProbeRecord {
            challenge,
            expectation,
            expectation_digest,
            observation_ids: vec![bound.binding.id.clone()],
            completion: settled_disposition(&"24".repeat(32)),
        });
        report.integrations[0].desktops[0].observations.push(bound);
        (report, journal)
    }
    #[test]
    fn desktop_ready_report_and_pair_use_original_instance_peer_and_private_receipt() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::report::create(&root.path().join("run")).unwrap();
        let (report, journal) = ready_desktop_pair(&store);
        report
            .validate()
            .expect("standalone desktop report uses desktop structural readiness contract");
        validate_pair(&report, &journal).unwrap();
        let mut wrong_receipt = journal.clone();
        wrong_receipt.readiness_probes[0].completion = settled_disposition(
            &report.integrations[0].desktops[0].observations[0]
                .binding
                .id,
        );
        assert!(
            validate_pair(&report, &wrong_receipt).is_err(),
            "public Ready must bind the exact private response digest"
        );
        let mut wrong_identity = report.clone();
        let ready = wrong_identity.integrations[0].desktops[0].observations[0]
            .readiness
            .as_mut()
            .unwrap();
        ready.action_digest = "ff".repeat(32);
        ready.instance_binding_digest = ready.action_digest.clone();
        assert!(
            wrong_identity.validate().is_ok(),
            "standalone fields cannot manufacture an independently trusted action identity"
        );
        assert!(validate_pair(&wrong_identity, &journal).is_err());
        let mut wrong_peer = report.clone();
        wrong_peer.integrations[0].desktops[0].observations[0]
            .readiness
            .as_mut()
            .unwrap()
            .peer_uid = Some(9999);
        assert!(wrong_peer.validate().is_ok());
        assert!(
            validate_pair(&wrong_peer, &journal).is_err(),
            "pair must use the original declared broker principal"
        );
    }
    #[test]
    fn desktop_driver_readiness_completion_uses_response_digest_and_stays_unknown_without_it() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::report::create(&root.path().join("run")).unwrap();
        let (report, _) = ready_desktop_pair(&store);
        let mut observation = report.integrations[0].desktops[0].observations[0].clone();
        assert_eq!(
            crate::native::integration::desktop::desktop_readiness_completion(&observation),
            settled_disposition(&"24".repeat(32)),
            "completion receipt must be the authenticated response digest, not an observation label"
        );
        observation.readiness.as_mut().unwrap().response_digest = None;
        assert!(matches!(
            crate::native::integration::desktop::desktop_readiness_completion(&observation),
            Disposition::Unknown { .. }
        ));
    }
    #[test]
    fn original_scope_history_and_lost_release_receipt_persist_without_rewriting_other_knowledge() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::report::create(&root.path().join("run")).unwrap();
        let (report, journal) = saved_desktop(&store);
        let original = journal.integration_actions[0].clone();
        let owner = journal.ownership[0].clone();
        let before_checks = report.integrations[0].checks.clone();
        let before_local = report.catalog.scenarios[0].dispositions.local.clone();
        let mut projector = RunProjector::new(
            &store,
            StoredRun::NativeIntegration(report),
            StoredJournal::NativeIntegration(journal),
        )
        .unwrap();
        let recovery_id = publish_recovery(&mut projector, &owner.ownership_id);
        let mut cancels = 0;
        let query = query_original_desktop_observing(
            &original,
            &CancellationToken::default(),
            |action| {
                let identity = match action {
                    Action::QueryDesktop { identity } => identity,
                    Action::CancelDesktop { identity } => {
                        cancels += 1;
                        identity
                    }
                    _ => panic!("only exact original Query/Cancel allowed"),
                };
                Ok(v2::Reply::DesktopState {
                    identity: Box::new(identity),
                    state: Box::new(if cancels >= 2 { settled() } else { pending() }),
                })
            },
            |observation| projector.record_broker_recovery_observation(&recovery_id, observation),
        )
        .unwrap();
        assert_eq!(cancels, 2);
        assert!(matches!(query.outcome, IntegrationOutcome::Complete { .. }));
        let StoredJournal::NativeIntegration(saved) = projector.journal() else {
            unreachable!()
        };
        assert!(
            saved.integration_actions[0].terminal.is_none(),
            "lifetime observation must not rewrite effect terminal"
        );
        assert!(
            matches!(
                saved.integration_ownership[0].disposition,
                Disposition::Unknown { .. }
            ),
            "native scope knowledge cannot rewrite the saved SUT resource disposition"
        );
        assert!(saved.integration_recoveries[0].broker_release.is_none());
        assert!(
            super::super::recovery::integration_release_blocked(saved, &owner.ownership_id),
            "unproved original effect still blocks a new Release"
        );
        let mut exact_effect = saved.clone();
        let receipt = "0b".repeat(32);
        exact_effect.integration_actions[0].completion = settled_disposition(&receipt);
        exact_effect.integration_actions[0].terminal =
            Some(IntegrationOutcome::Complete { receipt });
        exact_effect.integration_recoveries[0]
            .queries
            .push(OriginalQueryOutcome {
                original: action_reference(&original),
                view: OriginalQueryView::CachedTerminal,
                outcome: exact_effect.integration_actions[0]
                    .terminal
                    .clone()
                    .unwrap(),
            });
        exact_effect.validate().unwrap();
        assert!(
            !super::super::recovery::integration_release_blocked(
                &exact_effect,
                &owner.ownership_id
            ),
            "proved original scope permits cleanup release without upgrading the separate SUT resource Unknown"
        );
        exact_effect.integration_ownership[0].revocation =
            Some(AuthorityRevocation::ManagerRestarted);
        assert!(
            super::super::recovery::integration_release_blocked(&exact_effect, &owner.ownership_id),
            "a revoked original authority remains unresolved"
        );
        let lease = v2::LeaseId::parse(&original.lease).unwrap();
        let intent = v2::IntentId::from_bytes([0x17; 16]);
        reconcile_broker_release(
            &mut projector,
            &owner,
            lease,
            &v2::LeaseView::Held { intent },
            BrokerReleaseSource::QueryLease,
        )
        .unwrap();
        assert!(
            reconcile_broker_release(
                &mut projector,
                &owner,
                lease,
                &v2::LeaseView::AwaitingExternalReset {
                    intent: v2::IntentId::from_bytes([0xff; 16])
                },
                BrokerReleaseSource::QueryLease
            )
            .is_err()
        );
        reconcile_broker_release(
            &mut projector,
            &owner,
            lease,
            &v2::LeaseView::AwaitingExternalReset { intent },
            BrokerReleaseSource::QueryLease,
        )
        .unwrap();
        super::super::recovery::record_observed_release(&mut projector, &owner, lease).unwrap();
        drop(projector);
        let (
            NativeReportVersion::IntegrationV2(report),
            NativeJournalVersion::IntegrationV2(saved),
        ) = crate::native::storage::load_native_versioned(&store).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(report.integrations[0].checks, before_checks);
        assert_eq!(report.catalog.scenarios[0].dispositions.local, before_local);
        assert!(matches!(
            report.outcome,
            crate::model::RunOutcome::Incomplete
        ));
        assert_eq!(saved.integration_actions[0], original);
        assert!(
            saved.releases.is_empty(),
            "query evidence must not fabricate a missing Release publication"
        );
        assert_eq!(saved.integration_recoveries[0].broker_observations.len(), 2);
        assert_eq!(
            saved.integration_recoveries[0]
                .broker_observations
                .last()
                .unwrap()
                .state,
            settled()
        );
        assert_eq!(
            saved.integration_recoveries[0]
                .broker_release
                .as_ref()
                .unwrap()
                .source,
            BrokerReleaseSource::QueryLease
        );
        assert!(matches!(
            saved.integration_recoveries[0].broker,
            Disposition::Complete { .. }
        ));
        assert!(
            saved.integration_recoveries[0]
                .prior_local
                .iter()
                .any(|p| !complete(p))
        );
        assert!(matches!(
            saved.recovery_attempts[0].local,
            Disposition::Unknown { .. }
        ));
    }
    #[test]
    fn lost_quarantined_state_is_saved_and_cannot_be_replaced_by_an_invented_owner() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::report::create(&root.path().join("run")).unwrap();
        let (report, journal) = saved_desktop(&store);
        let original = journal.integration_actions[0].clone();
        let owner = journal.ownership[0].clone();
        let checks = report.integrations[0].checks.clone();
        let mut projector = RunProjector::new(
            &store,
            StoredRun::NativeIntegration(report),
            StoredJournal::NativeIntegration(journal),
        )
        .unwrap();
        let recovery_id = publish_recovery(&mut projector, &owner.ownership_id);
        let lost = BrokerState {
            quarantined: true,
            ..BrokerState::unknown()
        };
        let result = query_original_desktop_observing(
            &original,
            &CancellationToken::default(),
            |action| {
                let Action::QueryDesktop { identity } = action else {
                    panic!("lost owner never authorizes Cancel, Activate or Close")
                };
                Ok(v2::Reply::DesktopState {
                    identity: Box::new(identity),
                    state: Box::new(lost.clone()),
                })
            },
            |observation| projector.record_broker_recovery_observation(&recovery_id, observation),
        )
        .unwrap();
        assert!(!result.cached_terminal);
        assert!(matches!(result.outcome, IntegrationOutcome::Unknown { .. }));
        let IntegrationRequest::Desktop { request, .. } = &original.request else {
            unreachable!()
        };
        let before = match projector.journal() {
            StoredJournal::NativeIntegration(j) => j.encode().unwrap(),
            _ => unreachable!(),
        };
        assert!(
            projector
                .record_broker_recovery_observation(
                    &recovery_id,
                    RecoveredBrokerObservation {
                        original: action_reference(&original),
                        identity: request.identity().unwrap(),
                        source: BrokerObservationSource::Query,
                        state: pending()
                    }
                )
                .is_err()
        );
        let StoredJournal::NativeIntegration(saved) = projector.journal() else {
            unreachable!()
        };
        assert_eq!(
            saved.encode().unwrap(),
            before,
            "rejection preserves the saved original knowledge"
        );
        reconcile_broker_release(
            &mut projector,
            &owner,
            v2::LeaseId::parse(&original.lease).unwrap(),
            &v2::LeaseView::AwaitingExternalReset {
                intent: v2::IntentId::from_bytes([0x17; 16]),
            },
            BrokerReleaseSource::QueryLease,
        )
        .unwrap();
        drop(projector);
        let (
            NativeReportVersion::IntegrationV2(report),
            NativeJournalVersion::IntegrationV2(saved),
        ) = crate::native::storage::load_native_versioned(&store).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(report.integrations[0].checks, checks);
        assert_eq!(
            saved.integration_recoveries[0].broker_observations[0].state,
            lost
        );
        assert!(saved.integration_recoveries[0].broker_release.is_some());
        assert!(matches!(
            saved.integration_recoveries[0].broker,
            Disposition::Unknown { .. }
        ));
        assert!(matches!(
            saved.integration_ownership[0].disposition,
            Disposition::Unknown { .. }
        ));
        assert_eq!(saved.integration_actions[0], original);
    }
    #[test]
    fn original_desktop_cancel_is_polled_until_independent_scope_settles() {
        let original = original();
        let IntegrationRequest::Desktop { request, .. } = &original.request else {
            unreachable!()
        };
        let identity = request.identity().unwrap();
        let mut trace = Vec::new();
        let mut cancels = 0;
        let result = query_original_desktop(&original, &CancellationToken::default(), |action| {
            let state = match action {
                Action::QueryDesktop { identity: observed } => {
                    assert_eq!(observed, identity);
                    trace.push("query");
                    if cancels >= 2 { settled() } else { pending() }
                }
                Action::CancelDesktop { identity: observed } => {
                    assert_eq!(observed, identity);
                    trace.push("cancel");
                    cancels += 1;
                    if cancels >= 2 { settled() } else { pending() }
                }
                _ => panic!("recovery must not prepare/acquire/activate/request-close"),
            };
            Ok(v2::Reply::DesktopState {
                identity: Box::new(identity.clone()),
                state: Box::new(state),
            })
        })
        .unwrap();
        assert_eq!(
            cancels, 2,
            "one nonblocking cleanup call is not a scope join"
        );
        assert_eq!(trace, ["query", "cancel", "query", "cancel", "query"]);
        assert!(matches!(
            result.outcome,
            IntegrationOutcome::Complete { .. }
        ));
    }
    #[test]
    fn lost_unknown_desktop_owner_never_gets_a_cancel_or_fresh_effect() {
        let original = original();
        let mut count = 0;
        let result = query_original_desktop(&original, &CancellationToken::default(), |action| {
            let Action::QueryDesktop { identity } = action else {
                panic!("lost native ownership grants no effect authority")
            };
            count += 1;
            Ok(v2::Reply::DesktopState {
                identity: Box::new(identity),
                state: Box::new(BrokerState::unknown()),
            })
        })
        .unwrap();
        assert_eq!(count, 1);
        assert!(matches!(result.outcome, IntegrationOutcome::Unknown { .. }));
        assert!(!result.cached_terminal);
    }
}
