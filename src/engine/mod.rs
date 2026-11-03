//! Blocking scenario engine. Call on a worker thread; event callbacks must not block.
pub(crate) mod common;
pub mod domain;
mod recovery;
mod selected;
pub use recovery::{NativeRecoveryResult, recover_native};
pub use selected::run_selected;
pub(crate) use selected::{
    NativeCleanupFacts, finalize_native_cleanup, native_cleanup_facts, native_cleanup_pending,
    run_selected_with_parent,
};
#[cfg(feature = "native-catalog-test-support")]
pub use selected::{
    fixture_finalize_native_cleanup, fixture_native_cleanup_facts, fixture_retained_native_session,
};
mod downgrade;
mod interrupted_rollback;
mod interruption;
mod plugin;
pub mod projector;
mod rollback;
mod rollback_checks;
mod stages;
use crate::{
    model::*,
    recipe::{Expectation, ValidatedRecipe},
    report,
    runner::{Docker, Environment},
};
use anyhow::{Context, Result, ensure};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);
impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    pub fn flag(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }
}
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub profiles: Vec<ProfileName>,
    pub scenarios: Vec<ScenarioKind>,
    /// Parent of a newly generated private run directory.
    pub output_root: PathBuf,
    pub pull: bool,
    pub log_limit: u64,
}
impl RunOptions {
    pub fn all(recipe: &ValidatedRecipe) -> Self {
        Self {
            profiles: recipe.recipe.profiles.keys().copied().collect(),
            scenarios: recipe.declared_scenarios(),
            output_root: recipe.project_root.join(".theseus-runs"),
            pull: false,
            log_limit: crate::recipe::DEFAULT_LOG_LIMIT,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Started,
    ScenarioStarted,
    StageStarted,
    StageFinished,
    ScenarioFinished,
    Finished,
}
#[derive(Debug, Clone)]
pub struct EngineEvent {
    pub kind: EventKind,
    pub report_path: PathBuf,
    pub scenario_id: Option<String>,
    pub stage: Option<String>,
}
pub struct RunResult {
    pub report: RunReport,
    pub report_path: PathBuf,
}

pub fn run(
    recipe: &ValidatedRecipe,
    options: RunOptions,
    cancel: CancellationToken,
    emit: impl FnMut(EngineEvent),
) -> Result<RunResult> {
    run_with(recipe, options, cancel, &mut Docker::default(), emit)
}
pub fn run_with(
    recipe: &ValidatedRecipe,
    options: RunOptions,
    cancel: CancellationToken,
    environment: &mut impl Environment,
    emit: impl FnMut(EngineEvent),
) -> Result<RunResult> {
    run_with_parent(recipe, options, cancel, environment, None, emit)
}
/// A pinned parent keeps suite-owned run creation inside the opened directory.
pub(crate) fn run_with_parent(
    recipe: &ValidatedRecipe,
    options: RunOptions,
    cancel: CancellationToken,
    environment: &mut impl Environment,
    parent: Option<&report::ReportStore>,
    mut emit: impl FnMut(EngineEvent),
) -> Result<RunResult> {
    recipe.validate_selection(&options.profiles, &options.scenarios)?;
    ensure!(
        options
            .profiles
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == options.profiles.len()
            && options
                .scenarios
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == options.scenarios.len(),
        "duplicate matrix selection"
    );
    ensure!(
        options.log_limit > 0 && options.log_limit <= crate::recipe::DEFAULT_LOG_LIMIT,
        "invalid log limit"
    );
    let id = format!(
        "{}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        std::process::id()
    );
    let directory = options.output_root.join(&id);
    let store = match parent {
        Some(parent) => parent.create_child(&id),
        None => report::create(&directory),
    }
    .with_context(|| format!("create run evidence directory {}", directory.display()))?;
    let mut report = RunReport::new(&id);
    report.recipe_path = recipe.path.display().to_string();
    report.recipe_digest = recipe.digest.clone();
    report.image = recipe.recipe.image.clone();
    report.network = recipe.recipe.network;
    report.selected_profiles = options.profiles.clone();
    report.selected_scenarios = options.scenarios.clone();
    report.started_at = now();
    let mut journal = RunJournal {
        schema_version: JOURNAL_SCHEMA_VERSION,
        run_id: id.clone(),
        resources: vec![],
        cleanup_errors: vec![],
    };
    store
        .save_journal(&journal)
        .with_context(|| format!("save run journal at {}", store.root().display()))?;
    save_event(
        &store,
        &mut report,
        &mut emit,
        EventKind::Started,
        None,
        None,
    )?;
    // Suite Started observers may cancel after failing to persist recovery refs.
    // Do not call prepare (which can pull images) after that boundary fails.
    // Standalone runs retain their existing pre-cancelled scenario evidence.
    let mut cleanup = crate::runner::process::CleanupFailures::default();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let prepared = (|| {
            ensure!(
                parent.is_none() || !cancel.is_cancelled(),
                "cancelled before environment preparation"
            );
            preflight_env(recipe, &options)
        })()
        .and_then(|()| environment.prepare(recipe, options.pull, &cancel));
        match prepared {
            Err(error) => {
                cleanup.retain(&error);
                report.errors.push(format!("preflight: {error:#}"));
            }
            Ok(prepared) => {
                report.image_id = Some(prepared.image_id);
                report.artifacts = prepared.artifacts;
                'profiles: for profile in &options.profiles {
                    for scenario in &options.scenarios {
                        let index = report.scenarios.len();
                        let sid = format!("{profile}-{scenario}");
                        report
                            .scenarios
                            .push(ScenarioReport::new(&sid, *profile, *scenario));
                        report.scenarios[index].mcp = recipe.recipe.profiles[profile].mcp.clone();
                        if *scenario == ScenarioKind::PluginLifecycle {
                            report.scenarios[index].plugin =
                                recipe.recipe.profiles[profile].plugin.clone();
                        }
                        if *scenario == ScenarioKind::Downgrade {
                            report.scenarios[index].downgrade =
                                recipe.recipe.profiles[profile].downgrade.clone();
                        }
                        if *scenario == ScenarioKind::InterruptedRollback {
                            report.scenarios[index].interrupted_rollback =
                                recipe.recipe.profiles[profile].interrupted_rollback.clone();
                        }
                        if *scenario == ScenarioKind::Rollback {
                            report.scenarios[index].rollback =
                                recipe.recipe.profiles[profile].rollback.clone();
                        }
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            stages::scenario(stages::ScenarioContext {
                                recipe,
                                options: &options,
                                cancel: &cancel,
                                environment,
                                store: &store,
                                report: &mut report,
                                index,
                                journal: &mut journal,
                                emit: &mut emit,
                                cleanup: &mut cleanup,
                            })
                        }));
                        let mut panic = None;
                        let result = match result {
                            Ok(result) => result,
                            Err(original) => {
                                cleanup.retain_panic(original.as_ref());
                                panic = Some(original);
                                Err(anyhow::anyhow!(
                                    "scenario panicked; owned guest cleanup required"
                                ))
                            }
                        };
                        let interrupted = result.is_err() || cancel.is_cancelled();
                        if let Err(error) = result {
                            cleanup.retain(&error);
                            report.scenarios[index]
                                .errors
                                .push(format!("scenario: {error:#}"));
                        }
                        if interrupted {
                            mark_remaining_checks(
                                recipe,
                                &mut report.scenarios[index],
                                cancel.is_cancelled(),
                            );
                        }
                        if let Some(resource) = journal.resources.iter_mut().find(|r| {
                            Some(&r.container_id) == report.scenarios[index].container_id.as_ref()
                        }) {
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                environment.remove(resource)
                            })) {
                                Ok(Ok(())) => {
                                    resource.removed = true;
                                    report.scenarios[index].teardown = TeardownStatus::Complete;
                                }
                                Ok(Err(error)) => {
                                    cleanup.retain(&error);
                                    let reason = format!("{error:#}");
                                    journal.cleanup_errors.push(reason.clone());
                                    report.scenarios[index].teardown =
                                        TeardownStatus::Failed { reason };
                                }
                                Err(original) => {
                                    cleanup.retain_panic(original.as_ref());
                                    if panic.is_none() {
                                        panic = Some(original);
                                    }
                                    let reason = "owned guest teardown panicked".to_string();
                                    journal.cleanup_errors.push(reason.clone());
                                    report.scenarios[index].teardown =
                                        TeardownStatus::Failed { reason };
                                }
                            }
                        } else if report.scenarios[index].container_id.is_none() {
                            report.scenarios[index].teardown = TeardownStatus::Complete;
                        }
                        if let Err(error) = store.save_journal(&journal) {
                            report
                                .errors
                                .push(format!("save cleanup journal: {error:#}"));
                        }
                        if let Some(original) = panic {
                            std::panic::resume_unwind(original);
                        }
                        save_event(
                            &store,
                            &mut report,
                            &mut emit,
                            EventKind::ScenarioFinished,
                            Some(sid),
                            None,
                        )?;
                        if cleanup.pending() {
                            break 'profiles;
                        }
                    }
                }
            }
        }
        report.cancelled |= cancel.is_cancelled();
        report.finished_at = Some(now());
        save_event(
            &store,
            &mut report,
            &mut emit,
            EventKind::Finished,
            None,
            None,
        )?;
        Ok(RunResult {
            report,
            report_path: store.report_path(),
        })
    }));
    match result {
        Ok(result) => {
            cleanup.finish()?;
            result
        }
        Err(original) => cleanup.resume_unwind(original),
    }
}
fn preflight_env(recipe: &ValidatedRecipe, options: &RunOptions) -> Result<()> {
    for name in &options.profiles {
        let profile = &recipe.recipe.profiles[name];
        if let Some(p) = &profile.mcp {
            ensure!(
                recipe.recipe.schema_version == 8
                    && recipe.recipe.network == crate::recipe::Network::None
                    && *name == ProfileName::User
                    && recipe.declared_scenarios().len() == 1,
                "invalid MCP execution boundary"
            );
            for scenario in &options.scenarios {
                crate::recipe::validate_mcp_contract(profile, *scenario)?;
            }
            let path = recipe
                .target_artifact
                .join(&p.client_artifact)
                .canonicalize()
                .context("resolve MCP client artifact")?;
            ensure!(
                path.starts_with(&recipe.target_artifact)
                    && path.is_file()
                    && path.metadata()?.len() > 0,
                "MCP client artifact must be a nonempty confined target file"
            );
        }
        let mut names: Vec<&String> = profile.steps.values().flat_map(|c| &c.pass_env).collect();
        for check in &profile.checks {
            match &check.expectation {
                Expectation::McpStdio { probe } => probe.validate()?,
                Expectation::JsonCommand { pass_env, .. }
                | Expectation::Command { pass_env, .. }
                | Expectation::Executable { pass_env, .. } => names.extend(pass_env),
                _ => {}
            }
        }
        for name in names {
            let value = std::env::var_os(name)
                .ok_or_else(|| anyhow::anyhow!("missing pass_env variable {name}"))?;
            ensure!(
                !value.as_encoded_bytes().contains(&0),
                "pass_env variable {name} contains NUL"
            );
        }
    }
    Ok(())
}
pub(crate) fn save_event(
    store: &report::ReportStore,
    report: &mut RunReport,
    emit: &mut impl FnMut(EngineEvent),
    kind: EventKind,
    scenario_id: Option<String>,
    stage: Option<String>,
) -> Result<()> {
    report.recompute_outcome();
    store
        .save(report)
        .with_context(|| format!("save run evidence at {}", store.report_path().display()))?;
    emit(EngineEvent {
        kind,
        report_path: store.report_path(),
        scenario_id,
        stage,
    });
    Ok(())
}
/// RFC3339 UTC without invoking a host command.
pub(crate) fn now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = seconds / 86400;
    let day_seconds = seconds % 86400;
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        day_seconds / 3600,
        day_seconds / 60 % 60,
        day_seconds % 60
    )
}

fn mark_remaining_checks(recipe: &ValidatedRecipe, scenario: &mut ScenarioReport, cancelled: bool) {
    let mut stage = StageReport::new("unperformed_checks");
    stage.outcome = if cancelled {
        StageOutcome::Cancelled
    } else {
        StageOutcome::Incomplete
    };
    stage.skip_reason = Some("scenario interrupted before these checks could run".into());
    for check in &recipe.recipe.profiles[&scenario.profile].checks {
        if !checkpoint_applies_with_policy(
            scenario.scenario,
            check.checkpoint,
            scenario.downgrade.as_ref(),
        ) {
            continue;
        }
        if !checkpoint_applies_with_recovery(
            scenario.scenario,
            check.checkpoint,
            scenario.rollback_recovery(),
        ) {
            continue;
        }
        if matches!(check.expectation, Expectation::Changed { .. })
            && check.checkpoint == Checkpoint::Interrupted
            && !scenario
                .rollback_recovery()
                .is_some_and(|p| p.change_checks.contains(&check.id))
        {
            continue;
        }
        if scenario
            .stages
            .iter()
            .flat_map(|s| &s.checks)
            .any(|c| c.id == check.id)
        {
            continue;
        }
        stage.checks.push(CheckResult {
            mcp_required: matches!(check.expectation, Expectation::McpStdio { .. }),
            mcp: None,
            json_expectation: None,
            json_required: matches!(check.expectation, Expectation::JsonCommand { .. }),
            command_expectation: None,
            id: check.id.clone(),
            checkpoint: check.checkpoint,
            outcome: CheckOutcome::Incomplete,
            message: "scenario interrupted; check not performed".into(),
            path: check.expectation.path().map(str::to_owned),
            command_ids: vec![],
            backup: None,
            comparison: matches!(
                check.expectation,
                Expectation::Unchanged { .. } | Expectation::Changed { .. }
            )
            .then(|| ComparisonReference::for_checkpoint(check.checkpoint))
            .flatten(),
        });
    }
    if !stage.checks.is_empty() {
        scenario.stages.push(stage);
    }
}
