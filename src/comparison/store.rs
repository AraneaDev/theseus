//! Bounded comparison envelopes and fd-relative source access.
use super::*;
use crate::{
    model::RunOutcome,
    report::{self, ReportStore},
    runner::{Docker, Environment},
};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path};

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn manifest_digest(manifest: &SuiteManifest) -> Result<String> {
    Ok(digest(&serde_json::to_vec(manifest)?))
}
fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
pub(super) fn run_id(source: &SourceReference) -> Result<&str> {
    let parts = source.path.split('/').collect::<Vec<_>>();
    ensure!(
        parts.len() == 3
            && parts[0] == "sources"
            && parts[1] == source.variant_id
            && safe_id(parts[1])
            && safe_id(parts[2]),
        "unsafe source reference {}",
        source.path
    );
    Ok(parts[2])
}
fn validate(artifact: &ComparisonArtifact) -> Result<()> {
    ensure!(
        artifact.schema_version == COMPARISON_SCHEMA_VERSION,
        "unsupported comparison schema {}",
        artifact.schema_version
    );
    ensure!(safe_id(&artifact.suite_id), "invalid suite identity");
    validate_manifest(&artifact.manifest)?;
    ensure!(
        artifact.manifest_digest == manifest_digest(&artifact.manifest)?,
        "suite manifest digest mismatch"
    );
    let variants = artifact
        .manifest
        .variants
        .iter()
        .map(|v| v.id.as_str())
        .collect::<BTreeSet<_>>();
    ensure!(
        artifact.recipe_digests.len() == variants.len()
            && artifact
                .recipe_digests
                .iter()
                .all(|(id, hash)| variants.contains(id.as_str()) && valid_digest(hash)),
        "invalid declared recipe identities"
    );
    ensure!(artifact.sources.len() <= 16, "too many comparison sources");
    let mut seen = BTreeSet::new();
    for source in &artifact.sources {
        run_id(source)?;
        ensure!(
            variants.contains(source.variant_id.as_str()) && seen.insert(&source.variant_id),
            "undeclared or duplicate source variant"
        );
        ensure!(
            source
                .report_digest
                .as_ref()
                .is_none_or(|hash| valid_digest(hash)),
            "invalid source content digest"
        );
    }
    Ok(())
}
pub(super) fn save(store: &ReportStore, artifact: &ComparisonArtifact) -> Result<()> {
    validate(artifact)?;
    store.atomic_json("comparison.json", artifact)
}
fn open(path: &Path) -> Result<(ReportStore, ComparisonArtifact)> {
    let root = if path
        .file_name()
        .is_some_and(|name| name == "comparison.json")
    {
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
    } else {
        path
    };
    let store = ReportStore::safe_root(root, false)?;
    let artifact: ComparisonArtifact = store.read_json("comparison.json")?;
    validate(&artifact)?;
    Ok((store, artifact))
}
fn missing(error: &anyhow::Error) -> bool {
    error.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::NOENT)
        || error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}
pub(super) fn read_sources(
    store: &ReportStore,
    artifact: &ComparisonArtifact,
) -> Result<Vec<SourceReport>> {
    artifact
        .sources
        .iter()
        .map(|source| {
            let read = store
                .open_child(&source.path)
                .and_then(|store| store.read_bytes("report.json"));
            let bytes = match read {
                Ok(bytes) => bytes,
                Err(error) if missing(&error) && source.report_digest.is_none() => {
                    return Ok(SourceReport {
                        variant_id: source.variant_id.clone(),
                        report: None,
                        error: Some(format!(
                            "{}unfinished source {}: {error:#}",
                            source
                                .error
                                .as_ref()
                                .map(|e| format!("{e}; "))
                                .unwrap_or_default(),
                            source.path
                        )),
                    });
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("read source {}", source.path));
                }
            };
            if let Some(hash) = &source.report_digest {
                ensure!(
                    *hash == digest(&bytes),
                    "source digest mismatch: {}",
                    source.path
                );
            }
            let report = report::decode_report(&bytes)
                .with_context(|| format!("decode source {}", source.path))?;
            ensure!(
                report.run_id == run_id(source)?,
                "source run identity mismatch: {}",
                source.path
            );
            ensure!(
                Some(&report.recipe_digest) == artifact.recipe_digests.get(&source.variant_id),
                "source recipe identity mismatch: {}",
                source.path
            );
            ensure!(
                same_selection(&report.selected_profiles, &artifact.manifest.profiles)
                    && same_selection(&report.selected_scenarios, &artifact.manifest.scenarios),
                "source matrix identity mismatch: {}",
                source.path
            );
            let error = source.error.clone().or_else(|| {
                source
                    .report_digest
                    .is_none()
                    .then(|| "source has no finalized content digest".into())
            });
            Ok(SourceReport {
                variant_id: source.variant_id.clone(),
                report: Some(report),
                error,
            })
        })
        .collect()
}
pub(super) fn recompute(
    artifact: &ComparisonArtifact,
    sources: &[SourceReport],
) -> ComparisonReport {
    let mut report = derive(&artifact.manifest, sources);
    for summary in &mut report.sources {
        if let Some(reference) = artifact
            .sources
            .iter()
            .find(|r| r.variant_id == summary.variant_id)
        {
            summary.report_path = Some(format!("{}/report.json", reference.path));
        }
        // The declared digest remains available even if the source is unfinished.
        summary.recipe_digest = artifact.recipe_digests.get(&summary.variant_id).cloned();
    }
    if !artifact.finished {
        report.outcome = RunOutcome::aggregate([report.outcome, RunOutcome::Incomplete]);
        report
            .diagnostics
            .push("suite execution is unfinished".into());
    }
    if artifact.cancelled {
        report.outcome = RunOutcome::aggregate([report.outcome, RunOutcome::Cancelled]);
    }
    report
}
/// Read only pinned, contained evidence and recompute all derived verdicts offline.
pub fn load_report(path: &Path) -> Result<ComparisonReport> {
    Ok(load_document(path)?.report)
}
/// Validated declaration context alongside recomputed evidence; no recipe files needed.
#[derive(Debug, Clone)]
pub struct LoadedComparison {
    pub manifest: SuiteManifest,
    pub report: ComparisonReport,
}
/// Load declaration context and evidence through the same pinned safe store.
pub fn load_document(path: &Path) -> Result<LoadedComparison> {
    let (store, artifact) = open(path)?;
    let report = recompute(&artifact, &read_sources(&store, &artifact)?);
    Ok(LoadedComparison {
        manifest: artifact.manifest,
        report,
    })
}
/// Recover only journaled resources; report content/digests are irrelevant to cleanup.
pub fn cleanup(path: &Path) -> Result<()> {
    cleanup_with(path, &mut Docker::default())
}
pub fn cleanup_with(path: &Path, environment: &mut impl Environment) -> Result<()> {
    let (store, artifact) = open(path)?;
    // Validate and pin every journal before the first external removal.
    let mut sources = Vec::new();
    for source in &artifact.sources {
        let source_store = store.open_child(&source.path)?;
        let journal = source_store.load_journal()?;
        ensure!(
            journal.run_id == run_id(source)?,
            "journal run identity mismatch"
        );
        for resource in &journal.resources {
            ensure!(
                resource.labels.get("io.theseus.run") == Some(&journal.run_id),
                "journal run ownership mismatch"
            );
            ensure!(
                resource.labels.get("io.theseus.owner").map(String::as_str) == Some("v1")
                    && resource.labels.contains_key("io.theseus.scenario"),
                "journal resource lacks ownership labels"
            );
            ensure!(
                resource.container_id.len() == 64
                    && resource.container_id.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid journaled container ID"
            );
        }
        sources.push((source_store, journal));
    }
    let mut errors = vec![];
    for (store, mut journal) in sources {
        journal.cleanup_errors.clear();
        for resource in journal.resources.iter_mut().filter(|r| !r.removed) {
            match environment.remove(resource) {
                Ok(()) => resource.removed = true,
                Err(error) => journal.cleanup_errors.push(format!("{error:#}")),
            }
        }
        errors.extend(journal.cleanup_errors.clone());
        if let Err(error) = store.save_journal(&journal) {
            errors.push(format!(
                "save cleanup journal {}: {error:#}",
                store.root().display()
            ));
        }
    }
    ensure!(
        errors.is_empty(),
        "comparison cleanup failed: {}",
        errors.join("; ")
    );
    Ok(())
}
