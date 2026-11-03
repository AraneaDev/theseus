//! Strict retained-family readers. Saved evidence never supplies launch authority.
use super::{model::*, recipe::ValidatedNativeRecipe};
use crate::{
    engine::domain::{SelectedRecipe, StoredJournal, StoredRun},
    report,
};
use anyhow::{Result, bail, ensure};
use std::{
    io::Read,
    path::{Path, PathBuf},
};
use theseus_host::fs::{HostDir, RelativePath, RootPolicy, fingerprint};

pub fn load_recipe(path: &Path) -> Result<SelectedRecipe> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let dir = HostDir::open(parent, RootPolicy::AllComponentsNoFollow)?;
    let leaf = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("recipe requires filename"))?;
    let relative = RelativePath::parse(
        leaf.to_str()
            .ok_or_else(|| anyhow::anyhow!("recipe filename is not UTF-8"))?,
    )?;
    let mut file = dir.open_file(&relative)?;
    let before = fingerprint(&file)?;
    let canonical_parent = parent.canonicalize()?;
    let verify = |file: &std::fs::File| -> Result<()> {
        let named = HostDir::open(&canonical_parent, RootPolicy::AllComponentsNoFollow)?;
        let original = HostDir::open(parent, RootPolicy::AllComponentsNoFollow)?;
        ensure!(
            dir.same_object(&named)? && dir.same_object(&original)?,
            "recipe parent changed"
        );
        ensure!(
            before == fingerprint(file)? && before == fingerprint(&dir.open_file(&relative)?)?,
            "recipe changed during read"
        );
        Ok(())
    };
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_ARTIFACT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_ARTIFACT_BYTES, "recipe exceeds bound");
    // Validate retained source and pathname context on both sides of parsing.
    verify(&file)?;
    let parsed = decode_recipe(&bytes, canonical_parent.join(leaf));
    verify(&file)?;
    parsed
}
pub fn decode_recipe(bytes: &[u8], path: PathBuf) -> Result<SelectedRecipe> {
    // JSON is the explicit native namespace; malformed JSON cannot fall through to TOML.
    if bytes.iter().copied().find(|b| !b.is_ascii_whitespace()) == Some(b'{') {
        Ok(SelectedRecipe::Native(ValidatedNativeRecipe::decode(
            bytes, path,
        )?))
    } else {
        Ok(SelectedRecipe::Docker(crate::recipe::parse_bytes(
            &path, bytes,
        )?))
    }
}
fn root(path: &Path) -> Result<PathBuf> {
    if [
        REPORT_FILENAME,
        JOURNAL_FILENAME,
        "report.json",
        "journal.json",
    ]
    .iter()
    .any(|n| path.file_name().is_some_and(|leaf| leaf == *n))
    {
        return Ok(path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_owned());
    }
    Ok(path.to_owned())
}
pub fn load_run(path: &Path) -> Result<StoredRun> {
    ensure!(
        !path
            .file_name()
            .is_some_and(|n| n == JOURNAL_FILENAME || n == "journal.json"),
        "expected run directory or report filename"
    );
    let store = report::open_existing(&root(path)?)?;
    let run = store.load_selected_report()?;
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        ensure!(
            !matches!(
                (&run, name),
                (StoredRun::Native(_), "report.json") | (StoredRun::Docker(_), REPORT_FILENAME)
            ),
            "artifact filename/family mismatch"
        );
    }
    Ok(run)
}
pub fn load_journal(path: &Path) -> Result<StoredJournal> {
    ensure!(
        !path
            .file_name()
            .is_some_and(|n| n == REPORT_FILENAME || n == "report.json"),
        "expected run directory or journal filename"
    );
    let journal = report::open_existing(&root(path)?)?.load_selected_journal()?;
    ensure!(
        !matches!(
            (&journal, path.file_name().and_then(|n| n.to_str())),
            (StoredJournal::Native(_), Some("journal.json"))
                | (StoredJournal::Docker(_), Some(JOURNAL_FILENAME))
        ),
        "journal filename/family mismatch"
    );
    Ok(journal)
}
pub fn validate_report(report: &NativeReport) -> Result<()> {
    report.validate()?;
    ensure!(report.scenarios.len() <= 3, "too many native scenarios");
    let mut kinds = std::collections::BTreeSet::new();
    for scenario in &report.scenarios {
        ensure!(
            kinds.insert(scenario.scenario),
            "duplicate native scenario kind"
        );
    }
    for identity in &report.identities {
        match identity {
            Provenance::Observed { value, source } => {
                text(value, 4096)?;
                text(source, 4096)?;
            }
            Provenance::ProviderReported { value, provider } => {
                text(value, 4096)?;
                text(provider, 256)?;
            }
            Provenance::Unavailable { reason } => text(reason, 4096)?,
        }
    }
    for claim in &report.provider_claims {
        claim.validate()?;
    }
    for failure in &report.failures {
        text(failure, 4096)?;
    }
    for scenario in &report.scenarios {
        let d = &scenario.dispositions;
        for state in [&d.local, &d.remote, &d.release, &d.reset] {
            state.validate()?;
        }
        for stage in &scenario.stages {
            for failure in &stage.failures {
                text(failure, 4096)?;
            }
            for command in &stage.commands {
                for reference in [&command.stdout_log, &command.stderr_log]
                    .into_iter()
                    .flatten()
                {
                    ensure!(
                        reference.bytes <= crate::recipe::DEFAULT_LOG_LIMIT,
                        "native log exceeds bound"
                    );
                    let parts: Vec<_> = reference.path.split('/').collect();
                    ensure!(
                        parts.first() != Some(&"native-intents"),
                        "native intent metadata is not a command log"
                    );
                    ensure!(
                        !parts.is_empty()
                            && parts.iter().all(|p| !p.is_empty()
                                && !matches!(*p, "." | "..")
                                && !p.contains(['\\', ':', '\0'])),
                        "unsafe native log reference"
                    );
                }
            }
            for check in &stage.checks {
                for id in &check.command_ids {
                    text(id, 128)?;
                }
            }
        }
    }
    Ok(())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactFamily {
    Docker,
    Native,
    Comparison,
    Empty,
}
pub(crate) fn require_family(actual: ArtifactFamily, expected: ArtifactFamily) -> Result<()> {
    if actual != ArtifactFamily::Empty && actual != expected {
        bail!("artifact family mismatch");
    }
    Ok(())
}

/// Native envelope fields must never be accepted by permissive legacy DTOs,
/// including an overlapping object carrying both legacy and native members.
pub(crate) fn ensure_legacy_namespace(bytes: &[u8]) -> Result<()> {
    use serde::de::{self, MapAccess, Visitor};
    struct Legacy;
    impl<'de> Visitor<'de> for Legacy {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("legacy artifact object")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
            while let Some(key) = map.next_key::<String>()? {
                if key == "kind" {
                    let value = map.next_value::<serde_json::Value>()?;
                    if value
                        .as_str()
                        .is_some_and(|s| s.starts_with("theseus_native_"))
                    {
                        return Err(de::Error::custom(
                            "native namespace under legacy artifact filename",
                        ));
                    }
                } else {
                    let _ = map.next_value::<de::IgnoredAny>()?;
                }
            }
            Ok(())
        }
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    serde::de::Deserializer::deserialize_map(&mut decoder, Legacy)?;
    decoder.end()?;
    Ok(())
}
