//! Closed backend dispatch and native publication-before-effect orchestration.
use super::{CancellationToken, EngineEvent, EventKind, domain::*, projector::RunProjector};
use crate::{
    model::{RunOutcome, StageOutcome},
    native::{
        adapter::NativeDriver,
        launch::{LaunchAuthority, LaunchPreparation},
        model::*,
        provider::{CommandProvider, NativeProvider},
        transport::{NativeSession, NativeStartFailure, validate_preparation},
    },
    report::{self, ReportStore},
};
use anyhow::{Result, ensure};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};
use theseus_native_client::ClientMode;
use theseus_native_helper_store::IntentRecord;
use theseus_native_protocol::v2;

pub fn run_selected(
    recipe: &SelectedRecipe,
    options: SelectedRunOptions,
    launch: Option<&LaunchAuthority>,
    cancel: CancellationToken,
    emit: impl FnMut(EngineEvent),
) -> Result<SelectedRunResult> {
    super::run_selected_with_parent(recipe, options, launch, cancel, None, emit)
}
pub(crate) fn run_selected_with_parent(
    recipe: &SelectedRecipe,
    options: SelectedRunOptions,
    launch: Option<&LaunchAuthority>,
    cancel: CancellationToken,
    parent: Option<&ReportStore>,
    emit: impl FnMut(EngineEvent),
) -> Result<SelectedRunResult> {
    compile(recipe, &options)?;
    match (recipe, options) {
        (SelectedRecipe::Docker(recipe), SelectedRunOptions::Docker(options)) => {
            let result = super::run_with_parent(
                recipe,
                options,
                cancel,
                &mut crate::runner::Docker::default(),
                parent,
                emit,
            )?;
            Ok(SelectedRunResult {
                report: StoredRun::Docker(result.report),
                report_path: result.report_path,
            })
        }
        (SelectedRecipe::Native(recipe), SelectedRunOptions::Native(options)) => run_native(
            recipe,
            None,
            options,
            launch.ok_or_else(|| anyhow::anyhow!("native launch authority required"))?,
            cancel,
            parent,
            emit,
        ),
        (
            SelectedRecipe::NativeIntegration(recipe),
            SelectedRunOptions::NativeIntegration(options),
        ) => run_native(
            &recipe.core,
            Some(recipe),
            options,
            launch.ok_or_else(|| anyhow::anyhow!("integration launch authority required"))?,
            cancel,
            parent,
            emit,
        ),
        _ => unreachable!("backend selection compiled"),
    }
}
fn nonce() -> Result<String> {
    let mut bytes = [0; 16];
    theseus_host::random_bytes(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn invocation(
    launch: &LaunchAuthority,
    resource: &NativeResource,
    action: ProviderAction,
) -> Result<ProviderInvocation> {
    Ok(ProviderInvocation {
        ownership_id: resource.ownership_id.clone(),
        provider: launch.provider_name().into(),
        invocation_id: nonce()?,
        action,
        target_id: launch.view().target_id.clone(),
        baseline_label: launch.baseline_label().into(),
        launch_fingerprint: launch.view().fingerprint.clone(),
        prior_resource: resource.clone(),
    })
}
#[derive(Default)]
pub(super) struct Owners {
    pub(super) provider: Option<CommandProvider>,
    pub(super) session: Option<NativeSession>,
    pub(super) start: Option<NativeStartFailure>,
    pub(super) preparation: Option<LaunchPreparation>,
    pub(super) errors: Vec<anyhow::Error>,
    pub(super) panics: Vec<Box<dyn std::any::Any + Send>>,
    pub(super) pending_cleanup: bool,
    pub(super) remote_unknown: bool,
    pub(super) evidence: Option<(ReportStore, StoredRun, StoredJournal)>,
    pub(super) intents: Option<theseus_native_helper_store::IntentStore>,
    nested_cleanup: Vec<crate::runner::process::CleanupFailures>,
    publication_unknown: bool,
    captured_errors: usize,
}
impl Owners {
    fn capture_errors(&mut self) {
        let first = self.captured_errors;
        self.captured_errors = self.errors.len();
        let captures = self.errors[first..]
            .iter()
            .map(|error| {
                catch_unwind(AssertUnwindSafe(|| {
                    crate::runner::process::CleanupFailures::capture_on_worker(error)
                }))
            })
            .collect::<Vec<_>>();
        for capture in captures {
            match capture {
                Ok((cleanup, facts)) => {
                    self.publication_unknown |= facts.publication_unknown;
                    self.nested_cleanup.push(cleanup);
                }
                Err(payload) => {
                    self.pending_cleanup = true;
                    self.publication_unknown = true;
                    self.panics.push(payload);
                }
            }
        }
    }
    fn settle(&mut self) -> bool {
        self.settle_for(Duration::from_secs(30))
    }
    fn settle_for(&mut self, budget: Duration) -> bool {
        if let Some(s) = &mut self.session {
            s.request_stop();
        }
        if let Some(s) = &mut self.start {
            s.request_stop();
        }
        if let Some(s) = &mut self.preparation {
            s.request_stop();
        }
        let end = Instant::now() + budget;
        loop {
            let mut done = true;
            if let Some(s) = &mut self.session {
                match s.poll_settle() {
                    Ok(v) => done &= v && s.is_settled(),
                    Err(e) => {
                        self.errors.push(e.into());
                        return false;
                    }
                }
            }
            if let Some(s) = &mut self.start {
                match s.poll_settle() {
                    Ok(v) => done &= v && s.is_settled(),
                    Err(e) => {
                        self.errors.push(e);
                        return false;
                    }
                }
            }
            if let Some(s) = &mut self.preparation {
                match s.poll_settle() {
                    Ok(v) => done &= v && s.is_settled(),
                    Err(e) => {
                        self.errors.push(e);
                        return false;
                    }
                }
            }
            if let Some(s) = &mut self.provider {
                match s.poll_settle() {
                    Ok(v) => done &= v && s.settled(),
                    Err(e) => {
                        self.errors.push(e);
                        return false;
                    }
                }
            }
            if done {
                self.nested_cleanup = std::mem::take(&mut self.nested_cleanup)
                    .into_iter()
                    .map(|cleanup| cleanup.settled())
                    .collect();
                return !self.nested_cleanup.iter().any(|cleanup| cleanup.pending());
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::yield_now();
        }
    }
}
/// Original typed errors, panic payloads and partial native owners survive an unresolved join.
struct RetainedFailure {
    message: String,
    owners: Mutex<Owners>,
}
impl std::fmt::Debug for RetainedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetainedNativeFailure")
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for RetainedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for RetainedFailure {}
pub(super) fn retained_failure(message: String, mut owners: Owners) -> anyhow::Error {
    owners.capture_errors();
    anyhow::Error::new(RetainedFailure {
        message,
        owners: Mutex::new(owners),
    })
}
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NativeCleanupFacts {
    pub local_pending: bool,
    pub remote_unknown: bool,
    pub publication_unknown: bool,
}
impl NativeCleanupFacts {
    pub fn pending(self) -> bool {
        self.local_pending || self.remote_unknown || self.publication_unknown
    }
    fn unavailable() -> Self {
        Self {
            local_pending: true,
            remote_unknown: true,
            publication_unknown: true,
        }
    }
}
fn cleanup_facts(owners: &Owners) -> NativeCleanupFacts {
    NativeCleanupFacts {
        local_pending: owners
            .nested_cleanup
            .iter()
            .any(|cleanup| cleanup.pending())
            || owners
                .session
                .as_ref()
                .is_some_and(|owner| !owner.is_settled())
            || owners
                .start
                .as_ref()
                .is_some_and(|owner| !owner.is_settled())
            || owners
                .preparation
                .as_ref()
                .is_some_and(|owner| !owner.is_settled())
            || owners
                .provider
                .as_ref()
                .is_some_and(|owner| !owner.settled()),
        remote_unknown: owners.remote_unknown,
        publication_unknown: owners.publication_unknown,
    }
}
/// Read-only inspection: never traverses error sources or polls native owners.
pub(crate) fn native_cleanup_facts(error: &anyhow::Error) -> Option<NativeCleanupFacts> {
    let failure = error.downcast_ref::<RetainedFailure>()?;
    Some(match failure.owners.try_lock() {
        Ok(owners) => cleanup_facts(&owners),
        Err(_) => NativeCleanupFacts::unavailable(),
    })
}
pub(crate) fn native_cleanup_pending(error: &anyhow::Error) -> bool {
    native_cleanup_facts(error).is_some_and(NativeCleanupFacts::pending)
}
/// Owning finalizer workers only: advance the exact existing local owner bundle.
/// No launch, provider invocation/query, report publication or new identity occurs.
pub(crate) fn finalize_native_cleanup(error: &anyhow::Error) -> Option<NativeCleanupFacts> {
    let failure = error.downcast_ref::<RetainedFailure>()?;
    let Ok(mut owners) = failure.owners.lock() else {
        return Some(NativeCleanupFacts::unavailable());
    };
    owners.capture_errors();
    let joined = match catch_unwind(AssertUnwindSafe(|| {
        owners.settle_for(Duration::from_millis(250))
    })) {
        Ok(joined) => joined,
        Err(payload) => {
            owners.panics.push(payload);
            false
        }
    };
    owners.pending_cleanup = owners.remote_unknown || !joined;
    owners.capture_errors();
    Some(cleanup_facts(&owners))
}
#[cfg(feature = "native-catalog-test-support")]
#[doc(hidden)]
pub fn fixture_retained_native_session(
    session: NativeSession,
    remote_unknown: bool,
) -> anyhow::Error {
    retained_failure(
        "explicit inert native cleanup fixture".into(),
        Owners {
            session: Some(session),
            remote_unknown,
            pending_cleanup: true,
            ..Owners::default()
        },
    )
}

#[cfg(feature = "native-catalog-test-support")]
#[doc(hidden)]
pub fn fixture_native_cleanup_facts(error: &anyhow::Error) -> (bool, bool, bool) {
    let facts = native_cleanup_facts(error).expect("explicit retained native fixture");
    (
        facts.local_pending,
        facts.remote_unknown,
        facts.publication_unknown,
    )
}
#[cfg(feature = "native-catalog-test-support")]
#[doc(hidden)]
pub fn fixture_finalize_native_cleanup(error: &anyhow::Error) -> (bool, bool, bool) {
    let facts = finalize_native_cleanup(error).expect("explicit retained native fixture");
    (
        facts.local_pending,
        facts.remote_unknown,
        facts.publication_unknown,
    )
}

pub(super) fn event(
    owners: &mut Owners,
    emit: &mut impl FnMut(EngineEvent),
    kind: EventKind,
    path: &Path,
    id: Option<&str>,
) {
    let emitted = catch_unwind(AssertUnwindSafe(|| {
        emit(EngineEvent {
            kind,
            report_path: path.to_path_buf(),
            scenario_id: id.map(str::to_owned),
            stage: None,
        })
    }));
    if let Err(payload) = emitted {
        owners.panics.push(payload);
    }
}
pub(super) fn settle_owned(owners: &mut Owners) -> bool {
    owners.capture_errors();
    match catch_unwind(AssertUnwindSafe(|| owners.settle())) {
        Ok(settled) => settled,
        Err(payload) => {
            owners.panics.push(payload);
            false
        }
    }
}
struct CatalogReportMut<'a> {
    scenarios: &'a mut Vec<NativeScenario>,
    provider_claims: &'a mut Vec<ProviderReceipt>,
    failures: &'a mut Vec<String>,
    outcome: &'a mut RunOutcome,
}
fn native_report<'a>(projector: &'a mut RunProjector<'_>) -> CatalogReportMut<'a> {
    match projector.report_mut() {
        StoredRun::Native(r) => CatalogReportMut {
            scenarios: &mut r.scenarios,
            provider_claims: &mut r.provider_claims,
            failures: &mut r.failures,
            outcome: &mut r.outcome,
        },
        StoredRun::NativeIntegration(r) => CatalogReportMut {
            scenarios: &mut r.catalog.scenarios,
            provider_claims: &mut r.catalog.provider_claims,
            failures: &mut r.catalog.failures,
            outcome: &mut r.catalog.outcome,
        },
        _ => unreachable!(),
    }
}
fn native_ownership<'a>(projector: &'a RunProjector<'_>) -> &'a [NativeResource] {
    match projector.journal() {
        StoredJournal::Native(j) => &j.ownership,
        StoredJournal::NativeIntegration(j) => &j.ownership,
        _ => unreachable!(),
    }
}
fn native_provider_calls<'a>(projector: &'a RunProjector<'_>) -> &'a [NativeProviderCall] {
    match projector.journal() {
        StoredJournal::Native(j) => &j.provider_calls,
        StoredJournal::NativeIntegration(j) => &j.provider_calls,
        _ => unreachable!(),
    }
}
fn run_native(
    recipe: &crate::native::recipe::ValidatedNativeRecipe,
    integration: Option<&crate::native::integration::ValidatedIntegrationRecipe>,
    options: NativeRunOptions,
    launch: &LaunchAuthority,
    cancel: CancellationToken,
    parent: Option<&ReportStore>,
    mut emit: impl FnMut(EngineEvent),
) -> Result<SelectedRunResult> {
    // Pure ceiling validation precedes even creating an evidence directory.
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
    let provider = launch.provider_factory()?;
    let id = format!("native-{}", nonce()?);
    let store = match parent {
        Some(p) => p.create_child(&id)?,
        None => report::create(&options.output_root.join(&id))?,
    };
    let path = store.native_report_path();
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
    let (selected_report, selected_journal) = if let Some(integration) = integration {
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
    };
    let mut projector = RunProjector::new(&store, selected_report, selected_journal)?;
    projector.bind_launch(&launch.view().fingerprint)?;
    let mut intent_store = store.intent_store()?;
    let mut owners = Owners {
        provider: Some(provider),
        ..Owners::default()
    };
    event(&mut owners, &mut emit, EventKind::Started, &path, None);
    let mut epochs = std::collections::BTreeSet::new();
    for (index, kind) in options.scenarios.into_iter().enumerate() {
        if cancel.is_cancelled() || !owners.panics.is_empty() {
            break;
        }
        let scenario_id = format!("native-{index}-{kind}");
        let ownership_id = nonce()?;
        let mut resource = NativeResource {
            scenario_id: scenario_id.clone(),
            ownership_id: ownership_id.clone(),
            ownership: NativeOwnership::Preparing {
                provider: launch.provider_name().into(),
                invocation_id: nonce()?,
                receipt: None,
            },
            catalog_authority: NativeCatalogAuthority::Valid,
        };
        native_report(&mut projector)
            .scenarios
            .push(NativeScenario {
                id: scenario_id.clone(),
                scenario: kind,
                ownership_id: ownership_id.clone(),
                stages: vec![],
                outcome: RunOutcome::Incomplete,
                dispositions: NativeDispositions::default(),
            });
        if let StoredRun::NativeIntegration(report) = projector.report_mut() {
            report
                .integrations
                .push(crate::native::integration::IntegrationScenarioEvidence {
                    native_lifetime: None,
                    scenario_id: scenario_id.clone(),
                    ownership_id: ownership_id.clone(),
                    services: vec![],
                    desktops: vec![],
                    checks: vec![],
                    dispositions: crate::native::integration::IntegrationDispositions {
                        helper_worker: Disposition::NotStarted,
                        registration: Disposition::NotStarted,
                        manager_action: if report.requirements.service.is_none() && report.requirements.desktop.is_some() {
                            Disposition::Complete {
                                receipt: "no service manager authority declared".into(),
                            }
                        } else {
                            Disposition::NotStarted
                        },
                        broker: if report.requirements.desktop.is_some() {
                            Disposition::Unknown {
                                reason: "original desktop broker not yet bound".into(),
                            }
                        } else {
                            Disposition::Complete {
                                receipt: "no desktop broker required".into(),
                            }
                        },
                        primary: if report.requirements.desktop.as_ref().is_some_and(|d|d.modes==[crate::native::integration::DesktopMode::NativeActivation] && d.lifetime==crate::native::integration::LifetimeRequirement::ActivationInstanceTerminated) {
                            Disposition::NotStarted
                        } else if report.requirements.desktop.is_some() {
                            Disposition::Unknown {
                                reason: "original desktop primary unproved".into(),
                            }
                        } else {
                            Disposition::Complete {
                                receipt: "no owned primary required".into(),
                            }
                        },
                        descendant_scope: if report.requirements.desktop.as_ref().is_some_and(|d|d.modes==[crate::native::integration::DesktopMode::NativeActivation]) {
                            Disposition::NotStarted
                        } else if report.requirements.desktop.is_some() {
                            Disposition::Unknown {
                                reason: "original desktop descendant scope unproved".into(),
                            }
                        } else {
                            Disposition::Complete {
                                receipt: "no owned descendant scope required".into(),
                            }
                        },
                    },
                });
        }
        projector.save_report()?;
        event(
            &mut owners,
            &mut emit,
            EventKind::ScenarioStarted,
            &path,
            Some(&scenario_id),
        );
        let mut provider_started = false;
        let mut provider_prepared = false;
        let mut desktop_header = None;
        let mut preparation_invocation = None;
        let setup = catch_unwind(AssertUnwindSafe(
            || -> Result<super::projector::PublishedResource> {
                ensure!(
                    owners.panics.is_empty(),
                    "native event observer panicked before effects"
                );
                owners.preparation = Some(launch.prepare());
                if owners.preparation.as_ref().unwrap().error().is_some() {
                    anyhow::bail!(
                        "native transport preparation failed; original preparation retained"
                    )
                }
                let mut prepare = invocation(launch, &resource, ProviderAction::Prepare)?;
                if let NativeOwnership::Preparing { invocation_id, .. } = &resource.ownership {
                    prepare.invocation_id = invocation_id.clone();
                }
                let saved = projector.publish_provider(resource.clone(), prepare.clone())?;
                preparation_invocation = Some(prepare.clone());
                provider_started = true;
                owners.remote_unknown = true;
                let receipt = owners.provider.as_mut().unwrap().invoke(saved)?;
                let baseline = owners
                    .provider
                    .as_ref()
                    .unwrap()
                    .validate_original_baseline(launch, &prepare, receipt.clone())?;
                ensure!(
                    epochs.insert(receipt.reset_epoch.clone()),
                    "provider reused a baseline epoch"
                );
                projector.record_provider_receipt(&prepare, receipt.clone())?;
                native_report(&mut projector)
                    .provider_claims
                    .push(receipt.clone());
                let active_launch = launch.with_baseline(&baseline)?;
                desktop_header = active_launch
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
                provider_prepared = true;
                match NativeSession::start_prepared(
                    &active_launch,
                    ClientMode::Fresh,
                    owners.preparation.take().unwrap(),
                ) {
                    Ok(session) => owners.session = Some(session),
                    Err(failure) => {
                        owners.start = Some(failure);
                        anyhow::bail!(
                            "native client startup failed; original startup owner retained"
                        )
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
                        .validate_integration_context(&active_launch, &ownership_id)?;
                    if let Some(integration) = integration
                        && integration.recipe.integrations.desktop.is_some()
                    {
                        if owners.session.as_ref().unwrap().integration_description().is_some_and(|d|matches!(d.manifest.desktop_native,Some(theseus_native_protocol::v2::NativeDesktopDefinition::LinuxGio {..}))) {
                            owners.session.as_mut().unwrap().admit_linux_gio_qualification(&active_launch, &ownership_id)?;
                        }
                        let manifest = &owners
                            .session
                            .as_ref()
                            .unwrap()
                            .integration_description()
                            .ok_or_else(|| {
                                anyhow::anyhow!("desktop description missing before Acquire")
                            })?
                            .manifest;
                        let header = desktop_header.as_ref().ok_or_else(|| {
                            anyhow::anyhow!("desktop header missing before Acquire")
                        })?;
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
                    theseus_host::fs::CommitScope::FileAndDirectorySynced => {
                        "file_and_directory_synced"
                    }
                    theseus_host::fs::CommitScope::FileFlushedAtomicReplace => {
                        "file_flushed_atomic_replace"
                    }
                };
                let identity = NativeIntent {
                    intent: prepared.record().intent.to_hex(),
                    binding: receipt.binding.clone(),
                    manifest_digest: receipt.manifest_digest.clone(),
                    workspace_marker: receipt.workspace_marker.clone(),
                    reset_epoch: receipt.reset_epoch.clone(),
                    publication_scope: scope.into(),
                };
                resource.ownership = NativeOwnership::Intent { prepared: identity };
                let saved = projector.publish_intent(resource.clone(), prepared)?;
                let acquired = owners.session.as_mut().unwrap().acquire(saved, &baseline)?;
                resource.ownership = NativeOwnership::Acquired {
                    prepared: acquired.prepared,
                    lease: acquired.lease.to_hex(),
                };
                projector.publish_acquired(resource.clone())
            },
        ));
        let mut published = None;
        match setup {
            Ok(Ok(p)) => published = Some(p),
            Ok(Err(e)) => owners.errors.push(e),
            Err(p) => owners.panics.push(p),
        }
        if let Some(published) = published {
            // Driver is outside the unwind boundary so panic cannot drop its session.
            let mut driver = NativeDriver::new(
                owners.session.take().unwrap(),
                &mut projector,
                published,
                recipe,
            );
            driver.set_cancellation(cancel.clone());
            match catch_unwind(AssertUnwindSafe(|| {
                driver.set_limits(&options.limits)?;
                if let Some(integration) = integration {
                    if integration.recipe.integrations.desktop.is_some() {
                        let header = desktop_header.take().ok_or_else(|| {
                            anyhow::anyhow!("original desktop provider context missing")
                        })?;
                        if integration.recipe.integrations.service.is_some() {
                            crate::native::integration::driver::IntegrationDriver::combined(
                                &mut driver,
                                integration,
                                header,
                                kind,
                            )?
                            .run(kind)
                        } else {
                            crate::native::integration::desktop::DesktopDriver::new_for_scenario(
                                &mut driver,
                                integration,
                                header,
                                kind,
                            )?
                            .run(kind)
                        }
                    } else {
                        crate::native::integration::driver::IntegrationDriver::new(
                            &mut driver,
                            integration,
                        )
                        .run(kind)
                    }
                } else {
                    driver.run(kind)
                }
            })) {
                Ok(Ok(())) => {}
                Ok(Err(e)) => owners.errors.push(e),
                Err(p) => owners.panics.push(p),
            }
            let released = catch_unwind(AssertUnwindSafe(|| {
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
            }));
            match released {
                Ok(Ok(v2::LeaseView::AwaitingExternalReset { .. })) => {
                    let NativeOwnership::Acquired { lease, .. } = &resource.ownership else {
                        unreachable!()
                    };
                    let disposition = Disposition::Complete {
                        receipt: "original lease awaiting external reset".into(),
                    };
                    if let Err(e) = driver.projector.record_release_completion(
                        &ownership_id,
                        lease,
                        disposition.clone(),
                    ) {
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
                            receipt:
                                "original broker lease released before original helper lease closed"
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
            owners.session = Some(driver.session);
        }
        let completed_stages = native_report(&mut projector)
            .scenarios
            .last()
            .unwrap()
            .stages
            .iter()
            .map(|stage| stage.name.clone())
            .collect::<Vec<_>>();
        for stage in completed_stages {
            let emitted = catch_unwind(AssertUnwindSafe(|| {
                emit(EngineEvent {
                    kind: EventKind::StageFinished,
                    report_path: path.clone(),
                    scenario_id: Some(scenario_id.clone()),
                    stage: Some(stage),
                })
            }));
            if let Err(payload) = emitted {
                owners.panics.push(payload);
            }
        }
        let local_complete = settle_owned(&mut owners);
        let mut reset_complete = !provider_started;
        // Even an unknown preparation/acquisition is disposed by original owner identity.
        if local_complete && provider_started {
            let original = native_ownership(&projector)
                .iter()
                .find(|r| r.ownership_id == ownership_id)
                .cloned();
            if let Some(original) = original {
                owners.pending_cleanup = true;
                let reset = catch_unwind(AssertUnwindSafe(|| -> Result<ProviderReceipt> {
                    if !provider_prepared {
                        let prepare = preparation_invocation.as_ref().ok_or_else(|| {
                            anyhow::anyhow!("original provider preparation identity missing")
                        })?;
                        let receipt = owners.provider.as_mut().unwrap().query(prepare)?.ok_or_else(|| anyhow::anyhow!("original provider preparation remains unknown; no new disposition identity authorized"))?;
                        v2::Digest::parse(&receipt.reset_epoch)?;
                        owners
                            .provider
                            .as_ref()
                            .unwrap()
                            .validate_original_baseline(launch, prepare, receipt.clone())?;
                        let known = native_provider_calls(&projector)
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
                    let original = native_ownership(&projector)
                        .iter()
                        .find(|owner| owner.ownership_id == original.ownership_id)
                        .cloned()
                        .expect("original preparing owner retained");
                    let invocation = invocation(launch, &original, ProviderAction::Dispose)?;
                    let saved = projector.publish_provider(original, invocation.clone())?;
                    native_report(&mut projector)
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
                        epochs.insert(receipt.reset_epoch.clone()),
                        "provider disposition did not advance epoch"
                    );
                    ensure!(
                        receipt.manifest_digest == launch.binding().manifest_digest.to_hex(),
                        "provider disposition exceeds catalog ceiling"
                    );
                    projector.record_provider_receipt(&invocation, receipt.clone())?;
                    Ok(receipt)
                }));
                match reset {
                    Ok(Ok(receipt)) => {
                        reset_complete = true;
                        let r = native_report(&mut projector);
                        r.provider_claims.push(receipt.clone());
                        r.scenarios.last_mut().unwrap().dispositions.reset = Disposition::Complete {
                            receipt: receipt.receipt,
                        }
                    }
                    Ok(Err(e)) => owners.errors.push(e),
                    Err(p) => owners.panics.push(p),
                }
            }
        }
        let all_local = settle_owned(&mut owners);
        owners.remote_unknown = provider_started && !reset_complete;
        owners.pending_cleanup = owners.remote_unknown || !all_local;
        let report = native_report(&mut projector);
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
                "native infrastructure or publication failed; original typed failure retained"
                    .into(),
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
            let sut_failed = evidence
                .checks
                .iter()
                .any(|c| c.outcome == crate::model::CheckOutcome::Failed);
            let integration_outcome = if sut_failed {
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
            };
            report.outcome = RunOutcome::aggregate([report.catalog.outcome, integration_outcome]);
        }
        if let Err(e) = projector.save_report() {
            owners.errors.push(e)
        }
        event(
            &mut owners,
            &mut emit,
            EventKind::ScenarioFinished,
            &path,
            Some(&scenario_id),
        );
        if !owners.errors.is_empty() || !owners.panics.is_empty() || !all_local || !reset_complete {
            break;
        }
        owners.session = None;
        owners.start = None;
        owners.preparation = None;
    }
    if cancel.is_cancelled() {
        if let StoredRun::NativeIntegration(report) = projector.report_mut() {
            report.outcome = RunOutcome::Cancelled;
        }
        *native_report(&mut projector).outcome = RunOutcome::Cancelled;
    }
    if let Err(e) = projector.save_report() {
        owners.errors.push(e)
    }
    event(&mut owners, &mut emit, EventKind::Finished, &path, None);
    if !owners.errors.is_empty() || !owners.panics.is_empty() || owners.pending_cleanup {
        let (retained_report, retained_journal) = projector.into_parts();
        owners.evidence = Some((store, retained_report, retained_journal));
        owners.intents = Some(intent_store);
        return Err(retained_failure(
            format!("native run incomplete; evidence at {}", path.display()),
            owners,
        ));
    }
    let (report, _) = projector.into_parts();
    Ok(SelectedRunResult {
        report,
        report_path: path,
    })
}

#[cfg(test)]
mod retained_panic_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct DropProbe(Arc<AtomicUsize>);
    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[derive(Debug)]
    struct PanickingSource(Arc<AtomicUsize>);
    impl std::fmt::Display for PanickingSource {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("inert source")
        }
    }
    impl std::error::Error for PanickingSource {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            std::panic::panic_any(DropProbe(self.0.clone()))
        }
    }
    #[test]
    fn every_observer_and_error_capture_panic_payload_remains_owned_until_bundle_drop() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let mut owners = Owners::default();
        owners
            .errors
            .push(anyhow::Error::new(PanickingSource(dropped.clone())));
        owners
            .errors
            .push(anyhow::Error::new(PanickingSource(dropped.clone())));
        owners.capture_errors();
        let mut observer = |_: EngineEvent| std::panic::panic_any(DropProbe(dropped.clone()));
        event(
            &mut owners,
            &mut observer,
            EventKind::Started,
            Path::new("inert"),
            None,
        );
        event(
            &mut owners,
            &mut observer,
            EventKind::Finished,
            Path::new("inert"),
            None,
        );
        assert_eq!(owners.panics.len(), 4);
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        let error = retained_failure("inert panic ownership test".into(), owners);
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        drop(error);
        assert_eq!(dropped.load(Ordering::SeqCst), 4);
    }
}
