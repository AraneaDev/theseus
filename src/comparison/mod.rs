//! Explicit comparison suites and pure evidence derivation.
mod manifest;
mod model;
mod scope;
pub use manifest::{ValidatedSuite, ValidatedVariant, load, validate_manifest};
pub use model::*;
pub use scope::compare_scope;

use crate::{
    model::{ProfileName, RunOutcome, RunReport, ScenarioKind},
    snapshot::Snapshot,
};
use std::collections::BTreeSet;

fn same_selection<T: Ord>(a: &[T], b: &[T]) -> bool {
    a.len() == b.len() && a.iter().collect::<BTreeSet<_>>() == b.iter().collect::<BTreeSet<_>>()
}
fn snapshots<'a>(
    source: &'a SourceReport,
    suite: &SuiteManifest,
    profile: ProfileName,
    scenario: ScenarioKind,
    stage: &str,
) -> Result<&'a [Snapshot], String> {
    if source.error.is_some() {
        return Err(format!("source {} has an error", source.variant_id));
    }
    let report = source
        .report
        .as_ref()
        .ok_or_else(|| format!("source {} has no report", source.variant_id))?;
    if !same_selection(&report.selected_profiles, &suite.profiles)
        || !same_selection(&report.selected_scenarios, &suite.scenarios)
    {
        return Err(format!(
            "source {} selection does not match suite matrix",
            source.variant_id
        ));
    }
    let mut scenarios = report
        .scenarios
        .iter()
        .filter(|s| s.profile == profile && s.scenario == scenario);
    let selected = scenarios
        .next()
        .ok_or_else(|| format!("source {} missing {profile}/{scenario}", source.variant_id))?;
    if scenarios.next().is_some() {
        return Err(format!(
            "source {} has duplicate {profile}/{scenario}",
            source.variant_id
        ));
    }
    let mut stages = selected.stages.iter().filter(|s| s.name == stage);
    let selected = stages
        .next()
        .ok_or_else(|| format!("source {} missing stage {stage}", source.variant_id))?;
    if stages.next().is_some() {
        return Err(format!(
            "source {} has duplicate stage {stage}",
            source.variant_id
        ));
    }
    Ok(&selected.snapshots)
}
fn source<'a>(sources: &'a [SourceReport], id: &str) -> Result<&'a SourceReport, String> {
    let mut matching = sources.iter().filter(|s| s.variant_id == id);
    let selected = matching
        .next()
        .ok_or_else(|| format!("missing source {id}"))?;
    if matching.next().is_some() {
        return Err(format!("duplicate source {id}"));
    }
    Ok(selected)
}
fn source_outcome(source: &SourceReport) -> RunOutcome {
    let Some(report) = &source.report else {
        return RunOutcome::Incomplete;
    };
    let mut normalized: RunReport = report.clone();
    normalized.recompute_outcome();
    let mut outcomes = vec![report.outcome, normalized.outcome];
    if source.error.is_some() || report.finished_at.is_none() {
        outcomes.push(RunOutcome::Incomplete);
    }
    RunOutcome::aggregate(outcomes)
}
/// Derive verdicts solely from supplied evidence, never from the filesystem.
/// Recomputes lifecycle outcomes conservatively (retaining adverse saved outcomes).
/// Callers must validate source path/digest/recipe identity and normalize old report
/// schemas with the existing report reader before supplying persisted evidence.
/// Invalid dimensions return Incomplete and no cells before matrix allocation.
pub fn derive(suite: &SuiteManifest, sources: &[SourceReport]) -> ComparisonReport {
    let mut result = ComparisonReport {
        cells: vec![],
        outcome: RunOutcome::Incomplete,
        diagnostics: vec![],
    };
    if let Err(error) = validate_manifest(suite) {
        result.diagnostics.push(format!("invalid suite: {error}"));
        return result;
    }
    if sources.len() > 16 {
        result.diagnostics.push("more than 16 sources".into());
        return result;
    }
    let mut outcomes = sources.iter().map(source_outcome).collect::<Vec<_>>();
    for input in sources {
        if !suite.variants.iter().any(|v| v.id == input.variant_id) {
            result
                .diagnostics
                .push("source refers to an undeclared variant".into());
            outcomes.push(RunOutcome::Incomplete);
        }
    }
    for variant in &suite.variants {
        if let Err(reason) = source(sources, &variant.id) {
            result.diagnostics.push(reason);
            outcomes.push(RunOutcome::Incomplete);
        }
    }
    let mut differences = 0;
    for variant in suite.variants.iter().filter(|v| v.id != suite.baseline) {
        for profile in &suite.profiles {
            for scenario in &suite.scenarios {
                for stage in &suite.stages {
                    for scope in suite.scopes.iter().filter(|s| s.profile == *profile) {
                        let comparison = if differences >= MAX_SUITE_DIFFERENCES {
                            ScopeComparison::unknown("suite difference storage limit exhausted")
                        } else {
                            let evidence = (|| {
                                let a = source(sources, &suite.baseline)?;
                                let b = source(sources, &variant.id)?;
                                Ok::<_, String>((
                                    snapshots(a, suite, *profile, *scenario, stage)?,
                                    snapshots(b, suite, *profile, *scenario, stage)?,
                                ))
                            })();
                            match evidence {
                                Err(reason) => ScopeComparison::unknown(reason),
                                Ok((a, b)) => scope::compare_scope_bounded(
                                    a,
                                    b,
                                    &scope.path,
                                    MAX_CELL_DIFFERENCES.min(MAX_SUITE_DIFFERENCES - differences),
                                ),
                            }
                        };
                        differences += comparison.differences.len();
                        outcomes.push(match comparison.status {
                            ComparisonStatus::Equivalent => RunOutcome::Passed,
                            ComparisonStatus::Different => RunOutcome::Failed,
                            ComparisonStatus::Unknown => RunOutcome::Incomplete,
                        });
                        result.cells.push(ComparisonCell {
                            variant_id: variant.id.clone(),
                            profile: *profile,
                            scenario: *scenario,
                            stage: stage.clone(),
                            scope_id: scope.id.clone(),
                            comparison,
                        });
                    }
                }
            }
        }
    }
    result.outcome = RunOutcome::aggregate(outcomes);
    result
}
