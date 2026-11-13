//! Phase helpers for `run_native`. Every unwind boundary stays at its call
//! site in `run_native`; these helpers are the bodies those boundaries guard.
use super::{Owners, invocation, native_ownership, native_provider_calls, native_report, nonce};
use crate::{
    engine::{CancellationToken, domain::*, projector::RunProjector},
    model::{RunOutcome, ScenarioKind, StageOutcome},
    native::{
        adapter::NativeDriver,
        integration::{IntegrationScenarioEvidence, ValidatedIntegrationRecipe},
        launch::LaunchAuthority,
        model::*,
        provider::NativeProvider,
        recipe::{NativeRunLimits, ValidatedNativeRecipe},
        transport::{NativeSession, validate_preparation},
    },
};
use anyhow::{Result, ensure};
use theseus_native_client::ClientMode;
use theseus_native_helper_store::{IntentRecord, IntentStore};
use theseus_native_protocol::v2;

/// Immutable run-wide authority shared by the per-scenario phases.
pub(super) struct RunCtx<'a> {
    pub(super) recipe: &'a ValidatedNativeRecipe,
    pub(super) integration: Option<&'a ValidatedIntegrationRecipe>,
    pub(super) launch: &'a LaunchAuthority,
}

/// Per-scenario state written during setup before any possible bail. It lives
/// outside the setup unwind boundary so every write survives an `Err` or panic.
pub(super) struct SetupFlags<'e> {
    pub(super) resource: NativeResource,
    pub(super) provider_started: bool,
    pub(super) provider_prepared: bool,
    pub(super) desktop_header: Option<v2::broker::IntegrationBrokerHeader>,
    pub(super) preparation_invocation: Option<ProviderInvocation>,
    pub(super) epochs: &'e mut std::collections::BTreeSet<String>,
}

/// Pure ceiling validation; precedes even creating an evidence directory.
pub(super) fn validate_ceiling(
    recipe: &ValidatedNativeRecipe,
    integration: Option<&ValidatedIntegrationRecipe>,
    options: &NativeRunOptions,
    launch: &LaunchAuthority,
) -> Result<()> {
    let os = match recipe.recipe.os {
        crate::native::recipe::NativeOs::Linux => v2::Os::Linux,
        crate::native::recipe::NativeOs::Macos => v2::Os::Macos,
        crate::native::recipe::NativeOs::Windows => v2::Os::Windows,
    };
    if let Some(integration) = integration {
        integration.validate_selection(&options.scenarios, &options.limits)?;
        crate::native::integration::view::validate_authority(integration, launch)?;
        let (core, integration_m) = launch
            .integration_components()
            .ok_or_else(|| anyhow::anyhow!("integration helper profile required"))?;
        ensure!(
            os == launch.binding().os
                && recipe.recipe.manifest_digest == core.to_hex()
                && integration.integration_manifest_digest == integration_m
                && integration.full_manifest_digest()? == launch.binding().manifest_digest.to_hex(),
            "integration recipe exceeds immutable launch ceiling"
        );
    } else {
        ensure!(
            os == launch.binding().os
                && launch.integration_components().is_none()
                && recipe.recipe.manifest_digest == launch.binding().manifest_digest.to_hex(),
            "native recipe exceeds immutable launch ceiling"
        );
    }
    Ok(())
}

/// Initial (empty, incomplete) report and journal envelopes for a run.
pub(super) fn create_envelopes(
    id: String,
    recipe: &ValidatedNativeRecipe,
    integration: Option<&ValidatedIntegrationRecipe>,
    options: &NativeRunOptions,
) -> Result<(StoredRun, StoredJournal)> {
    let report = NativeReport {
        kind: "theseus_native_report".into(),
        schema: 1,
        run_id: id.clone(),
        recipe_digest: recipe.digest.clone(),
        scenarios: vec![],
        identities: vec![],
        provider_claims: vec![],
        failures: vec![],
        outcome: RunOutcome::Incomplete,
    };
    Ok(if let Some(integration) = integration {
        (
            StoredRun::NativeIntegration(crate::native::integration::NativeIntegrationReport::new(
                id.clone(),
                integration,
                options.scenarios.clone(),
            )?),
            StoredJournal::NativeIntegration(
                crate::native::integration::NativeIntegrationJournal::new(
                    id.clone(),
                    integration,
                    options.scenarios.clone(),
                )?,
            ),
        )
    } else {
        (
            StoredRun::Native(report),
            StoredJournal::Native(NativeJournal::new(id)),
        )
    })
}

/// Provider preparation, session startup/admission and original Acquire.
/// Runs inside the caller's setup unwind boundary.
pub(super) fn prepare_and_acquire(
    ctx: &RunCtx<'_>,
    flags: &mut SetupFlags<'_>,
    owners: &mut Owners,
    projector: &mut RunProjector<'_>,
    intent_store: &mut IntentStore,
    ownership_id: &str,
    kind: ScenarioKind,
) -> Result<crate::engine::projector::PublishedResource> {
    let (recipe, integration, launch) = (ctx.recipe, ctx.integration, ctx.launch);
    ensure!(
        owners.panics.is_empty(),
        "native event observer panicked before effects"
    );
    owners.preparation = Some(launch.prepare());
    if owners.preparation.as_ref().unwrap().error().is_some() {
        anyhow::bail!("native transport preparation failed; original preparation retained")
    }
    let mut prepare = invocation(launch, &flags.resource, ProviderAction::Prepare)?;
    if let NativeOwnership::Preparing { invocation_id, .. } = &flags.resource.ownership {
        prepare.invocation_id = invocation_id.clone();
    }
    let saved = projector.publish_provider(flags.resource.clone(), prepare.clone())?;
    flags.preparation_invocation = Some(prepare.clone());
    flags.provider_started = true;
    owners.remote_unknown = true;
    let receipt = owners.provider.as_mut().unwrap().invoke(saved)?;
    let baseline = owners
        .provider
        .as_ref()
        .unwrap()
        .validate_original_baseline(launch, &prepare, receipt.clone())?;
    ensure!(
        flags.epochs.insert(receipt.reset_epoch.clone()),
        "provider reused a baseline epoch"
    );
    projector.record_provider_receipt(&prepare, receipt.clone())?;
    native_report(projector)
        .provider_claims
        .push(receipt.clone());
    let active_launch = launch.with_baseline(&baseline)?;
    flags.desktop_header = active_launch
        .integration_context()
        .and_then(|c| c.desktop_broker_header.clone());
    if let Some(integration) = integration {
        crate::native::transport::validate_integration_preparation(
            &active_launch,
            &baseline,
            integration,
        )?;
    } else {
        validate_preparation(&active_launch, &baseline, recipe)?;
    }
    flags.provider_prepared = true;
    match NativeSession::start_prepared(
        &active_launch,
        ClientMode::Fresh,
        owners.preparation.take().unwrap(),
    ) {
        Ok(session) => owners.session = Some(session),
        Err(failure) => {
            owners.start = Some(failure);
            anyhow::bail!("native client startup failed; original startup owner retained")
        }
    }
    owners.session.as_mut().unwrap().wait_ready()?;
    owners
        .session
        .as_ref()
        .unwrap()
        .validate_artifacts(recipe)?;
    if integration.is_some() {
        owners
            .session
            .as_ref()
            .unwrap()
            .validate_integration_context(&active_launch, ownership_id)?;
        if let Some(integration) = integration
            && integration.recipe.integrations.desktop.is_some()
        {
            if owners
                .session
                .as_ref()
                .unwrap()
                .integration_description()
                .is_some_and(|d| {
                    matches!(
                        d.manifest.desktop_native,
                        Some(theseus_native_protocol::v2::NativeDesktopDefinition::LinuxGio { .. })
                    )
                })
            {
                owners
                    .session
                    .as_mut()
                    .unwrap()
                    .admit_linux_gio_qualification(&active_launch, ownership_id)?;
            }
            let manifest = &owners
                .session
                .as_ref()
                .unwrap()
                .integration_description()
                .ok_or_else(|| anyhow::anyhow!("desktop description missing before Acquire"))?
                .manifest;
            let header = flags
                .desktop_header
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("desktop header missing before Acquire"))?;
            crate::native::integration::desktop::validate_admission(
                integration,
                manifest,
                header,
                kind,
            )?;
            owners
                .session
                .as_ref()
                .unwrap()
                .require_linux_gio_admission(manifest, header)?;
        }
    }
    let prepared = intent_store.prepare(IntentRecord {
        intent: v2::IntentId::parse(&nonce()?)?,
        binding: active_launch.binding().binding,
        manifest_digest: active_launch.binding().manifest_digest,
        workspace_marker: active_launch.binding().workspace_marker,
    })?;
    let scope = match prepared.commit_scope() {
        theseus_host::fs::CommitScope::FileAndDirectorySynced => "file_and_directory_synced",
        theseus_host::fs::CommitScope::FileFlushedAtomicReplace => "file_flushed_atomic_replace",
    };
    let identity = NativeIntent {
        intent: prepared.record().intent.to_hex(),
        binding: receipt.binding.clone(),
        manifest_digest: receipt.manifest_digest.clone(),
        workspace_marker: receipt.workspace_marker.clone(),
        reset_epoch: receipt.reset_epoch.clone(),
        publication_scope: scope.into(),
    };
    flags.resource.ownership = NativeOwnership::Intent { prepared: identity };
    let saved = projector.publish_intent(flags.resource.clone(), prepared)?;
    let acquired = owners.session.as_mut().unwrap().acquire(saved, &baseline)?;
    flags.resource.ownership = NativeOwnership::Acquired {
        prepared: acquired.prepared,
        lease: acquired.lease.to_hex(),
    };
    projector.publish_acquired(flags.resource.clone())
}

/// Scenario body selection. Runs inside the caller's driver unwind boundary.
pub(super) fn drive_scenario(
    driver: &mut NativeDriver<'_, '_>,
    integration: Option<&ValidatedIntegrationRecipe>,
    limits: &NativeRunLimits,
    desktop_header: &mut Option<v2::broker::IntegrationBrokerHeader>,
    kind: ScenarioKind,
) -> Result<()> {
    driver.set_limits(limits)?;
    if let Some(integration) = integration {
        if integration.recipe.integrations.desktop.is_some() {
            let header = desktop_header
                .take()
                .ok_or_else(|| anyhow::anyhow!("original desktop provider context missing"))?;
            if integration.recipe.integrations.service.is_some() {
                crate::native::integration::driver::IntegrationDriver::combined(
                    driver,
                    integration,
                    header,
                    kind,
                )?
                .run(kind)
            } else {
                crate::native::integration::desktop::DesktopDriver::new_for_scenario(
                    driver,
                    integration,
                    header,
                    kind,
                )?
                .run(kind)
            }
        } else {
            crate::native::integration::driver::IntegrationDriver::new(driver, integration)
                .run(kind)
        }
    } else {
        driver.run(kind)
    }
}

/// Publish the original release, then submit it. Runs inside the caller's
/// release unwind boundary.
pub(super) fn release_original(driver: &mut NativeDriver<'_, '_>) -> Result<v2::LeaseView> {
    let release = driver.projector.publish_release(&driver.resource)?;
    native_report(driver.projector)
        .scenarios
        .last_mut()
        .unwrap()
        .dispositions
        .release = Disposition::Unknown {
        reason: "release published; settlement unproved".into(),
    };
    driver.projector.save_report()?;
    driver.session.release(release)
}

/// Record the outcome of the guarded original release.
pub(super) fn record_release(
    driver: &mut NativeDriver<'_, '_>,
    owners: &mut Owners,
    resource: &NativeResource,
    ownership_id: &str,
    released: std::thread::Result<Result<v2::LeaseView>>,
) {
    match released {
        Ok(Ok(v2::LeaseView::AwaitingExternalReset { .. })) => {
            let NativeOwnership::Acquired { lease, .. } = &resource.ownership else {
                unreachable!()
            };
            let disposition = Disposition::Complete {
                receipt: "original lease awaiting external reset".into(),
            };
            if let Err(e) =
                driver
                    .projector
                    .record_release_completion(ownership_id, lease, disposition.clone())
            {
                owners.errors.push(e)
            }
            native_report(driver.projector)
                .scenarios
                .last_mut()
                .unwrap()
                .dispositions
                .release = disposition;
            if let StoredRun::NativeIntegration(report) = driver.projector.report_mut()
                && report.requirements.desktop.is_some()
                && let Some(evidence) = report.integrations.last_mut()
            {
                evidence.dispositions.broker = Disposition::Complete {
                    receipt: "original broker lease released before original helper lease closed"
                        .into(),
                };
            }
        }
        Ok(Ok(_)) => owners
            .errors
            .push(anyhow::anyhow!("native release has not settled")),
        Ok(Err(e)) => owners.errors.push(e),
        Err(payload) => owners.panics.push(payload),
    }
}

/// Provider disposition of the original owner identity. Runs inside the
/// caller's reset unwind boundary.
pub(super) fn dispose_provider(
    launch: &LaunchAuthority,
    flags: &mut SetupFlags<'_>,
    owners: &mut Owners,
    projector: &mut RunProjector<'_>,
    original: &NativeResource,
) -> Result<ProviderReceipt> {
    if !flags.provider_prepared {
        let prepare = flags
            .preparation_invocation
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("original provider preparation identity missing"))?;
        let receipt = owners.provider.as_mut().unwrap().query(prepare)?.ok_or_else(|| anyhow::anyhow!("original provider preparation remains unknown; no new disposition identity authorized"))?;
        v2::Digest::parse(&receipt.reset_epoch)?;
        owners
            .provider
            .as_ref()
            .unwrap()
            .validate_original_baseline(launch, prepare, receipt.clone())?;
        let known = native_provider_calls(projector)
            .iter()
            .find(|call| &call.invocation == prepare)
            .and_then(|call| call.receipt.clone());
        if let Some(known) = known {
            ensure!(
                known == receipt,
                "provider query changed original preparation receipt"
            );
            projector.save_journal()?;
        } else {
            projector.record_provider_receipt(prepare, receipt)?;
        }
    }
    let original = native_ownership(projector)
        .iter()
        .find(|owner| owner.ownership_id == original.ownership_id)
        .cloned()
        .expect("original preparing owner retained");
    let invocation = invocation(launch, &original, ProviderAction::Dispose)?;
    let saved = projector.publish_provider(original, invocation.clone())?;
    native_report(projector)
        .scenarios
        .last_mut()
        .unwrap()
        .dispositions
        .reset = Disposition::Unknown {
        reason: "provider disposition published; completion unproved".into(),
    };
    projector.save_report()?;
    let receipt = owners.provider.as_mut().unwrap().invoke(saved)?;
    ensure!(
        receipt.invocation_digest == invocation.digest()?,
        "provider disposition receipt mismatch"
    );
    v2::Digest::parse(&receipt.reset_epoch)?;
    ensure!(
        flags.epochs.insert(receipt.reset_epoch.clone()),
        "provider disposition did not advance epoch"
    );
    ensure!(
        receipt.manifest_digest == launch.binding().manifest_digest.to_hex(),
        "provider disposition exceeds catalog ceiling"
    );
    projector.record_provider_receipt(&invocation, receipt.clone())?;
    Ok(receipt)
}

/// Pure integration outcome derived from a scenario's integration evidence.
fn integration_outcome(evidence: &IntegrationScenarioEvidence) -> RunOutcome {
    let sut_failed = evidence
        .checks
        .iter()
        .any(|c| c.outcome == crate::model::CheckOutcome::Failed);
    if sut_failed {
        RunOutcome::Failed
    } else if evidence
        .checks
        .iter()
        .any(|c| c.outcome != crate::model::CheckOutcome::Passed)
        || !evidence.complete()
    {
        RunOutcome::Incomplete
    } else {
        RunOutcome::Passed
    }
}

/// Settle local owners and project the scenario's final dispositions and
/// outcomes into the report (in memory; the caller saves). Returns whether all
/// original local owners joined.
pub(super) fn finalize_scenario(
    #[cfg_attr(
        not(feature = "native-integration-test-support"),
        allow(unused_variables)
    )]
    integration: Option<&ValidatedIntegrationRecipe>,
    owners: &mut Owners,
    projector: &mut RunProjector<'_>,
    cancel: &CancellationToken,
    provider_started: bool,
    reset_complete: bool,
) -> bool {
    let all_local = super::settle_owned(owners);
    owners.remote_unknown = provider_started && !reset_complete;
    owners.pending_cleanup = owners.remote_unknown || !all_local;
    let report = native_report(projector);
    let scenario = report.scenarios.last_mut().unwrap();
    scenario.dispositions.local = if all_local {
        Disposition::Complete {
            receipt: "all original local process and I/O owners joined".into(),
        }
    } else {
        Disposition::Unknown {
            reason: "original local owners retained".into(),
        }
    };
    scenario.dispositions.remote = if !provider_started {
        Disposition::NotStarted
    } else if reset_complete {
        Disposition::Complete {
            receipt: "provider disposed original owner".into(),
        }
    } else {
        Disposition::Unknown {
            reason: "provider disposition unproved".into(),
        }
    };
    scenario.outcome = if cancel.is_cancelled() {
        RunOutcome::Cancelled
    } else if !owners.errors.is_empty()
        || !owners.panics.is_empty()
        || !all_local
        || !reset_complete
    {
        RunOutcome::Incomplete
    } else {
        RunOutcome::aggregate(scenario.stages.iter().map(|s| match s.outcome {
            StageOutcome::Passed => RunOutcome::Passed,
            StageOutcome::Failed => RunOutcome::Failed,
            _ => RunOutcome::Incomplete,
        }))
    };
    for error in &owners.errors {
        #[cfg(feature = "native-integration-test-support")]
        if integration.is_some() {
            // Fixture controllers retain stderr privately. Keep the actual
            // source-test cause visible without changing shipping reports.
            use std::io::Write as _;
            let _ = writeln!(
                std::io::stderr().lock(),
                "source integration failure: {error:#}"
            );
        }
        #[cfg(not(feature = "native-integration-test-support"))]
        let _ = error;
        report.failures.push(
            "native infrastructure or publication failed; original typed failure retained".into(),
        );
    }
    if !owners.panics.is_empty() {
        report
            .failures
            .push("native execution panicked; original payload retained".into());
    }
    *report.outcome = RunOutcome::aggregate(report.scenarios.iter().map(|s| s.outcome));
    if let StoredRun::NativeIntegration(report) = projector.report_mut() {
        let evidence = report.integrations.last_mut().unwrap();
        evidence.dispositions.helper_worker = if all_local {
            Disposition::Complete {
                receipt: "original helper worker and transport joined".into(),
            }
        } else {
            Disposition::Unknown {
                reason: "helper worker original owners retained".into(),
            }
        };
        let integration_outcome = integration_outcome(evidence);
        report.outcome = RunOutcome::aggregate([report.catalog.outcome, integration_outcome]);
    }
    all_local
}
