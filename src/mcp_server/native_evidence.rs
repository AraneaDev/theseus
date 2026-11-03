//! Native saved evidence uses native records and genuine OS status domains.
use super::{
    budget::{PageCandidate, fit_page},
    cursor::CursorRecord,
    evidence::error,
    range_writer::RangeWriter,
    types::*,
};
use crate::{native::model::*, report::ReportStore};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
#[derive(Debug)]
pub(super) struct Snapshot {
    pub revision: String,
    pub report: NativeReport,
    pub sampled_hint: u64,
}
pub(super) fn load(store: &ReportStore, hint: u64) -> Result<Snapshot, QueryError> {
    crate::native::storage::require_family(
        store
            .artifact_family()
            .map_err(|e| QueryError::new(ErrorCode::EvidenceInvalid, e))?,
        crate::native::storage::ArtifactFamily::Native,
    )
    .map_err(|e| QueryError::new(ErrorCode::EvidenceInvalid, e))?;
    let bytes = store
        .read_bytes(crate::native::model::REPORT_FILENAME)
        .map_err(|e| QueryError::new(ErrorCode::IoError, e))?;
    let report =
        NativeReport::decode(&bytes).map_err(|e| QueryError::new(ErrorCode::EvidenceInvalid, e))?;
    crate::native::storage::validate_report(&report)
        .map_err(|e| QueryError::new(ErrorCode::EvidenceInvalid, e))?;
    Ok(Snapshot {
        revision: format!("{:x}", Sha256::digest(&bytes)),
        report,
        sampled_hint: hint,
    })
}
#[derive(Serialize)]
#[serde(untagged)]
enum Detail<'a> {
    Header {
        kind: &'a str,
        schema: u32,
        run_id: &'a str,
        recipe_digest: &'a str,
        outcome: crate::model::RunOutcome,
        identities: &'a [Provenance<String>],
        provider_claims: &'a [ProviderReceipt],
    },
    Scenario {
        id: &'a str,
        scenario: crate::model::ScenarioKind,
        ownership_id: &'a str,
        outcome: crate::model::RunOutcome,
        dispositions: &'a NativeDispositions,
    },
    Stage {
        name: &'a str,
        step: Option<crate::model::StepName>,
        checkpoint: Option<crate::model::Checkpoint>,
        outcome: crate::model::StageOutcome,
    },
    Command(&'a NativeCommandEvidence),
    Check(&'a NativeCheckEvidence),
    File(&'a NativeFileObservation),
    Error(&'a str),
}
struct Record<'a> {
    id: String,
    kind: RecordKind,
    detail: Detail<'a>,
}
fn records(report: &NativeReport) -> impl Iterator<Item = Record<'_>> {
    let header = std::iter::once(Record {
        id: "run".into(),
        kind: RecordKind::RunHeader,
        detail: Detail::Header {
            kind: &report.kind,
            schema: report.schema,
            run_id: &report.run_id,
            recipe_digest: &report.recipe_digest,
            outcome: report.outcome,
            identities: &report.identities,
            provider_claims: &report.provider_claims,
        },
    });
    let errors = report.failures.iter().enumerate().map(|(i, e)| Record {
        id: format!("error/{i}"),
        kind: RecordKind::Error,
        detail: Detail::Error(e),
    });
    let scenarios = report
        .scenarios
        .iter()
        .enumerate()
        .flat_map(|(s, scenario)| {
            let one = std::iter::once(Record {
                id: format!("s/{s}"),
                kind: RecordKind::ScenarioHeader,
                detail: Detail::Scenario {
                    id: &scenario.id,
                    scenario: scenario.scenario,
                    ownership_id: &scenario.ownership_id,
                    outcome: scenario.outcome,
                    dispositions: &scenario.dispositions,
                },
            });
            let stages = scenario
                .stages
                .iter()
                .enumerate()
                .flat_map(move |(t, stage)| {
                    let prefix = format!("s/{s}/stage/{t}");
                    let one = std::iter::once(Record {
                        id: prefix.clone(),
                        kind: RecordKind::StageHeader,
                        detail: Detail::Stage {
                            name: &stage.name,
                            step: stage.step,
                            checkpoint: stage.checkpoint,
                            outcome: stage.outcome,
                        },
                    });
                    let commands = stage.commands.iter().enumerate().map(move |(i, c)| Record {
                        id: format!("s/{s}/stage/{t}/command/{i}"),
                        kind: RecordKind::Command,
                        detail: Detail::Command(c),
                    });
                    let checks = stage.checks.iter().enumerate().map(move |(i, c)| Record {
                        id: format!("s/{s}/stage/{t}/check/{i}"),
                        kind: RecordKind::Check,
                        detail: Detail::Check(c),
                    });
                    let files = stage
                        .native_files
                        .iter()
                        .enumerate()
                        .map(move |(i, c)| Record {
                            id: format!("s/{s}/stage/{t}/native_file/{i}"),
                            kind: RecordKind::NativeFile,
                            detail: Detail::File(c),
                        });
                    let errors = stage.failures.iter().enumerate().map(move |(i, c)| Record {
                        id: format!("s/{s}/stage/{t}/error/{i}"),
                        kind: RecordKind::Error,
                        detail: Detail::Error(c),
                    });
                    one.chain(commands).chain(checks).chain(files).chain(errors)
                });
            one.chain(stages)
        });
    header.chain(errors).chain(scenarios)
}
fn select(report: &NativeReport, selector: &EvidenceSelector) -> Result<String, QueryError> {
    Ok(match selector {
        EvidenceSelector::Root(s) => match s.collection {
            RootCollection::Run => "run".into(),
            RootCollection::Scenarios => "s/".into(),
            RootCollection::Errors => "error/".into(),
            _ => return Err(error(ErrorCode::InvalidArguments)),
        },
        EvidenceSelector::Scenario(s) => {
            let i = report
                .scenarios
                .iter()
                .position(|r| r.id == s.scenario_id)
                .ok_or_else(|| error(ErrorCode::UnknownRecord))?;
            if !matches!(s.collection, ScenarioCollection::Stages) {
                return Err(error(ErrorCode::InvalidArguments));
            }
            format!("s/{i}/stage/")
        }
        EvidenceSelector::Stage(s) => {
            let i = report
                .scenarios
                .iter()
                .position(|r| r.id == s.scenario_id)
                .ok_or_else(|| error(ErrorCode::UnknownRecord))?;
            let t = s.stage_index.get();
            if usize::try_from(t)
                .ok()
                .and_then(|t| report.scenarios[i].stages.get(t))
                .is_none()
            {
                return Err(error(ErrorCode::UnknownRecord));
            }
            let name = match s.collection {
                StageCollection::Commands => "command",
                StageCollection::Checks => "check",
                StageCollection::NativeFiles => "native_file",
                StageCollection::StageErrors => "error",
                _ => return Err(error(ErrorCode::InvalidArguments)),
            };
            format!("s/{i}/stage/{t}/{name}/")
        }
        _ => return Err(error(ErrorCode::InvalidArguments)),
    })
}
fn selected(id: &str, prefix: &str) -> bool {
    if prefix == "run" {
        id == prefix
    } else {
        id.strip_prefix(prefix)
            .is_some_and(|tail| !tail.contains('/'))
    }
}
pub(super) fn evidence(
    store: &ReportStore,
    snapshot: &Snapshot,
    args: &EvidenceArgs,
    reply: &ReplyContext,
) -> Result<EvidenceData, QueryError> {
    ToolCall::RunEvidence(args.clone())
        .validate()
        .map_err(|_| error(ErrorCode::InvalidArguments))?;
    if args
        .revision
        .as_ref()
        .is_some_and(|r| r.as_str() != snapshot.revision)
    {
        return Err(error(ErrorCode::RevisionChanged));
    }
    let revision =
        Revision::parse(&snapshot.revision).map_err(|_| error(ErrorCode::InternalError))?;
    let mut cursor = CursorRecord::evidence(args, &revision).map_err(error)?;
    let decoded = args
        .cursor
        .as_ref()
        .map(|c| CursorRecord::decode(c, &reply.instance))
        .transpose()
        .map_err(error)?;
    let data = match &args.selector {
        EvidenceSelector::Record(s) => {
            let record = records(&snapshot.report)
                .find(|r| r.id == s.record_id.as_str())
                .ok_or_else(|| error(ErrorCode::UnknownRecord))?;
            let mut counter = RangeWriter::new(0, 0);
            serde_json::to_writer(&mut counter, &record.detail)
                .map_err(|e| QueryError::new(ErrorCode::InternalError, e.into()))?;
            let total = counter.total_bytes();
            if let Some(c) = decoded {
                c.validate(&cursor, total).map_err(error)?;
                cursor = c;
            }
            let mut writer =
                RangeWriter::new(cursor.next_position, usize::from(cursor.requested_limit));
            serde_json::to_writer(&mut writer, &record.detail)
                .map_err(|e| QueryError::new(ErrorCode::InternalError, e.into()))?;
            let bytes = writer.bytes().to_vec();
            let more = cursor.next_position + (bytes.len() as u64) < total;
            (
                EvidenceData::Record(RecordPage {
                    kind: RecordSelectorKind::Record,
                    revision,
                    record_id: s.record_id.clone(),
                    record_kind: record.kind,
                    encoding: Encoding::JsonUtf8,
                    offset: UInt::new(cursor.next_position)
                        .map_err(|_| error(ErrorCode::InvalidCursor))?,
                    bytes,
                    total_bytes: UInt::new(total)
                        .map_err(|_| error(ErrorCode::ResponseTooLarge))?,
                    next_cursor: None,
                }),
                more,
            )
        }
        EvidenceSelector::Log(selector) => {
            let command = snapshot
                .report
                .scenarios
                .iter()
                .flat_map(|s| s.stages.iter())
                .flat_map(|s| s.commands.iter())
                .find(|c| c.operation_id == selector.command_id)
                .ok_or_else(|| error(ErrorCode::UnknownRecord))?;
            let reference = match selector.stream {
                Stream::Stdout => command.stdout_log.as_ref(),
                Stream::Stderr => command.stderr_log.as_ref(),
            }
            .ok_or_else(|| error(ErrorCode::UnknownRecord))?;
            let probe = store.read_log_page(reference, 0, 1).map_err(|e| {
                QueryError::new(
                    if decoded.is_some() {
                        ErrorCode::LogChanged
                    } else {
                        ErrorCode::IoError
                    },
                    e,
                )
            })?;
            cursor = cursor
                .with_log_fingerprint(&probe.fingerprint.canonical_bytes())
                .map_err(error)?;
            if let Some(c) = decoded {
                c.validate(&cursor, probe.readable_total).map_err(error)?;
                cursor = c;
            }
            let page = store
                .read_log_page(
                    reference,
                    cursor.next_position,
                    usize::from(cursor.requested_limit),
                )
                .map_err(|e| QueryError::new(ErrorCode::LogChanged, e))?;
            if page.fingerprint != probe.fingerprint {
                return Err(error(ErrorCode::LogChanged));
            }
            let more = page.next_offset.is_some();
            let uint = |n| UInt::new(n).map_err(|_| error(ErrorCode::ResponseTooLarge));
            (
                EvidenceData::Log(LogPage {
                    kind: LogKind::Log,
                    revision,
                    command_id: selector.command_id.clone(),
                    stream: selector.stream,
                    offset: uint(page.offset)?,
                    bytes: page.bytes,
                    recorded_bytes: uint(page.recorded_bytes)?,
                    current_bytes: uint(page.current_bytes)?,
                    readable_total: uint(page.readable_total)?,
                    capture_truncated: reference.truncated,
                    change_token: cursor.change_token(),
                    next_cursor: None,
                }),
                more,
            )
        }
        selector => {
            let prefix = select(&snapshot.report, selector)?;
            let total = records(&snapshot.report)
                .filter(|r| selected(&r.id, &prefix))
                .count() as u64;
            if let Some(c) = decoded {
                c.validate(&cursor, total).map_err(error)?;
                cursor = c;
            }
            let summaries = records(&snapshot.report)
                .filter(|r| selected(&r.id, &prefix))
                .skip(cursor.next_position as usize)
                .take(usize::from(cursor.requested_limit))
                .map(|r| {
                    Ok(RecordSummary {
                        record_id: RecordId::parse(&r.id)
                            .map_err(|_| error(ErrorCode::InternalError))?,
                        record_kind: r.kind,
                        source_id: None,
                        summary: ShortPreview::new(&r.id, false),
                        outcome: None,
                    })
                })
                .collect::<Result<Vec<_>, QueryError>>()?;
            let more = cursor.next_position + (summaries.len() as u64) < total;
            (
                EvidenceData::Catalog(CatalogPage {
                    kind: CatalogKind::Catalog,
                    revision,
                    records: summaries,
                    next_cursor: None,
                }),
                more,
            )
        }
    };
    match fit_page(
        reply,
        PageCandidate {
            data: ToolData::RunEvidence(data.0),
            cursor,
            more_after_candidate: data.1,
        },
    )
    .map_err(|f| error(f.error.code))?
    {
        ToolData::RunEvidence(data) => Ok(data),
        _ => Err(error(ErrorCode::InternalError)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::*, report};
    fn report() -> NativeReport {
        NativeReport {
            kind: "theseus_native_report".into(),
            schema: 1,
            run_id: "native-run".into(),
            recipe_digest: "a".repeat(64),
            scenarios: vec![NativeScenario {
                id: "current-clean".into(),
                scenario: ScenarioKind::Clean,
                ownership_id: "owner".into(),
                outcome: RunOutcome::Incomplete,
                dispositions: NativeDispositions {
                    local: Disposition::Unknown {
                        reason: "prior child join unproved".into(),
                    },
                    remote: Disposition::Complete {
                        receipt: "remote query only".into(),
                    },
                    release: Disposition::NotStarted,
                    reset: Disposition::NotStarted,
                },
                stages: vec![NativeStage {
                    name: "capture".into(),
                    step: None,
                    checkpoint: Some(Checkpoint::TargetInstalled),
                    outcome: StageOutcome::Incomplete,
                    commands: vec![],
                    checks: vec![],
                    native_files: vec![NativeFileObservation {
                        request_id: "inspection".into(),
                        root_identity: "original-prefix".into(),
                        leaf: "reader".into(),
                        state: NativeFileState::Unknown {
                            reason: "mapping unproved".into(),
                        },
                    }],
                    failures: vec![],
                }],
            }],
            identities: vec![Provenance::Unavailable {
                reason: "child uid unobserved".into(),
            }],
            provider_claims: vec![],
            failures: vec![],
            outcome: RunOutcome::Incomplete,
        }
    }
    fn reply() -> ReplyContext {
        ReplyContext {
            era: Era::Legacy,
            id: RpcId::Integer(1),
            instance: Instance::parse(&"a".repeat(32)).unwrap(),
        }
    }
    #[test]
    fn native_files_are_explicit_and_snapshot_selector_refuses() {
        let temp = tempfile::tempdir().unwrap();
        let store = report::create(&temp.path().join("run")).unwrap();
        store.save_native_report(&report()).unwrap();
        let snapshot = load(&store, 9).unwrap();
        let mut args:EvidenceArgs=serde_json::from_value(serde_json::json!({"server_instance":"a".repeat(32),"handle":format!("{}:1","a".repeat(32)),"selector":{"kind":"catalog","collection":"native_files","scenario_id":"current-clean","stage_index":0},"limit":1})).unwrap();
        let EvidenceData::Catalog(page) = evidence(&store, &snapshot, &args, &reply()).unwrap()
        else {
            panic!()
        };
        assert_eq!(page.records.len(), 1);
        assert_eq!(
            page.records[0].record_id.as_str(),
            "s/0/stage/0/native_file/0"
        );
        args.selector = EvidenceSelector::Record(RecordSelector {
            kind: RecordSelectorKind::Record,
            record_id: page.records[0].record_id.clone(),
        });
        args.revision = Some(Revision::parse(&snapshot.revision).unwrap());
        args.limit = Some(UInt::new(4096).unwrap());
        let EvidenceData::Record(page) = evidence(&store, &snapshot, &args, &reply()).unwrap()
        else {
            panic!()
        };
        let value: serde_json::Value = serde_json::from_slice(&page.bytes).unwrap();
        assert_eq!(value["state"]["state"], "unknown");
        assert!(value.get("uid").is_none());
        args.selector=serde_json::from_value(serde_json::json!({"kind":"catalog","collection":"snapshots","scenario_id":"current-clean","stage_index":0})).unwrap();
        args.limit = Some(UInt::new(1).unwrap());
        assert!(evidence(&store, &snapshot, &args, &reply()).is_err());
        let projected =
            super::super::status_projection::project_native_snapshot(&snapshot).unwrap();
        assert_eq!(projected.guest, GuestCleanup::Unknown);
        assert_eq!(projected.sampled_hint, 9);
    }
    #[test]
    fn native_revision_hashes_exact_saved_bytes_and_mixed_families_fail() {
        let temp = tempfile::tempdir().unwrap();
        let store = report::create(&temp.path().join("run")).unwrap();
        store.save_native_report(&report()).unwrap();
        let first = load(&store, 0).unwrap();
        let mut bytes = std::fs::read(store.native_report_path()).unwrap();
        bytes.push(b'\n');
        std::fs::write(store.native_report_path(), &bytes).unwrap();
        assert_ne!(first.revision, load(&store, 0).unwrap().revision);
        std::fs::write(store.report_path(), b"{}").unwrap();
        assert!(load(&store, 0).is_err());
    }
}
