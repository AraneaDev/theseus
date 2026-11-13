//! Closed backend dispatch and native publication-before-effect orchestration.
use super::{CancellationToken, EngineEvent, EventKind, domain::*, projector::RunProjector};
use crate::{
    model::RunOutcome,
    native::{adapter::NativeDriver, launch::LaunchAuthority, model::*},
    report::{self, ReportStore},
};
use anyhow::Result;
use std::panic::{AssertUnwindSafe, catch_unwind};

mod native_run;
mod owners;
use native_run::{
    RunCtx, SetupFlags, create_envelopes, dispose_provider, drive_scenario, finalize_scenario,
    prepare_and_acquire, record_release, release_original, validate_ceiling,
};
pub(crate) use owners::{
    NativeCleanupFacts, finalize_native_cleanup, native_cleanup_facts, native_cleanup_pending,
};
pub(super) use owners::{Owners, event, retained_failure, settle_owned};
#[cfg(feature = "native-catalog-test-support")]
pub use owners::{
    fixture_finalize_native_cleanup, fixture_native_cleanup_facts, fixture_retained_native_session,
};

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
/// Pure initial integration dispositions derived from declared requirements.
fn initial_integration_dispositions(
    requirements: &crate::native::integration::IntegrationRequirements,
    os: crate::native::recipe::NativeOs,
) -> crate::native::integration::IntegrationDispositions {
    crate::native::integration::IntegrationDispositions {
        helper_worker: Disposition::NotStarted,
        registration: Disposition::NotStarted,
        manager_action: if requirements.service.is_none() && requirements.desktop.is_some() {
            Disposition::Complete {
                receipt: "no service manager authority declared".into(),
            }
        } else {
            Disposition::NotStarted
        },
        broker: if requirements.desktop.is_some() {
            Disposition::Unknown {
                reason: "original desktop broker not yet bound".into(),
            }
        } else {
            Disposition::Complete {
                receipt: "no desktop broker required".into(),
            }
        },
        primary: if requirements.desktop.as_ref().is_some_and(|d| {
            d.modes == [crate::native::integration::DesktopMode::NativeActivation]
                && d.lifetime
                    == crate::native::integration::LifetimeRequirement::ActivationInstanceTerminated
        }) {
            Disposition::NotStarted
        } else if requirements.desktop.is_some() {
            Disposition::Unknown {
                reason: "original desktop primary unproved".into(),
            }
        } else {
            Disposition::Complete {
                receipt: "no owned primary required".into(),
            }
        },
        descendant_scope: if requirements.desktop.as_ref().is_some_and(|d| {
            d.modes == [crate::native::integration::DesktopMode::NativeActivation]
                || (os == crate::native::recipe::NativeOs::Macos
                    && d.modes == [crate::native::integration::DesktopMode::OwnedExecutable]
                    && d.lifetime
                        == crate::native::integration::LifetimeRequirement::OwnedPrimaryExited)
        }) {
            Disposition::NotStarted
        } else if requirements.desktop.is_some() {
            Disposition::Unknown {
                reason: "original desktop descendant scope unproved".into(),
            }
        } else {
            Disposition::Complete {
                receipt: "no owned descendant scope required".into(),
            }
        },
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
    validate_ceiling(recipe, integration, &options, launch)?;
    let ctx = RunCtx {
        recipe,
        integration,
        launch,
    };
    let provider = launch.provider_factory()?;
    let id = format!("native-{}", nonce()?);
    let store = match parent {
        Some(p) => p.create_child(&id)?,
        None => report::create(&options.output_root.join(&id))?,
    };
    let path = store.native_report_path();
    let (selected_report, selected_journal) = create_envelopes(id, recipe, integration, &options)?;
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
        let resource = NativeResource {
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
                    owned_lifetime: None,
                    native_lifetime: None,
                    scenario_id: scenario_id.clone(),
                    ownership_id: ownership_id.clone(),
                    services: vec![],
                    desktops: vec![],
                    checks: vec![],
                    dispositions: initial_integration_dispositions(
                        &report.requirements,
                        recipe.recipe.os,
                    ),
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
        // Lives outside the setup unwind boundary: writes survive Err/panic.
        let mut flags = SetupFlags {
            resource,
            provider_started: false,
            provider_prepared: false,
            desktop_header: None,
            preparation_invocation: None,
            epochs: &mut epochs,
        };
        let setup = catch_unwind(AssertUnwindSafe(
            || -> Result<super::projector::PublishedResource> {
                prepare_and_acquire(
                    &ctx,
                    &mut flags,
                    &mut owners,
                    &mut projector,
                    &mut intent_store,
                    &ownership_id,
                    kind,
                )
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
                drive_scenario(
                    &mut driver,
                    integration,
                    &options.limits,
                    &mut flags.desktop_header,
                    kind,
                )
            })) {
                Ok(Ok(())) => {}
                Ok(Err(e)) => owners.errors.push(e),
                Err(p) => owners.panics.push(p),
            }
            let released = catch_unwind(AssertUnwindSafe(|| release_original(&mut driver)));
            record_release(
                &mut driver,
                &mut owners,
                &flags.resource,
                &ownership_id,
                released,
            );
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
        let provider_started = flags.provider_started;
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
                    dispose_provider(launch, &mut flags, &mut owners, &mut projector, &original)
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
        let all_local = finalize_scenario(
            integration,
            &mut owners,
            &mut projector,
            &cancel,
            provider_started,
            reset_complete,
        );
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
    use std::path::Path;
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
