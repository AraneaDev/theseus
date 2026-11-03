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
        recovery::recovery_mode,
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
use theseus_native_client::{Action, ClientMode, Event, Poll, RequestId};
use theseus_native_helper_store::{IntentRecord, VerifiedOriginalIntent};
use theseus_native_protocol::v2;

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
fn nonce() -> Result<String> {
    let mut bytes = [0; 16];
    theseus_host::random_bytes(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn journal<'a>(projector: &'a RunProjector<'_>) -> &'a NativeJournal {
    match projector.journal() {
        StoredJournal::Native(value) => value,
        _ => unreachable!(),
    }
}
fn report<'a>(projector: &'a mut RunProjector<'_>) -> &'a mut NativeReport {
    match projector.report_mut() {
        StoredRun::Native(value) => value,
        _ => unreachable!(),
    }
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
                } => {
                    std::thread::yield_now();
                    continue;
                }
                _ => bail!("original operation query refused"),
            };
            let completion = match state {
                v2::OperationView::Finished { outcome } => {
                    let known = report(projector)
                        .scenarios
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
                    Some(Disposition::Failed {
                        reason: "original operation not found; never replayed".into(),
                    })
                }
                v2::OperationView::Unknown { .. } => {
                    bail!("original operation disposition unknown; never replayed")
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
                    } => {}
                    _ => bail!("original operation cancellation refused"),
                }
            }
            ensure!(
                !cancel.is_cancelled(),
                "recovery cancelled while original operation unsettled"
            );
        }
    }
    Ok(())
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
    operations(projector, session, &owner.ownership_id, lease, cancel)?;
    let state = if matches!(state, v2::LeaseView::Held { .. }) {
        match request(session, Action::QueryLease(lease))? {
            v2::Reply::Lease { lease: l, state } if l == lease => state,
            _ => bail!("original lease query refused"),
        }
    } else {
        state
    };
    let state = if matches!(state, v2::LeaseView::Held { .. }) {
        let saved = projector.publish_recovery_release(&owner.ownership_id)?;
        report(projector)
            .scenarios
            .iter_mut()
            .find(|scenario| scenario.ownership_id == owner.ownership_id)
            .expect("admitted scenario")
            .dispositions
            .release = Disposition::Unknown {
            reason: "original recovery release published; settlement unproved".into(),
        };
        projector.save_report()?;
        session.release(saved)?
    } else {
        state
    };
    ensure!(
        matches!(state, v2::LeaseView::AwaitingExternalReset { .. }),
        "original lease remains quarantined or held"
    );
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
    report(projector)
        .scenarios
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
    mut emit: impl FnMut(EngineEvent),
) -> Result<NativeRecoveryResult> {
    let saved_report = store.load_native_report()?;
    let saved_journal = store.load_native_journal()?;
    ensure!(
        saved_report.run_id == saved_journal.run_id,
        "recovery report/journal identity mismatch"
    );
    ensure!(
        saved_journal.launch_fingerprint.as_deref() == Some(launch.view().fingerprint.as_str()),
        "recovery requires unchanged explicit launch fingerprint"
    );
    ensure!(
        !saved_journal.ownership.is_empty(),
        "native recovery requires original ownership"
    );
    let intents = if saved_journal
        .ownership
        .iter()
        .any(|owner| !matches!(owner.ownership, NativeOwnership::Preparing { .. }))
    {
        Some(store.open_intent_store()?)
    } else {
        None
    };
    let mut admissions = vec![];
    for owner in &saved_journal.ownership {
        ensure!(
            saved_report
                .scenarios
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
            let active = launch
                .for_recovery(prepared, saved_journal.launch_fingerprint.as_ref().unwrap())?;
            let intents = intents
                .as_ref()
                .expect("retained original intent namespace");
            admission.mode = Some(recovery_mode(
                &saved_journal,
                &owner.ownership_id,
                &active,
                intents,
            )?);
            admission.original = Some(intents.verify_original(&IntentRecord {
                intent: v2::IntentId::parse(&prepared.intent)?,
                binding: active.binding().binding,
                manifest_digest: active.binding().manifest_digest,
                workspace_marker: active.binding().workspace_marker,
            })?);
            admission.launch = Some(active);
        }
        for call in saved_journal
            .provider_calls
            .iter()
            .filter(|call| call.invocation.ownership_id == owner.ownership_id)
        {
            validate_context(launch, &call.invocation)?;
        }
        admissions.push(admission);
    }
    let path = store.native_report_path();
    let mut projector = RunProjector::new(
        &store,
        StoredRun::Native(saved_report),
        StoredJournal::Native(saved_journal),
    )?;
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
        let scenario = report(&mut projector)
            .scenarios
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
            validate_baseline(launch, &prepare.invocation, receipt.clone())?;
            if let Some(saved) = &prepare.receipt {
                ensure!(saved == &receipt, "provider query changed original receipt");
            } else {
                projector.record_provider_receipt(&prepare.invocation, receipt)?;
            }
            provider_confirmed = true;
            if let Some(active) = admission.launch.take() {
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
        let scenario = report(&mut projector)
            .scenarios
            .iter_mut()
            .find(|scenario| scenario.ownership_id == id)
            .unwrap();
        scenario.dispositions.remote = remote.clone();
        if remote_complete {
            scenario.dispositions.reset = remote;
        }
        if !matches!(scenario.dispositions.local, Disposition::Complete { .. })
            || !remote_complete
            || !owners.errors.is_empty()
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
    let value = report(&mut projector);
    value.outcome = RunOutcome::aggregate(value.scenarios.iter().map(|scenario| scenario.outcome));
    if !owners.errors.is_empty() || !owners.panics.is_empty() {
        value
            .failures
            .push("native recovery incomplete; original typed errors and owners retained".into());
    }
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
    let StoredRun::Native(report) = stored_report else {
        unreachable!()
    };
    let StoredJournal::Native(journal) = stored_journal else {
        unreachable!()
    };
    Ok(NativeRecoveryResult {
        report,
        journal,
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
