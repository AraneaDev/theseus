//! Per-owner recovery attempt steps: publication, query/reconnect, disposal and closing.
use super::{
    Admission, current, journal, nonce, recover_session, scenarios_mut, validate_disposition,
};
use crate::{
    engine::{CancellationToken, projector::RunProjector, selected::Owners},
    model::RunOutcome,
    native::{
        launch::LaunchAuthority,
        model::*,
        provider::{NativeProvider, validate_baseline},
        transport::NativeSession,
    },
};
use anyhow::{Result, bail, ensure};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Shared, read-only inputs of one owner's recovery attempt.
#[derive(Clone, Copy)]
pub(super) struct AttemptCtx<'a> {
    pub(super) launch: &'a LaunchAuthority,
    pub(super) id: &'a str,
    pub(super) cancel: &'a CancellationToken,
    pub(super) integration: bool,
}

/// Flags written during the attempt. Held behind `&mut` so writes made before an
/// error or panic remain visible to the caller.
#[derive(Default)]
pub(super) struct AttemptFlags {
    pub(super) remote_complete: bool,
    pub(super) provider_confirmed: bool,
}

/// Publishes the recovery attempt and the reconstructed prior local disposition.
pub(super) fn begin_attempt(
    projector: &mut RunProjector<'_>,
    prior_local: &mut Vec<Disposition>,
    id: &str,
    integration: bool,
) -> Result<String> {
    let recovery_id = nonce()?;
    let scenario = scenarios_mut(projector)
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
        ownership_id: id.into(),
        prior_local: prior,
        local: Disposition::Unknown {
            reason: "recovery attempt published; new local settlement unproved".into(),
        },
        remote: Disposition::Unknown {
            reason: "original remote settlement not queried".into(),
        },
    })?;
    if integration {
        crate::engine::integration_recovery::publish_attempt(projector, &recovery_id, id)?;
    }
    Ok(recovery_id)
}

/// Queries the original provider disposition or preparation and, when an
/// original intent is retained, reconnects a recovery session to it.
pub(super) fn query_or_reconnect(
    projector: &mut RunProjector<'_>,
    owners: &mut Owners,
    admission: &mut Admission,
    flags: &mut AttemptFlags,
    ctx: AttemptCtx<'_>,
) -> Result<()> {
    let AttemptCtx {
        launch,
        id,
        cancel,
        integration,
    } = ctx;
    let calls = journal(projector)
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
            validate_disposition(launch, &current(projector, id), &call.invocation, receipt)?;
            flags.remote_complete = true;
            return Ok(());
        }
        owners.provider = Some(launch.provider_factory()?);
        let receipt = owners
            .provider
            .as_mut()
            .unwrap()
            .query(&call.invocation)?
            .ok_or_else(|| {
                anyhow::anyhow!("original provider disposition remains unknown; never replayed")
            })?;
        validate_disposition(launch, &current(projector, id), &call.invocation, &receipt)?;
        projector.record_provider_receipt(&call.invocation, receipt)?;
        flags.remote_complete = true;
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
    flags.provider_confirmed = true;
    if let Some(mut active) = admission.launch.take() {
        if integration {
            let bound = launch.with_baseline(&baseline)?;
            ensure!(
                bound.binding().binding == active.binding().binding
                    && bound.binding().manifest_digest == active.binding().manifest_digest
                    && bound.binding().workspace_marker == active.binding().workspace_marker
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
            projector,
            owners.session.as_mut().unwrap(),
            &admission.owner,
            admission.original.as_ref().unwrap(),
            cancel,
        )?;
    }
    Ok(())
}

/// Disposes a confirmed original preparation once new owners have settled and
/// no reset/dispose was ever published for it.
pub(super) fn dispose_original(
    projector: &mut RunProjector<'_>,
    owners: &mut Owners,
    flags: &mut AttemptFlags,
    ctx: AttemptCtx<'_>,
    settled: bool,
) {
    let AttemptCtx { launch, id, .. } = ctx;
    if settled && !flags.remote_complete && owners.provider.is_some() {
        let no_pending_disposition = !journal(projector).provider_calls.iter().any(|call| {
            call.invocation.ownership_id == id
                && matches!(
                    call.invocation.action,
                    ProviderAction::Reset | ProviderAction::Dispose
                )
        });
        let known_prepare = journal(projector).provider_calls.iter().any(|call| {
            call.invocation.ownership_id == id
                && call.invocation.action == ProviderAction::Prepare
                && call.receipt.is_some()
        });
        if no_pending_disposition && known_prepare && flags.provider_confirmed {
            let dispose = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
                let resource = current(projector, id);
                let invocation = ProviderInvocation {
                    ownership_id: id.into(),
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
                flags.remote_complete = true;
                Ok(())
            }));
            match dispose {
                Ok(Ok(())) => {}
                Ok(Err(error)) => owners.errors.push(error),
                Err(payload) => owners.panics.push(payload),
            }
        }
    }
}

/// Settles owners again, then closes the attempt in the journal and report.
pub(super) fn finish_attempt(
    projector: &mut RunProjector<'_>,
    owners: &mut Owners,
    local: &mut Disposition,
    ctx: AttemptCtx<'_>,
    recovery_id: &str,
    remote_complete: bool,
) {
    let AttemptCtx {
        id, integration, ..
    } = ctx;
    let joined = crate::engine::selected::settle_owned(owners);
    if !joined {
        *local = Disposition::Unknown {
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
    owners.remote_unknown = !remote_complete;
    owners.pending_cleanup = !joined || owners.remote_unknown;
    if let Err(error) =
        projector.finish_recovery_attempt(recovery_id, local.clone(), remote.clone())
    {
        owners.errors.push(error)
    }
    if integration
        && let Err(error) = crate::engine::integration_recovery::finish_attempt(
            projector,
            recovery_id,
            local.clone(),
            remote.clone(),
        )
    {
        owners.errors.push(error);
    }
    let scenario = scenarios_mut(projector)
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
}
