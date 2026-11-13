//! Explicit query-only recovery. Saved journals import knowledge, never execution authority.
use super::{
    CancellationToken, EngineEvent, EventKind,
    domain::{StoredJournal, StoredRun},
    projector::RunProjector,
    selected::{Owners, event, retained_failure, settle_owned},
};
use crate::{
    model::RunOutcome,
    native::{
        launch::LaunchAuthority,
        model::*,
        provider::{NativeProvider, validate_baseline},
        transport::NativeSession,
    },
    report::ReportStore,
};
use anyhow::{Result, bail, ensure};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    time::{Duration, Instant},
};
use theseus_native_client::{
    Action, ClientMode, Event, Poll, RecoveredOperation, RecoveryImage, RequestId,
};
use theseus_native_helper_store::{IntentRecord, VerifiedOriginalIntent};
use theseus_native_protocol::v2;

mod outcome;

pub struct NativeRecoveryResult {
    pub report: NativeReport,
    pub journal: NativeJournal,
    pub report_path: PathBuf,
    pub prior_local: Vec<Disposition>,
    pub local: Disposition,
    pub remote: Disposition,
}
impl NativeRecoveryResult {
    pub fn complete(&self) -> bool {
        matches!(self.local, Disposition::Complete { .. })
            && matches!(self.remote, Disposition::Complete { .. })
            && self
                .prior_local
                .iter()
                .all(|state| matches!(state, Disposition::Complete { .. }))
    }
}
struct Admission {
    owner: NativeResource,
    launch: Option<LaunchAuthority>,
    mode: Option<ClientMode>,
    original: Option<VerifiedOriginalIntent>,
}
fn disposition_outcome(disposition: &Disposition) -> RunOutcome {
    match disposition {
        Disposition::Complete { .. } => RunOutcome::Passed,
        Disposition::Failed { .. } => RunOutcome::Failed,
        Disposition::Unknown { .. } | Disposition::NotStarted => RunOutcome::Incomplete,
    }
}
fn nonce() -> Result<String> {
    let mut bytes = [0; 16];
    theseus_host::random_bytes(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
/// Borrowed original core fields, independent of the selected storage envelope.
struct RecoveryJournal<'a> {
    launch_fingerprint: &'a Option<String>,
    ownership: &'a [NativeResource],
    operations: &'a [NativeOperation],
    provider_calls: &'a [NativeProviderCall],
    releases: &'a [NativeRelease],
    recovery_attempts: &'a [NativeRecoveryAttempt],
}
fn journal_view(journal: &StoredJournal) -> RecoveryJournal<'_> {
    match journal {
        StoredJournal::Native(j) => RecoveryJournal {
            launch_fingerprint: &j.launch_fingerprint,
            ownership: &j.ownership,
            operations: &j.operations,
            provider_calls: &j.provider_calls,
            releases: &j.releases,
            recovery_attempts: &j.recovery_attempts,
        },
        StoredJournal::NativeIntegration(j) => RecoveryJournal {
            launch_fingerprint: &j.launch_fingerprint,
            ownership: &j.ownership,
            operations: &j.operations,
            provider_calls: &j.provider_calls,
            releases: &j.releases,
            recovery_attempts: &j.recovery_attempts,
        },
        _ => unreachable!("native recovery admits native envelopes only"),
    }
}
fn journal<'a>(projector: &'a RunProjector<'_>) -> RecoveryJournal<'a> {
    journal_view(projector.journal())
}
fn scenarios(report: &StoredRun) -> &[NativeScenario] {
    match report {
        StoredRun::Native(r) => &r.scenarios,
        StoredRun::NativeIntegration(r) => &r.catalog.scenarios,
        _ => unreachable!("native recovery admits native envelopes only"),
    }
}
fn scenarios_mut<'a>(projector: &'a mut RunProjector<'_>) -> &'a mut Vec<NativeScenario> {
    projector
        .catalog_scenarios_mut()
        .expect("admitted native report")
}
fn original_mode(
    saved: &StoredJournal,
    owner: &NativeResource,
    launch: &LaunchAuthority,
    original: VerifiedOriginalIntent,
) -> Result<ClientMode> {
    let (prepared, lease) = match &owner.ownership {
        NativeOwnership::Intent { prepared } => (prepared, None),
        NativeOwnership::Acquired { prepared, lease } => {
            (prepared, Some(v2::LeaseId::parse(lease)?))
        }
        NativeOwnership::Preparing { .. } => bail!("preparation has no original native intent"),
    };
    let expected = launch.binding();
    ensure!(
        prepared.binding == expected.binding.to_hex()
            && prepared.manifest_digest == expected.manifest_digest.to_hex()
            && prepared.workspace_marker == expected.workspace_marker.to_hex()
            && prepared.reset_epoch == launch.provider_epoch().to_hex(),
        "recovery launch differs from original binding/epoch"
    );
    let mut operations = journal_view(saved)
        .operations
        .iter()
        .filter(|o| o.ownership_id == owner.ownership_id)
        .map(|o| {
            Ok(RecoveredOperation {
                request: o.request()?,
                digest: v2::Digest::parse(&o.request_digest)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if let StoredJournal::NativeIntegration(j) = saved {
        for binding in j
            .broker_bindings
            .iter()
            .filter(|b| b.ownership_id == owner.ownership_id)
        {
            let request = v2::Request::BrokerBind {
                original: Box::new(binding.original.clone()),
            };
            operations.push(RecoveredOperation {
                digest: v2::request_digest(&request)?,
                request,
            });
        }
        for action in j
            .integration_actions
            .iter()
            .filter(|a| a.ownership_id == owner.ownership_id)
        {
            operations.push(RecoveredOperation {
                request: action.request.wire()?,
                digest: v2::Digest::parse(&action.request_digest)?,
            });
        }
    }
    Ok(ClientMode::Recover {
        image: Box::new(RecoveryImage {
            binding: original.record().clone(),
            profile: launch.expected().profile,
            operations,
            lease,
        }),
        original,
    })
}
fn current(projector: &RunProjector<'_>, id: &str) -> NativeResource {
    journal(projector)
        .ownership
        .iter()
        .find(|owner| owner.ownership_id == id)
        .expect("admitted original owner")
        .clone()
}
fn validate_context(launch: &LaunchAuthority, invocation: &ProviderInvocation) -> Result<()> {
    invocation.validate()?;
    ensure!(
        invocation.provider == launch.provider_name()
            && invocation.target_id == launch.view().target_id
            && invocation.baseline_label == launch.baseline_label()
            && invocation.launch_fingerprint == launch.view().fingerprint,
        "recovery provider context changed"
    );
    Ok(())
}
fn validate_disposition(
    launch: &LaunchAuthority,
    owner: &NativeResource,
    invocation: &ProviderInvocation,
    receipt: &ProviderReceipt,
) -> Result<()> {
    validate_context(launch, invocation)?;
    receipt.validate()?;
    ensure!(
        invocation.prior_resource == *owner
            && receipt.invocation_digest == invocation.digest()?
            && receipt.provider == invocation.provider
            && receipt.invocation_id == invocation.invocation_id,
        "recovery disposition identity differs from original"
    );
    ensure!(
        receipt.manifest_digest == launch.binding().manifest_digest.to_hex(),
        "recovery disposition exceeds launch ceiling"
    );
    v2::Digest::parse(&receipt.reset_epoch)?;
    let previous = match &owner.ownership {
        NativeOwnership::Preparing {
            receipt: Some(receipt),
            ..
        } => Some(&receipt.reset_epoch),
        NativeOwnership::Intent { prepared } | NativeOwnership::Acquired { prepared, .. } => {
            Some(&prepared.reset_epoch)
        }
        _ => None,
    };
    ensure!(
        previous.is_none_or(|epoch| epoch != &receipt.reset_epoch),
        "recovery disposition did not advance original epoch"
    );
    Ok(())
}
fn wait(session: &mut NativeSession, id: RequestId) -> Result<v2::Reply> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        ensure!(
            Instant::now() < deadline,
            "recovery reply unproved at deadline"
        );
        match session.poll()? {
            Poll::Event(Event::Reply { request, reply }) if request == id => return Ok(reply),
            Poll::Event(Event::ConnectionLost | Event::PeerClosed(_)) | Poll::TransportSettled => {
                bail!("recovery connection lost; original identities retained")
            }
            _ => std::thread::yield_now(),
        }
    }
}
fn request(session: &mut NativeSession, action: Action<'_>) -> Result<v2::Reply> {
    let id = session.submit(action)?;
    wait(session, id)
}
fn validate_known_output(known: &NativeCommandEvidence, outcome: &v2::Outcome) -> Result<()> {
    if known.stdout_complete || known.stderr_complete {
        let v2::Outcome::RunCompleted {
            truncated,
            stdout_bytes,
            ..
        } = outcome
        else {
            bail!("original output evidence changed to non-run outcome")
        };
        ensure!(!truncated, "original complete output changed to truncated");
        if known.stdout_complete && known.stdout_utf8 {
            ensure!(
                *stdout_bytes == known.stdout.len() as u64,
                "original complete stdout byte count changed"
            );
        }
    }
    Ok(())
}
fn operations(
    projector: &mut RunProjector<'_>,
    session: &mut NativeSession,
    id: &str,
    lease: v2::LeaseId,
    cancel: &CancellationToken,
) -> Result<()> {
    let integration = matches!(projector.journal(), StoredJournal::NativeIntegration(_));
    let originals = journal(projector)
        .operations
        .iter()
        .filter(|operation| operation.ownership_id == id)
        .cloned()
        .collect::<Vec<_>>();
    for original in originals {
        let operation = v2::OperationId::parse(&original.operation_id)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut cancellation = false;
        loop {
            if integration && (Instant::now() >= deadline || cancel.is_cancelled()) {
                if matches!(original.completion, Disposition::Unknown { .. }) {
                    projector.record_operation_completion(id, &original.lease, &original.operation_id,
                        Disposition::Unknown { reason: "original SUT operation settlement unproved at deadline/cancellation; never replayed".into() })?;
                }
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "original operation settlement remains unknown"
            );
            let state = match request(session, Action::Query { lease, operation })? {
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
                _ => bail!("original operation query refused"),
            };
            let completion = match state {
                v2::OperationView::Finished { outcome } => {
                    let known = scenarios_mut(projector)
                        .iter()
                        .filter(|scenario| scenario.ownership_id == id)
                        .flat_map(|scenario| scenario.stages.iter())
                        .flat_map(|stage| stage.commands.iter())
                        .find(|command| command.operation_id == original.operation_id)
                        .cloned();
                    if let Some(known) = known {
                        ensure!(
                            known.command == original.command
                                && known.request_digest == original.request_digest,
                            "recovery command evidence differs from original request"
                        );
                        validate_known_output(&known, &outcome)?;
                        if let Some(status) = known.status {
                            let v2::Outcome::RunCompleted {
                                status: observed, ..
                            } = &outcome
                            else {
                                bail!("original terminal status changed to non-run outcome")
                            };
                            ensure!(
                                status == NativeStatus::from(observed.clone()),
                                "original terminal operation status changed"
                            );
                        }
                    }
                    Some(Disposition::Complete { receipt: "original terminal operation queried; missing historical output remains unavailable".into() })
                }
                v2::OperationView::Missing => {
                    ensure!(
                        !matches!(original.completion, Disposition::Complete { .. }),
                        "original terminal operation is missing"
                    );
                    Some(if integration {
                        Disposition::Unknown { reason: "original SUT operation missing; native owner cannot be reconstructed or replayed".into() }
                    } else {
                        Disposition::Failed {
                            reason: "original operation not found; never replayed".into(),
                        }
                    })
                }
                v2::OperationView::Unknown { reason } => {
                    if !integration {
                        bail!("original operation disposition unknown; never replayed");
                    }
                    ensure!(
                        !matches!(original.completion, Disposition::Complete { .. }),
                        "original terminal SUT operation changed to Unknown"
                    );
                    ensure!(
                        !scenarios_mut(projector)
                            .iter()
                            .filter(|s| s.ownership_id == id)
                            .flat_map(|s| &s.stages)
                            .flat_map(|s| &s.commands)
                            .any(|c| c.operation_id == original.operation_id && c.status.is_some()),
                        "known terminal SUT status changed to Unknown"
                    );
                    Some(Disposition::Unknown {
                        reason: format!(
                            "original SUT native owner unknown: {reason:?}; never replayed"
                        ),
                    })
                }
                v2::OperationView::Accepted { .. } | v2::OperationView::Running => None,
            };
            if let Some(completion) = completion {
                if matches!(original.completion, Disposition::Unknown { .. }) {
                    projector.record_operation_completion(
                        id,
                        &original.lease,
                        &original.operation_id,
                        completion,
                    )?;
                }
                break;
            }
            if !cancellation {
                match request(session, Action::Cancel { lease, operation })? {
                    v2::Reply::Operation {
                        lease: l,
                        operation: o,
                        ..
                    } if l == lease && o == operation => cancellation = true,
                    v2::Reply::Refused {
                        code: v2::RefusalCode::OwnershipPending,
                        ..
                    } => {}
                    _ => bail!("original operation cancellation refused"),
                }
            }
            if !integration {
                ensure!(
                    !cancel.is_cancelled(),
                    "recovery cancelled while original operation unsettled"
                );
            }
        }
    }
    Ok(())
}
pub(super) fn integration_release_blocked(
    j: &crate::native::integration::model::NativeIntegrationJournal,
    owner: &str,
) -> bool {
    j.operations.iter().any(|o| {
        o.ownership_id == owner
            && matches!(
                o.completion,
                Disposition::Unknown { .. } | Disposition::NotStarted
            )
    }) || j.integration_actions.iter().any(|a| {
        a.ownership_id == owner
            && matches!(
                a.completion,
                Disposition::Unknown { .. } | Disposition::NotStarted
            )
    }) || j.integration_ownership.iter().any(|r| {
        r.key.ownership_id == owner
            && (r.revocation.is_some()
                || (matches!(
                    r.disposition,
                    Disposition::Unknown { .. } | Disposition::NotStarted
                ) && !(r.kind
                    == crate::native::integration::model::IntegrationResourceKind::Desktop
                    && crate::native::integration::model::original_desktop_scope_settled(
                        j,
                        owner,
                        &r.key.slot,
                    ))))
    })
}
fn recover_session(
    projector: &mut RunProjector<'_>,
    session: &mut NativeSession,
    owner: &NativeResource,
    original: &VerifiedOriginalIntent,
    cancel: &CancellationToken,
) -> Result<()> {
    let record = original.record();
    let state = match request(session, Action::QueryOriginalAcquire(original))? {
        v2::Reply::Acquired {
            intent,
            binding,
            manifest_digest,
            state,
        } if intent == record.intent
            && binding == record.binding
            && manifest_digest == record.manifest_digest =>
        {
            state
        }
        _ => bail!("original acquisition query refused"),
    };
    let (lease, state) = match state {
        v2::AcquireView::NotFound => {
            ensure!(
                matches!(owner.ownership, NativeOwnership::Intent { .. })
                    && !journal(projector)
                        .operations
                        .iter()
                        .any(|operation| operation.ownership_id == owner.ownership_id)
                    && !journal(projector)
                        .releases
                        .iter()
                        .any(|release| release.ownership_id == owner.ownership_id),
                "original acquisition missing despite retained lease or operation evidence"
            );
            return Ok(());
        }
        v2::AcquireView::Lease { lease, state } => (lease, state),
    };
    projector.record_recovered_lease(&owner.ownership_id, &lease.to_hex())?;
    if matches!(projector.journal(), StoredJournal::NativeIntegration(_)) {
        super::integration_recovery::recover_actions(projector, session, owner, lease, cancel)?;
    }
    operations(projector, session, &owner.ownership_id, lease, cancel)?;
    // Query the exact lease before the release gate: a lost final Release reply
    // remains recoverable even when SUT checks or historical local owners are unknown.
    let desktop = matches!(projector.journal(), StoredJournal::NativeIntegration(j)
        if j.broker_bindings.iter().any(|b|b.ownership_id==owner.ownership_id));
    let state = if desktop {
        match request(session, Action::QueryLease(lease))? {
            v2::Reply::Lease {
                lease: observed,
                state,
            } if observed == lease => state,
            _ => bail!("original lease query refused"),
        }
    } else {
        state
    };
    if desktop && matches!(state, v2::LeaseView::AwaitingExternalReset { .. }) {
        super::integration_recovery::reconcile_broker_release(
            projector,
            owner,
            lease,
            &state,
            crate::native::integration::model::BrokerReleaseSource::QueryLease,
        )?;
        record_observed_release(projector, owner, lease)?;
    }
    if let StoredJournal::NativeIntegration(j) = projector.journal() {
        let unresolved = integration_release_blocked(j, &owner.ownership_id);
        if unresolved {
            // An external Dispose may independently settle the supplied target,
            // but no Release can claim an unproved original native owner settled.
            let scenario = scenarios_mut(projector)
                .iter_mut()
                .find(|s| s.ownership_id == owner.ownership_id)
                .expect("original scenario");
            if !matches!(scenario.dispositions.release, Disposition::Complete { .. }) {
                scenario.dispositions.release = Disposition::Unknown {
                    reason: "original native ownership unresolved; no Release submitted".into(),
                };
            }
            return projector.save_report();
        }
    }

    let state = if !desktop && matches!(state, v2::LeaseView::Held { .. }) {
        match request(session, Action::QueryLease(lease))? {
            v2::Reply::Lease {
                lease: observed,
                state,
            } if observed == lease => state,
            _ => bail!("original lease query refused"),
        }
    } else {
        state
    };
    let state = if matches!(state, v2::LeaseView::Held { .. }) {
        let saved = projector.publish_recovery_release(&owner.ownership_id)?;
        scenarios_mut(projector)
            .iter_mut()
            .find(|scenario| scenario.ownership_id == owner.ownership_id)
            .expect("admitted scenario")
            .dispositions
            .release = Disposition::Unknown {
            reason: "original recovery release published; settlement unproved".into(),
        };
        projector.save_report()?;
        let state = session.release(saved)?;
        if desktop {
            super::integration_recovery::reconcile_broker_release(
                projector,
                owner,
                lease,
                &state,
                crate::native::integration::model::BrokerReleaseSource::ReleaseReply,
            )?;
        }
        state
    } else {
        state
    };
    ensure!(
        matches!(state, v2::LeaseView::AwaitingExternalReset { .. }),
        "original lease remains quarantined or held"
    );
    record_observed_release(projector, owner, lease)
}
pub(super) fn record_observed_release(
    projector: &mut RunProjector<'_>,
    owner: &NativeResource,
    lease: v2::LeaseId,
) -> Result<()> {
    let completion = Disposition::Complete {
        receipt: "original lease queried awaiting external reset".into(),
    };
    if journal(projector).releases.iter().any(|release| {
        release.ownership_id == owner.ownership_id
            && matches!(release.completion, Disposition::Unknown { .. })
    }) {
        projector.record_release_completion(
            &owner.ownership_id,
            &lease.to_hex(),
            completion.clone(),
        )?;
    }
    scenarios_mut(projector)
        .iter_mut()
        .find(|scenario| scenario.ownership_id == owner.ownership_id)
        .expect("admitted scenario")
        .dispositions
        .release = completion;
    projector.save_report()
}
/// Consume an already opened retained native run store. Parsing saved endpoints
/// never supplies authority; all original intent bytes are verified before effects.
pub fn recover_native(
    store: ReportStore,
    launch: &LaunchAuthority,
    cancel: CancellationToken,
    emit: impl FnMut(EngineEvent),
) -> Result<NativeRecoveryResult> {
    let saved_report = store.load_native_report()?;
    let saved_journal = store.load_native_journal()?;
    let recovered = recover_stored_native(
        store,
        launch,
        cancel,
        emit,
        StoredRun::Native(saved_report),
        StoredJournal::Native(saved_journal),
    )?;
    let StoredRun::Native(report) = recovered.report else {
        unreachable!()
    };
    let StoredJournal::Native(journal) = recovered.journal else {
        unreachable!()
    };
    Ok(NativeRecoveryResult {
        report,
        journal,
        report_path: recovered.report_path,
        prior_local: recovered.prior_local,
        local: recovered.local,
        remote: recovered.remote,
    })
}
/// The selected outer envelope remains owned throughout admission, publication,
/// transport settlement, provider disposal and retained failure handling.
pub(crate) struct RecoveredNativeCore {
    pub report: StoredRun,
    pub journal: StoredJournal,
    pub report_path: PathBuf,
    pub prior_local: Vec<Disposition>,
    pub local: Disposition,
    pub remote: Disposition,
}
pub(crate) fn recover_stored_native(
    store: ReportStore,
    launch: &LaunchAuthority,
    cancel: CancellationToken,
    mut emit: impl FnMut(EngineEvent),
    saved_report: StoredRun,
    saved_journal: StoredJournal,
) -> Result<RecoveredNativeCore> {
    let integration = matches!(saved_report, StoredRun::NativeIntegration(_));
    let saved_core = journal_view(&saved_journal);
    ensure!(
        saved_core.launch_fingerprint.as_deref() == Some(launch.view().fingerprint.as_str()),
        "recovery requires unchanged explicit launch fingerprint"
    );
    ensure!(
        !saved_core.ownership.is_empty(),
        "native recovery requires original ownership"
    );
    let intents = if saved_core
        .ownership
        .iter()
        .any(|owner| !matches!(owner.ownership, NativeOwnership::Preparing { .. }))
    {
        Some(store.open_intent_store()?)
    } else {
        None
    };
    let mut admissions = vec![];
    for owner in saved_core.ownership {
        ensure!(
            scenarios(&saved_report)
                .iter()
                .any(|scenario| scenario.ownership_id == owner.ownership_id
                    && scenario.id == owner.scenario_id),
            "recovery original scenario missing"
        );
        let mut admission = Admission {
            owner: owner.clone(),
            launch: None,
            mode: None,
            original: None,
        };
        if let NativeOwnership::Intent { prepared } | NativeOwnership::Acquired { prepared, .. } =
            &owner.ownership
        {
            let active =
                launch.for_recovery(prepared, saved_core.launch_fingerprint.as_ref().unwrap())?;
            let intents = intents
                .as_ref()
                .expect("retained original intent namespace");
            let record = IntentRecord {
                intent: v2::IntentId::parse(&prepared.intent)?,
                binding: active.binding().binding,
                manifest_digest: active.binding().manifest_digest,
                workspace_marker: active.binding().workspace_marker,
            };
            admission.original = Some(intents.verify_original(&record)?);
            admission.mode = Some(original_mode(
                &saved_journal,
                owner,
                &active,
                intents.verify_original(&record)?,
            )?);
            admission.launch = Some(active);
        }
        for call in saved_core
            .provider_calls
            .iter()
            .filter(|call| call.invocation.ownership_id == owner.ownership_id)
        {
            validate_context(launch, &call.invocation)?;
        }
        admissions.push(admission);
    }
    let path = store.native_report_path();
    let mut projector = RunProjector::new(&store, saved_report, saved_journal)?;
    let mut owners = Owners::default();
    let mut prior_local = journal(&projector)
        .recovery_attempts
        .iter()
        .map(|attempt| {
            if matches!(attempt.local, Disposition::Complete { .. }) {
                attempt.local.clone()
            } else {
                Disposition::Unknown {
                    reason: "prior recovery process settlement remains unproved".into(),
                }
            }
        })
        .collect::<Vec<_>>();
    let mut all_remote = true;
    let mut local = Disposition::Complete {
        receipt: "no recovery transport required".into(),
    };
    event(&mut owners, &mut emit, EventKind::Started, &path, None);
    for mut admission in admissions {
        if !owners.panics.is_empty() {
            break;
        }
        let id = admission.owner.ownership_id.clone();
        let recovery_id = nonce()?;
        let scenario = scenarios_mut(&mut projector)
            .iter_mut()
            .find(|scenario| scenario.ownership_id == id)
            .unwrap();
        let prior = if matches!(scenario.dispositions.local, Disposition::Complete { .. }) {
            scenario.dispositions.local.clone()
        } else {
            Disposition::Unknown {
                reason:
                    "prior controller local process settlement cannot be reconstructed from journal"
                        .into(),
            }
        };
        scenario.dispositions.local = prior.clone();
        prior_local.push(prior.clone());
        projector.save_report()?;
        projector.record_recovery_attempt(NativeRecoveryAttempt {
            recovery_id: recovery_id.clone(),
            ownership_id: id.clone(),
            prior_local: prior,
            local: Disposition::Unknown {
                reason: "recovery attempt published; new local settlement unproved".into(),
            },
            remote: Disposition::Unknown {
                reason: "original remote settlement not queried".into(),
            },
        })?;
        if integration {
            super::integration_recovery::publish_attempt(&mut projector, &recovery_id, &id)?;
        }
        let mut remote_complete = false;
        let mut provider_confirmed = false;
        let work = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
            let calls = journal(&projector)
                .provider_calls
                .iter()
                .filter(|call| call.invocation.ownership_id == id)
                .cloned()
                .collect::<Vec<_>>();
            let disposition = calls.iter().rev().find(|call| {
                matches!(
                    call.invocation.action,
                    ProviderAction::Reset | ProviderAction::Dispose
                )
            });
            if let Some(call) = disposition {
                if let Some(receipt) = &call.receipt {
                    validate_disposition(
                        launch,
                        &current(&projector, &id),
                        &call.invocation,
                        receipt,
                    )?;
                    remote_complete = true;
                    return Ok(());
                }
                owners.provider = Some(launch.provider_factory()?);
                let receipt = owners
                    .provider
                    .as_mut()
                    .unwrap()
                    .query(&call.invocation)?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "original provider disposition remains unknown; never replayed"
                        )
                    })?;
                validate_disposition(
                    launch,
                    &current(&projector, &id),
                    &call.invocation,
                    &receipt,
                )?;
                projector.record_provider_receipt(&call.invocation, receipt)?;
                remote_complete = true;
                return Ok(());
            }
            let prepare = calls
                .iter()
                .find(|call| call.invocation.action == ProviderAction::Prepare)
                .ok_or_else(|| anyhow::anyhow!("original provider preparation missing"))?;
            owners.provider = Some(launch.provider_factory()?);
            let receipt = owners
                .provider
                .as_mut()
                .unwrap()
                .query(&prepare.invocation)?
                .ok_or_else(|| {
                    anyhow::anyhow!("original provider preparation remains unknown; never replayed")
                })?;
            let baseline = validate_baseline(launch, &prepare.invocation, receipt.clone())?;
            if let Some(saved) = &prepare.receipt {
                ensure!(saved == &receipt, "provider query changed original receipt");
            } else {
                projector.record_provider_receipt(&prepare.invocation, receipt)?;
            }
            provider_confirmed = true;
            if let Some(mut active) = admission.launch.take() {
                if integration {
                    let bound = launch.with_baseline(&baseline)?;
                    ensure!(
                        bound.binding().binding == active.binding().binding
                            && bound.binding().manifest_digest == active.binding().manifest_digest
                            && bound.binding().workspace_marker
                                == active.binding().workspace_marker
                            && bound.provider_epoch() == active.provider_epoch(),
                        "queried original preparation differs from original intent"
                    );
                    active = bound;
                }
                match NativeSession::start_authorized(&active, admission.mode.take().unwrap()) {
                    Ok(session) => owners.session = Some(session),
                    Err(failure) => {
                        owners.start = Some(failure);
                        bail!("recovery startup failed; original owner retained")
                    }
                }
                owners.session.as_mut().unwrap().wait_ready()?;
                recover_session(
                    &mut projector,
                    owners.session.as_mut().unwrap(),
                    &admission.owner,
                    admission.original.as_ref().unwrap(),
                    &cancel,
                )?;
            }
            Ok(())
        }));
        match work {
            Ok(Ok(())) => {}
            Ok(Err(error)) => owners.errors.push(error),
            Err(payload) => owners.panics.push(payload),
        }
        let settled = settle_owned(&mut owners);
        local = if settled {
            Disposition::Complete {
                receipt: "new recovery process and I/O owners joined".into(),
            }
        } else {
            Disposition::Unknown {
                reason: "new recovery owners retained".into(),
            }
        };
        if settled && !remote_complete && owners.provider.is_some() {
            let no_pending_disposition = !journal(&projector).provider_calls.iter().any(|call| {
                call.invocation.ownership_id == id
                    && matches!(
                        call.invocation.action,
                        ProviderAction::Reset | ProviderAction::Dispose
                    )
            });
            let known_prepare = journal(&projector).provider_calls.iter().any(|call| {
                call.invocation.ownership_id == id
                    && call.invocation.action == ProviderAction::Prepare
                    && call.receipt.is_some()
            });
            if no_pending_disposition && known_prepare && provider_confirmed {
                let dispose = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
                    let resource = current(&projector, &id);
                    let invocation = ProviderInvocation {
                        ownership_id: id.clone(),
                        provider: launch.provider_name().into(),
                        invocation_id: nonce()?,
                        action: ProviderAction::Dispose,
                        target_id: launch.view().target_id.clone(),
                        baseline_label: launch.baseline_label().into(),
                        launch_fingerprint: launch.view().fingerprint.clone(),
                        prior_resource: resource.clone(),
                    };
                    let saved = projector.publish_provider(resource.clone(), invocation.clone())?;
                    let receipt = owners.provider.as_mut().unwrap().invoke(saved)?;
                    validate_disposition(launch, &resource, &invocation, &receipt)?;
                    projector.record_provider_receipt(&invocation, receipt)?;
                    remote_complete = true;
                    Ok(())
                }));
                match dispose {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => owners.errors.push(error),
                    Err(payload) => owners.panics.push(payload),
                }
            }
        }
        let joined = settle_owned(&mut owners);
        if !joined {
            local = Disposition::Unknown {
                reason: "new recovery owners retained".into(),
            };
        }
        let remote = if remote_complete {
            Disposition::Complete {
                receipt: "matching original provider disposition receipt validated".into(),
            }
        } else {
            Disposition::Unknown {
                reason: "original provider disposition remains unknown".into(),
            }
        };
        all_remote &= remote_complete;
        owners.remote_unknown = !remote_complete;
        owners.pending_cleanup = !joined || owners.remote_unknown;
        if let Err(error) =
            projector.finish_recovery_attempt(&recovery_id, local.clone(), remote.clone())
        {
            owners.errors.push(error)
        }
        if integration
            && let Err(error) = super::integration_recovery::finish_attempt(
                &mut projector,
                &recovery_id,
                local.clone(),
                remote.clone(),
            )
        {
            owners.errors.push(error);
        }
        let scenario = scenarios_mut(&mut projector)
            .iter_mut()
            .find(|scenario| scenario.ownership_id == id)
            .unwrap();
        scenario.dispositions.remote = remote.clone();
        if remote_complete {
            scenario.dispositions.reset = remote;
        }
        if (!matches!(scenario.dispositions.local, Disposition::Complete { .. })
            || !remote_complete
            || !owners.errors.is_empty())
            && (!integration || scenario.outcome != RunOutcome::Failed)
        {
            scenario.outcome = RunOutcome::Incomplete;
        }
        if let Err(error) = projector.save_report() {
            owners.errors.push(error)
        }
        if owners.pending_cleanup || !owners.errors.is_empty() || !owners.panics.is_empty() {
            break;
        }
        owners.provider = None;
        owners.session = None;
        owners.start = None;
    }
    let has_failures = !owners.errors.is_empty() || !owners.panics.is_empty();
    let original_integration_outcome = outcome::integration_journal_outcome(projector.journal());
    outcome::aggregate_report(
        projector.report_mut(),
        original_integration_outcome,
        has_failures,
    );
    if let Err(error) = projector.save_report() {
        owners.errors.push(error)
    }
    event(&mut owners, &mut emit, EventKind::Finished, &path, None);
    let (stored_report, stored_journal) = projector.into_parts();
    if !owners.errors.is_empty() || !owners.panics.is_empty() || owners.pending_cleanup {
        owners.evidence = Some((store, stored_report, stored_journal));
        owners.intents = intents;
        return Err(retained_failure(
            format!("native recovery incomplete; evidence at {}", path.display()),
            owners,
        ));
    }
    Ok(RecoveredNativeCore {
        report: stored_report,
        journal: stored_journal,
        report_path: path,
        prior_local,
        local,
        remote: if all_remote {
            Disposition::Complete {
                receipt: "all original provider dispositions validated".into(),
            }
        } else {
            Disposition::Unknown {
                reason: "provider disposition remains unknown".into(),
            }
        },
    })
}

#[cfg(test)]
mod terminal_output_tests {
    use super::*;
    #[test]
    fn recovered_terminal_output_cannot_change_byte_count_or_truncation_or_fill_missing_output() {
        let mut known = NativeCommandEvidence {
            command: CatalogCommand {
                id: "check".into(),
                program: CatalogProgram::CheckTarget,
                expected_status: NativeStatus::PosixExit { code: 0 },
                stdout_literal: Some("é".into()),
            },
            operation_id: "01".repeat(16),
            request_digest: "02".repeat(32),
            status: Some(NativeStatus::PosixExit { code: 0 }),
            stdout: "é".into(),
            stdout_complete: true,
            stdout_utf8: true,
            stderr_complete: true,
            stdout_log: None,
            stderr_log: None,
            failure: None,
        };
        let terminal = |stdout_bytes, truncated| v2::Outcome::RunCompleted {
            reason: v2::RunReason::Natural,
            status: v2::NativeStatus::PosixExit { code: 0 },
            cleanup: v2::Cleanup::PosixLeaderReapedGroupTerminationRequested,
            stdout_bytes,
            stderr_bytes: 0,
            truncated,
        };
        assert!(validate_known_output(&known, &terminal(2, false)).is_ok());
        assert!(validate_known_output(&known, &terminal(1, false)).is_err());
        assert!(validate_known_output(&known, &terminal(2, true)).is_err());
        known.stdout.clear();
        known.stdout_complete = false;
        known.stderr_complete = false;
        assert!(validate_known_output(&known, &terminal(2, true)).is_ok());
        assert!(known.stdout.is_empty());
        assert!(!known.stdout_complete);
        assert!(!known.stderr_complete);
    }
}
