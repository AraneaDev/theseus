//! Borrowed report record serialization and checked structural traversal.
use super::{evidence::error, types::*};
use crate::{model::*, snapshot::*};
use serde::{Serialize, Serializer, ser::SerializeMap};

pub(super) struct Record<'a> {
    pub id: String,
    pub detail: Detail<'a>,
}
pub(super) enum Detail<'a> {
    Run(&'a RunReport),
    Scenario(&'a ScenarioReport),
    Stage(&'a StageReport),
    Command(&'a CommandRecord),
    Check(&'a CheckResult),
    Finding(&'a Finding),
    Artifact(&'a ArtifactDigest),
    ArtifactFile(&'a ArtifactFileDigest),
    Snapshot(&'a Snapshot),
    Entry(&'a str, &'a FileEntry),
    Baseline(&'a FileChange),
    Change(&'a FileChange),
    Error(&'a str),
}
impl Detail<'_> {
    pub fn kind(&self) -> RecordKind {
        match self {
            Self::Run(_) => RecordKind::RunHeader,
            Self::Scenario(_) => RecordKind::ScenarioHeader,
            Self::Stage(_) => RecordKind::StageHeader,
            Self::Command(_) => RecordKind::Command,
            Self::Check(_) => RecordKind::Check,
            Self::Finding(_) => RecordKind::Finding,
            Self::Artifact(_) => RecordKind::ArtifactHeader,
            Self::ArtifactFile(_) => RecordKind::ArtifactFile,
            Self::Snapshot(_) => RecordKind::SnapshotHeader,
            Self::Entry(..) => RecordKind::SnapshotEntry,
            Self::Baseline(_) => RecordKind::BaselineChange,
            Self::Change(_) => RecordKind::StageChange,
            Self::Error(_) => RecordKind::Error,
        }
    }
}
impl Record<'_> {
    pub fn summary(&self) -> Result<RecordSummary, QueryError> {
        let (source, description, outcome) = match &self.detail {
            Detail::Run(r) => (
                Some(r.run_id.as_str()),
                r.run_id.as_str(),
                Some(r.outcome.to_string()),
            ),
            Detail::Scenario(r) => (
                Some(r.id.as_str()),
                r.id.as_str(),
                Some(r.outcome.to_string()),
            ),
            Detail::Stage(r) => (
                Some(r.name.as_str()),
                r.name.as_str(),
                Some(r.outcome.to_string()),
            ),
            Detail::Command(r) => (
                Some(r.id.as_str()),
                r.phase.as_str(),
                Some(r.outcome.to_string()),
            ),
            Detail::Check(r) => (
                Some(r.id.as_str()),
                r.message.as_str(),
                Some(r.outcome.to_string()),
            ),
            Detail::Finding(r) => (None, r.message.as_str(), None),
            Detail::Artifact(r) => (Some(r.name.as_str()), r.name.as_str(), None),
            Detail::ArtifactFile(r) => (Some(r.path.as_str()), r.path.as_str(), None),
            Detail::Snapshot(r) => (Some(r.root.as_str()), r.root.as_str(), None),
            Detail::Entry(path, _) => (Some(*path), *path, None),
            Detail::Baseline(r) | Detail::Change(r) => {
                (Some(r.path.as_str()), r.path.as_str(), None)
            }
            Detail::Error(message) => (None, *message, None),
        };
        Ok(RecordSummary {
            record_id: RecordId::parse(&self.id).map_err(|_| error(ErrorCode::InternalError))?,
            record_kind: self.detail.kind(),
            source_id: source.map(|s| ShortPreview::new(s, false)),
            summary: ShortPreview::new(description, false),
            outcome,
        })
    }
}

// Each variant retains only a model borrow. Catalog size never determines an
// intermediate allocation; callers ask for at most one bounded page of summaries.
pub(super) enum Catalog<'a> {
    Run(&'a RunReport),
    Scenarios(&'a [ScenarioReport]),
    Artifacts(&'a [ArtifactDigest]),
    Errors(String, &'a [String]),
    Stages(usize, &'a [StageReport]),
    Findings(usize, &'a [Finding]),
    Commands(String, &'a [CommandRecord]),
    Checks(String, &'a [CheckResult]),
    Snapshots(String, &'a [Snapshot]),
    Baselines(String, &'a [FileChange]),
    Changes(String, &'a [FileChange]),
    Files(usize, &'a [ArtifactFileDigest]),
    Entries(String, &'a std::collections::BTreeMap<String, FileEntry>),
}
impl<'a> Catalog<'a> {
    pub fn select(report: &'a RunReport, selector: &EvidenceSelector) -> Result<Self, QueryError> {
        Ok(match selector {
            EvidenceSelector::Root(s) => match s.collection {
                RootCollection::Run => Self::Run(report),
                RootCollection::Scenarios => Self::Scenarios(&report.scenarios),
                RootCollection::Artifacts => Self::Artifacts(&report.artifacts),
                RootCollection::Errors => Self::Errors(String::new(), &report.errors),
            },
            EvidenceSelector::Scenario(s) => {
                let (index, scenario) = scenario(report, &s.scenario_id)?;
                match s.collection {
                    ScenarioCollection::Stages => Self::Stages(index, &scenario.stages),
                    ScenarioCollection::Findings => Self::Findings(index, &scenario.findings),
                    ScenarioCollection::ScenarioErrors => {
                        Self::Errors(format!("s/{index}/"), &scenario.errors)
                    }
                }
            }
            EvidenceSelector::Stage(s) => {
                let (index, scenario) = scenario(report, &s.scenario_id)?;
                let ti = index_of(s.stage_index.get())?;
                let stage = get(&scenario.stages, ti)?;
                let prefix = format!("s/{index}/stage/{ti}");
                match s.collection {
                    StageCollection::NativeFiles => return Err(error(ErrorCode::InvalidArguments)),
                    StageCollection::Commands => Self::Commands(prefix, &stage.commands),
                    StageCollection::Checks => Self::Checks(prefix, &stage.checks),
                    StageCollection::Snapshots => Self::Snapshots(prefix, &stage.snapshots),
                    StageCollection::BaselineChanges => {
                        Self::Baselines(prefix, &stage.baseline_diff)
                    }
                    StageCollection::StageChanges => Self::Changes(prefix, &stage.stage_diff),
                    StageCollection::StageErrors => {
                        Self::Errors(format!("{prefix}/"), &stage.errors)
                    }
                }
            }
            EvidenceSelector::Artifact(s) => {
                let index = index_of(s.artifact_index.get())?;
                Self::Files(index, &get(&report.artifacts, index)?.files)
            }
            EvidenceSelector::Snapshot(s) => {
                let (si, scenario) = scenario(report, &s.scenario_id)?;
                let ti = index_of(s.stage_index.get())?;
                let ni = index_of(s.snapshot_index.get())?;
                let snapshot = get(&get(&scenario.stages, ti)?.snapshots, ni)?;
                Self::Entries(
                    format!("s/{si}/stage/{ti}/snapshot/{ni}"),
                    &snapshot.entries,
                )
            }
            _ => return Err(error(ErrorCode::InvalidArguments)),
        })
    }
    pub fn len(&self) -> usize {
        match self {
            Self::Run(_) => 1,
            Self::Scenarios(v) => v.len(),
            Self::Artifacts(v) => v.len(),
            Self::Errors(_, v) => v.len(),
            Self::Stages(_, v) => v.len(),
            Self::Findings(_, v) => v.len(),
            Self::Commands(_, v) => v.len(),
            Self::Checks(_, v) => v.len(),
            Self::Snapshots(_, v) => v.len(),
            Self::Baselines(_, v) | Self::Changes(_, v) => v.len(),
            Self::Files(_, v) => v.len(),
            Self::Entries(_, v) => v.len(),
        }
    }
    pub fn record(&self, index: usize) -> Result<Record<'a>, QueryError> {
        let (id, detail) = match self {
            Self::Run(r) if index == 0 => ("run".into(), Detail::Run(r)),
            Self::Run(_) => return Err(error(ErrorCode::UnknownRecord)),
            Self::Scenarios(v) => (format!("s/{index}"), Detail::Scenario(get(v, index)?)),
            Self::Artifacts(v) => (
                format!("artifact/{index}"),
                Detail::Artifact(get(v, index)?),
            ),
            Self::Errors(prefix, v) => (
                format!("{prefix}error/{index}"),
                Detail::Error(get(v, index)?),
            ),
            Self::Stages(si, v) => (
                format!("s/{si}/stage/{index}"),
                Detail::Stage(get(v, index)?),
            ),
            Self::Findings(si, v) => (
                format!("s/{si}/finding/{index}"),
                Detail::Finding(get(v, index)?),
            ),
            Self::Commands(prefix, v) => (
                format!("{prefix}/command/{index}"),
                Detail::Command(get(v, index)?),
            ),
            Self::Checks(prefix, v) => (
                format!("{prefix}/check/{index}"),
                Detail::Check(get(v, index)?),
            ),
            Self::Snapshots(prefix, v) => (
                format!("{prefix}/snapshot/{index}"),
                Detail::Snapshot(get(v, index)?),
            ),
            Self::Baselines(prefix, v) => (
                format!("{prefix}/baseline/{index}"),
                Detail::Baseline(get(v, index)?),
            ),
            Self::Changes(prefix, v) => (
                format!("{prefix}/change/{index}"),
                Detail::Change(get(v, index)?),
            ),
            Self::Files(ai, v) => (
                format!("artifact/{ai}/file/{index}"),
                Detail::ArtifactFile(get(v, index)?),
            ),
            Self::Entries(prefix, v) => {
                let (path, entry) = v
                    .iter()
                    .nth(index)
                    .ok_or_else(|| error(ErrorCode::UnknownRecord))?;
                (
                    format!("{prefix}/entry/{index}"),
                    Detail::Entry(path, entry),
                )
            }
        };
        Ok(Record { id, detail })
    }
}
fn get<T>(values: &[T], index: usize) -> Result<&T, QueryError> {
    values
        .get(index)
        .ok_or_else(|| error(ErrorCode::UnknownRecord))
}
fn index_of(n: u64) -> Result<usize, QueryError> {
    usize::try_from(n).map_err(|_| error(ErrorCode::UnknownRecord))
}
fn number(s: &str) -> Result<usize, QueryError> {
    if s.is_empty() || (s.len() > 1 && s.starts_with('0')) || !s.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(error(ErrorCode::UnknownRecord));
    }
    s.parse().map_err(|_| error(ErrorCode::UnknownRecord))
}
pub(super) fn scenario<'a>(
    report: &'a RunReport,
    id: &str,
) -> Result<(usize, &'a ScenarioReport), QueryError> {
    let mut found = None;
    for (index, scenario) in report
        .scenarios
        .iter()
        .enumerate()
        .filter(|(_, s)| s.id == id)
    {
        if found.is_some() {
            return Err(error(ErrorCode::AmbiguousRecord));
        }
        found = Some((index, scenario));
    }
    found.ok_or_else(|| error(ErrorCode::UnknownRecord))
}
pub(super) fn command<'a>(
    report: &'a RunReport,
    id: &str,
) -> Result<&'a CommandRecord, QueryError> {
    let mut found = None;
    for command in report
        .scenarios
        .iter()
        .flat_map(|s| &s.stages)
        .flat_map(|s| &s.commands)
        .filter(|c| c.id == id)
    {
        if found.is_some() {
            return Err(error(ErrorCode::AmbiguousRecord));
        }
        found = Some(command);
    }
    found.ok_or_else(|| error(ErrorCode::UnknownRecord))
}
pub(super) fn record<'a>(report: &'a RunReport, id: &RecordId) -> Result<Record<'a>, QueryError> {
    let parts: Vec<_> = id.as_str().split('/').collect();
    let detail = match parts.as_slice() {
        ["run"] => Detail::Run(report),
        ["error", i] => Detail::Error(get(&report.errors, number(i)?)?),
        ["artifact", a] => Detail::Artifact(get(&report.artifacts, number(a)?)?),
        ["artifact", a, "file", i] => {
            Detail::ArtifactFile(get(&get(&report.artifacts, number(a)?)?.files, number(i)?)?)
        }
        ["s", s] => Detail::Scenario(get(&report.scenarios, number(s)?)?),
        ["s", s, "finding", i] => Detail::Finding(get(
            &get(&report.scenarios, number(s)?)?.findings,
            number(i)?,
        )?),
        ["s", s, "error", i] => Detail::Error(get(
            &get(&report.scenarios, number(s)?)?.errors,
            number(i)?,
        )?),
        ["s", s, "stage", t] => Detail::Stage(get(
            &get(&report.scenarios, number(s)?)?.stages,
            number(t)?,
        )?),
        ["s", s, "stage", t, kind, i] => {
            let stage = get(&get(&report.scenarios, number(s)?)?.stages, number(t)?)?;
            let index = number(i)?;
            match *kind {
                "command" => Detail::Command(get(&stage.commands, index)?),
                "check" => Detail::Check(get(&stage.checks, index)?),
                "snapshot" => Detail::Snapshot(get(&stage.snapshots, index)?),
                "baseline" => Detail::Baseline(get(&stage.baseline_diff, index)?),
                "change" => Detail::Change(get(&stage.stage_diff, index)?),
                "error" => Detail::Error(get(&stage.errors, index)?),
                _ => return Err(error(ErrorCode::UnknownRecord)),
            }
        }
        ["s", s, "stage", t, "snapshot", n, "entry", i] => {
            let snapshot = get(
                &get(&get(&report.scenarios, number(s)?)?.stages, number(t)?)?.snapshots,
                number(n)?,
            )?;
            let (path, entry) = snapshot
                .entries
                .iter()
                .nth(number(i)?)
                .ok_or_else(|| error(ErrorCode::UnknownRecord))?;
            Detail::Entry(path, entry)
        }
        _ => return Err(error(ErrorCode::UnknownRecord)),
    };
    Ok(Record {
        id: id.as_str().into(),
        detail,
    })
}

impl Serialize for Detail<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // Direct model records preserve every optional proof and exact numeric token.
        match self {
            Self::Command(v) => return v.serialize(s),
            Self::Check(v) => return v.serialize(s),
            Self::Finding(v) => return v.serialize(s),
            Self::ArtifactFile(v) => return v.serialize(s),
            Self::Baseline(v) | Self::Change(v) => return v.serialize(s),
            _ => {}
        }
        let mut m = s.serialize_map(None)?;
        macro_rules! field {
            ($name:literal,$value:expr) => {
                m.serialize_entry($name, $value)?
            };
        }
        macro_rules! optional {
            ($name:literal,$value:expr) => {
                if let Some(value) = $value {
                    m.serialize_entry($name, value)?;
                }
            };
        }
        match self {
            Self::Run(v) => {
                field!("schema_version", &v.schema_version);
                field!("run_id", &v.run_id);
                field!("recipe_path", &v.recipe_path);
                field!("recipe_digest", &v.recipe_digest);
                field!("image", &v.image);
                field!("image_id", &v.image_id);
                field!("network", &v.network);
                field!("selected_profiles", &v.selected_profiles);
                field!("selected_scenarios", &v.selected_scenarios);
                field!("started_at", &v.started_at);
                field!("finished_at", &v.finished_at);
                field!("outcome", &v.outcome);
                field!("cancelled", &v.cancelled);
                field!("scenario_count", &v.scenarios.len());
                field!("artifact_count", &v.artifacts.len());
                field!("error_count", &v.errors.len());
            }
            Self::Scenario(v) => {
                optional!("mcp", &v.mcp);
                optional!("plugin", &v.plugin);
                field!("id", &v.id);
                field!("profile", &v.profile);
                field!("scenario", &v.scenario);
                optional!("downgrade", &v.downgrade);
                optional!("rollback", &v.rollback);
                optional!("interrupted_rollback", &v.interrupted_rollback);
                field!("outcome", &v.outcome);
                field!("container_id", &v.container_id);
                field!("teardown", &v.teardown);
                field!("stage_count", &v.stages.len());
                field!("finding_count", &v.findings.len());
                field!("error_count", &v.errors.len());
            }
            Self::Stage(v) => {
                field!("name", &v.name);
                field!("step", &v.step);
                field!("checkpoint", &v.checkpoint);
                field!("outcome", &v.outcome);
                field!("skip_reason", &v.skip_reason);
                optional!("restart", &v.restart);
                field!("command_count", &v.commands.len());
                field!("check_count", &v.checks.len());
                field!("snapshot_count", &v.snapshots.len());
                field!("baseline_diff_count", &v.baseline_diff.len());
                field!("stage_diff_count", &v.stage_diff.len());
                field!("error_count", &v.errors.len());
            }
            Self::Artifact(v) => {
                field!("name", &v.name);
                field!("sha256", &v.sha256);
                field!("file_count", &v.files.len());
            }
            Self::Snapshot(v) => {
                field!("root", &v.root);
                field!("completeness", &v.completeness);
                field!("entry_count", &v.entries.len());
            }
            Self::Entry(path, entry) => {
                field!("path", path);
                field!("entry", entry);
            }
            Self::Error(message) => field!("message", message),
            _ => unreachable!("direct model records serialized above"),
        }
        m.end()
    }
}
