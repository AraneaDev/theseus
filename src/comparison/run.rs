//! Sequential orchestration through the lifecycle engine, with durable recovery refs.
use super::*;
use crate::{
    engine::{self, CancellationToken, EngineEvent, EventKind, RunOptions},
    report::ReportStore,
    runner::{Docker, Environment},
};
use anyhow::{Context, Result, anyhow, ensure};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone)]
pub struct SuiteRunOptions {
    pub output_root: PathBuf,
    pub pull: bool,
}
#[derive(Debug, Clone)]
pub struct ComparisonEvent {
    pub report_path: PathBuf,
    pub variant_id: Option<String>,
    pub source: Option<EngineEvent>,
}
pub struct ComparisonResult {
    pub report: ComparisonReport,
    pub report_path: PathBuf,
}
pub fn run(
    suite: &ValidatedSuite,
    options: SuiteRunOptions,
    cancel: CancellationToken,
    emit: impl FnMut(ComparisonEvent),
) -> Result<ComparisonResult> {
    run_with(suite, options, cancel, &mut Docker::default(), emit)
}
pub fn run_with(
    suite: &ValidatedSuite,
    options: SuiteRunOptions,
    cancel: CancellationToken,
    environment: &mut impl Environment,
    mut emit: impl FnMut(ComparisonEvent),
) -> Result<ComparisonResult> {
    validate_manifest(&suite.manifest)?;
    ensure!(
        suite.variants.len() == suite.manifest.variants.len()
            && suite
                .variants
                .iter()
                .zip(&suite.manifest.variants)
                .all(|(a, b)| a.id == b.id),
        "validated variant identities do not match manifest"
    );
    for variant in &suite.variants {
        variant
            .recipe
            .validate_selection(&suite.manifest.profiles, &suite.manifest.scenarios)?;
    }
    let id = format!(
        "{}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        std::process::id()
    );
    let parent = ReportStore::safe_root(&options.output_root, true)?;
    let store = parent.create_child(&id)?;
    let source_root = store.create_child("sources")?;
    let report_path = store.root().join("comparison.json");
    let mut artifact = ComparisonArtifact {
        schema_version: COMPARISON_SCHEMA_VERSION,
        suite_id: id,
        manifest: suite.manifest.clone(),
        manifest_digest: store::manifest_digest(&suite.manifest)?,
        recipe_digests: suite
            .variants
            .iter()
            .map(|v| (v.id.clone(), v.recipe.digest.clone()))
            .collect(),
        sources: vec![],
        report: derive(&suite.manifest, &[]),
        cancelled: cancel.is_cancelled(),
        finished: false,
    };
    store::save(&store, &artifact)
        .with_context(|| format!("save suite {}", report_path.display()))?;
    emit(ComparisonEvent {
        report_path: report_path.clone(),
        variant_id: None,
        source: None,
    });
    let mut failure = None;
    for variant in &suite.variants {
        if cancel.is_cancelled() || failure.is_some() {
            break;
        }
        let variant_store = match source_root.create_child(&variant.id) {
            Ok(store) => store,
            Err(error) => {
                failure = Some(error);
                break;
            }
        };
        let mut source_path = None;
        let mut tracked = super::environment::TrackedEnvironment {
            inner: environment,
            error: None,
        };
        let result = engine::run_with_parent(
            &variant.recipe,
            RunOptions {
                profiles: suite.manifest.profiles.clone(),
                scenarios: suite.manifest.scenarios.clone(),
                output_root: variant_store.root().to_path_buf(),
                pull: options.pull,
                log_limit: crate::recipe::DEFAULT_LOG_LIMIT,
            },
            cancel.clone(),
            &mut tracked,
            Some(&variant_store),
            |event| {
                if event.kind == EventKind::Started {
                    let run_id = event
                        .report_path
                        .parent()
                        .and_then(|p| p.file_name())
                        .expect("engine generated run directory")
                        .to_string_lossy();
                    let path = format!("sources/{}/{run_id}", variant.id);
                    source_path = Some(path.clone());
                    artifact.sources.push(SourceReference {
                        variant_id: variant.id.clone(),
                        path,
                        report_digest: None,
                        error: None,
                    });
                }
                let started = event.kind == EventKind::Started;
                emit(ComparisonEvent {
                    report_path: report_path.clone(),
                    variant_id: Some(variant.id.clone()),
                    source: Some(event),
                });
                if started && let Err(error) = store::save(&store, &artifact) {
                    failure = Some(anyhow!(
                        "save source reference in {}; source recovery directory {}: {error:#}",
                        report_path.display(),
                        store
                            .root()
                            .join(source_path.as_ref().expect("Started reference"))
                            .display()
                    ));
                    cancel.cancel();
                }
            },
        );
        let mut stop = failure.is_some() || cancel.is_cancelled();
        if let Some(path) = &source_path {
            let source = artifact.sources.last_mut().expect("Started reference");
            match result {
                Ok(result) => {
                    // Preflight and engine infrastructure errors are recorded at run
                    // level; ordinary lifecycle Failed/Incomplete sources continue.
                    let mut errors = result.report.errors.clone();
                    for scenario in &result.report.scenarios {
                        errors.extend(scenario.errors.clone());
                        if let crate::model::TeardownStatus::Failed { reason } = &scenario.teardown
                        {
                            errors.push(reason.clone());
                        }
                    }
                    stop |= !errors.is_empty();
                    if !errors.is_empty() {
                        source.error = Some(errors.join("; "));
                    }
                }
                Err(error) => {
                    source.error = Some(format!("{error:#}"));
                    stop = true;
                    failure.get_or_insert(error);
                }
            }
            if let Some(error) = tracked.error {
                source.error = Some(error);
                stop = true;
            }
            match store
                .open_child(path)
                .and_then(|source| source.read_bytes("report.json"))
            {
                Ok(bytes) => source.report_digest = Some(store::digest(&bytes)),
                Err(error) => {
                    source.error = Some(format!("finalize source: {error:#}"));
                    stop = true;
                    failure.get_or_insert(error);
                }
            }
        } else if let Err(error) = result {
            failure.get_or_insert(error);
            stop = true;
        }
        artifact.cancelled = cancel.is_cancelled();
        if let Err(error) = store::save(&store, &artifact) {
            failure.get_or_insert(error.context(format!("save suite {}", report_path.display())));
            stop = true;
            cancel.cancel();
        }
        if stop {
            break;
        }
    }
    artifact.cancelled = cancel.is_cancelled();
    artifact.finished = failure.is_none();
    match store::read_sources(&store, &artifact) {
        Ok(sources) => artifact.report = store::recompute(&artifact, &sources),
        Err(error) => {
            failure.get_or_insert(error);
            artifact.finished = false;
            artifact.report = store::recompute(&artifact, &[]);
        }
    }
    if let Err(error) = store::save(&store, &artifact) {
        failure.get_or_insert(error);
    }
    if let Some(error) = failure {
        return Err(error).with_context(|| {
            format!(
                "comparison stopped; recovery suite {}; sources {}",
                report_path.display(),
                artifact
                    .sources
                    .iter()
                    .map(|s| store.root().join(&s.path).display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
    }
    emit(ComparisonEvent {
        report_path: report_path.clone(),
        variant_id: None,
        source: None,
    });
    Ok(ComparisonResult {
        report: artifact.report,
        report_path,
    })
}
