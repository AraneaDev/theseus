use super::*;
use crate::{model::*, report};
use sha2::Sha256;

fn fixture(bytes: &[u8]) -> (tempfile::TempDir, ReportStore) {
    let temp = tempfile::tempdir().unwrap();
    let store = report::create(&temp.path().join("run")).unwrap();
    std::fs::write(store.report_path(), bytes).unwrap();
    (temp, store)
}
fn reply() -> ReplyContext {
    ReplyContext {
        era: Era::Legacy,
        id: RpcId::String("evidence".into()),
        instance: Instance::parse(&"a".repeat(32)).unwrap(),
    }
}
fn args(selector: serde_json::Value, revision: Option<&str>, limit: u64) -> EvidenceArgs {
    serde_json::from_value(serde_json::json!({"server_instance":"a".repeat(32),"handle":format!("{}:1","a".repeat(32)),"selector":selector,"limit":limit,"revision":revision.unwrap_or(&"b".repeat(64))})).unwrap()
}
fn catalog(store: &ReportStore, snap: &ReportSnapshot, selector: serde_json::Value) -> CatalogPage {
    let mut a = args(selector, Some(&snap.revision), 32);
    a.revision = None;
    match evidence(store, snap, &a, &reply()).unwrap() {
        EvidenceData::Catalog(p) => p,
        _ => panic!("catalog required"),
    }
}
fn detail(store: &ReportStore, snap: &ReportSnapshot, id: &str, limit: u64) -> Vec<u8> {
    let mut a = args(
        serde_json::json!({"kind":"record","record_id":id}),
        Some(&snap.revision),
        limit,
    );
    let mut bytes = Vec::new();
    loop {
        match evidence(store, snap, &a, &reply()).unwrap() {
            EvidenceData::Record(p) => {
                assert_eq!(p.offset.get(), bytes.len() as u64);
                assert!(!p.bytes.is_empty() || p.next_cursor.is_none());
                bytes.extend_from_slice(&p.bytes);
                a.cursor = p.next_cursor;
                if a.cursor.is_none() {
                    assert_eq!(p.total_bytes.get(), bytes.len() as u64);
                    return bytes;
                }
            }
            _ => panic!("record required"),
        }
    }
}
#[test]
fn snapshots_preserve_existing_reader_semantics_and_hash_exact_bytes_for_all_schemas() {
    for version in 1..=8 {
        let bytes = if version <= 6 {
            std::fs::read(format!(
                "tests/fixtures/reports/schema{version}-report.json"
            ))
            .unwrap()
        } else {
            serde_json::to_vec(&proof_report(version)).unwrap()
        };
        let (_tmp, store) = fixture(&bytes);
        let expected = report::load(store.root()).unwrap();
        let snap = load_snapshot(&store, 17).unwrap();
        assert_eq!(snap.revision, format!("{:x}", Sha256::digest(&bytes)));
        assert_eq!(snap.sampled_hint, 17);
        assert_eq!(
            serde_json::to_value(&snap.report).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        let mut changed = bytes;
        changed.push(b'\n');
        std::fs::write(store.report_path(), &changed).unwrap();
        let later = load_snapshot(&store, 17).unwrap();
        assert_ne!(snap.revision, later.revision);
    }
}

#[test]
fn stored_declared_outcome_is_preserved_only_for_legacy_report_schemas() {
    let original: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/reports/schema1-report.json"
    ))
    .unwrap();
    for version in 1..=8 {
        let mut document = original.clone();
        document["schema_version"] = version.into();
        document["outcome"] = "failed".into();
        document["scenarios"][0]["outcome"] = "failed".into();
        let (_tmp, store) = fixture(&serde_json::to_vec(&document).unwrap());
        let snap = load_snapshot(&store, 0).unwrap();
        assert_eq!(
            snap.report.outcome,
            if version <= 3 {
                RunOutcome::Failed
            } else {
                RunOutcome::Passed
            }
        );
        let view = super::super::status_projection::project_snapshot(&snap).unwrap();
        assert_eq!(view.saved.outcome, snap.report.outcome);
        assert_eq!(view.saved.counts.passed_checks.get(), 1);
    }
}

// Reuses the model's actual pure JSON evaluator and guest protocol proof builder;
// no Environment, engine worker, subprocess, or probe executable is constructed.
fn proof_report(version: u32) -> RunReport {
    let check:crate::recipe::Check=serde_json::from_value(serde_json::json!({"id":"json","checkpoint":"target_installed","kind":"json_command","script":"printf true","expected_exit":0,"assertions":[{"kind":"equals","pointer":"","equals_json":"true"}]})).unwrap();
    let mut result = crate::checks::evaluate_json_command(
        &check,
        &crate::checks::JsonCommandObservation {
            exit_status: Some(0),
            stdout: "true",
            complete: true,
            utf8: true,
            error: None,
        },
    );
    result.command_ids.push("json-command".into());
    let mut command = CommandRecord::new("json-command", "check:json");
    command.exit_status = Some(0);
    command.outcome = CommandOutcome::Succeeded;
    command.stdout = Some(LogReference {
        path: "logs/json.stdout".into(),
        bytes: 4,
        truncated: false,
    });
    let mut stage = StageReport::new("target_installed");
    stage.outcome = StageOutcome::Passed;
    stage.commands.push(command);
    stage.checks.push(result);
    if version == 8 {
        use crate::mcp::{McpProbe, McpProtocol, McpSession, McpShutdown};
        let probe = McpProbe {
            argv: vec!["/inputs/target/server".into()],
            env: Default::default(),
            timeout_seconds: 15,
            protocol: McpProtocol::Modern20260728,
            server_name: "fixture".into(),
            server_version: "2".into(),
            tools: vec!["hello_v2".into()],
        };
        let mut session = McpSession::new(probe.clone()).unwrap();
        for response in [
            serde_json::json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"public","_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fixture","version":"2"}}}),
            serde_json::json!({"resultType":"complete","tools":[{"name":"hello_v2","inputSchema":{"type":"object"}}],"ttlMs":0,"cacheScope":"public"}),
        ] {
            let outgoing: serde_json::Value =
                serde_json::from_slice(session.pending_write()).unwrap();
            session.written(session.pending_write().len()).unwrap();
            let mut line = serde_json::to_vec(
                &serde_json::json!({"jsonrpc":"2.0","id":outgoing["id"],"result":response}),
            )
            .unwrap();
            line.push(b'\n');
            session.receive(&line);
        }
        session.eof();
        let proof = session.finish(McpShutdown::Graceful);
        assert!(proof.verified_pass());
        let mut check = stage.checks[0].clone();
        check.id = "mcp".into();
        check.json_expectation = None;
        check.json_required = false;
        check.mcp_required = true;
        check.command_ids = vec!["mcp-command".into()];
        check.mcp = Some(proof.clone());
        let mut command = CommandRecord::new("mcp-command", "check:mcp");
        command.argv = probe.argv;
        command.environment = command.identity.public_environment();
        command.outcome = CommandOutcome::Succeeded;
        command.exit_status = Some(0);
        command.stdout = Some(LogReference {
            path: "logs/mcp.stdout".into(),
            bytes: proof.stdout_bytes,
            truncated: false,
        });
        command.stderr = Some(LogReference {
            path: "logs/mcp.stderr".into(),
            bytes: 0,
            truncated: false,
        });
        stage.checks.push(check);
        stage.commands.push(command);
    }
    let mut scenario = ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
    scenario.teardown = TeardownStatus::Complete;
    scenario.stages.push(stage);
    let mut report = RunReport::new(format!("schema{version}"));
    report.schema_version = version;
    report.scenarios.push(scenario);
    report.recompute_outcome();
    report
}

#[test]
fn detail_preserves_all_optional_model_proof_fields_without_projection_loss() {
    let mut report = proof_report(8);
    let rollback: crate::recipe::Recipe =
        toml::from_str(include_str!("../../tests/fixtures/recipes/rollback.toml")).unwrap();
    let interrupted: crate::recipe::Recipe = toml::from_str(include_str!(
        "../../tests/fixtures/recipes/interrupted_rollback.toml"
    ))
    .unwrap();
    let s = &mut report.scenarios[0];
    s.rollback = rollback.profiles[&ProfileName::User].rollback.clone();
    s.interrupted_rollback = interrupted.profiles[&ProfileName::User]
        .interrupted_rollback
        .clone();
    s.plugin=Some(serde_json::from_value(serde_json::json!({"id":"target@market","scope":"user","base_version":"1","target_version":"2","client_version":"2.1.312 (Claude Code)","client_check":"version","client_artifact":"client/claude","checkpoints":{"base_installed":{"inventory":"base","preservation":["preserve"],"components":["component"]}}})).unwrap());
    s.mcp=Some(serde_json::from_value(serde_json::json!({"adapter":"claude-code-2.1.312","delivery":"standalone","client_claim":"native_health","server_id":"server","client_alias":"alias","scope":"user","client_version":"2.1.312 (Claude Code)","client_check":"version","client_artifact":"client/claude","checkpoints":{"configured":{"state":"active","registration":"registration","preservation":["preserve"],"activation":"activate","probe":"probe"}}})).unwrap());
    let stage = &mut s.stages[0];
    stage.restart = Some(RestartEvidence {
        container_id: "container".into(),
        running_verified: true,
    });
    let c = &mut stage.commands[0];
    c.interruption = Some(InterruptionEvidence {
        mechanism: InterruptionMechanism::ContainerSigkill,
        readiness_verified: true,
        stopped_verified: true,
    });
    c.rejection = Some(RejectionEvidence {
        expected_exit: 9,
        stdout_contains: "rejected".into(),
        stdout_matched: true,
    });
    c.expected_failure = Some(ExpectedFailureEvidence {
        expected_exit: 9,
        stdout_contains: "expected failure".into(),
        stdout_matched: true,
    });
    c.forwarded_environment.push("ONLY_NAME".into());
    let c = &mut stage.checks[0];
    c.comparison = Some(ComparisonReference::reinstall());
    c.backup = Some(BackupReference {
        source_path: "/data".into(),
        stored_path: "/backup".into(),
    });
    c.command_expectation = Some(CommandExpectationEvidence {
        expected_exit: 0,
        stdout_contains: "healthy".into(),
        stdout_matched: true,
        stdout_match: Some("healthy".into()),
        executable: Some(ExecutableExpectationEvidence {
            name: "tool".into(),
            resolved_path: "/tool".into(),
            args: vec!["health".into()],
        }),
    });
    let snap = direct_snapshot(report);
    let (_tmp, store) = fixture(b"{}");
    let s = &snap.report.scenarios[0];
    let header: serde_json::Value =
        serde_json::from_slice(&detail(&store, &snap, "s/0", 512)).unwrap();
    let original = serde_json::to_value(s).unwrap();
    for key in ["rollback", "interrupted_rollback", "plugin", "mcp"] {
        assert_eq!(header[key], original[key], "{key}");
        assert!(!header[key].is_null());
    }
    for (i, command) in s.stages[0].commands.iter().enumerate() {
        assert_eq!(
            detail(&store, &snap, &format!("s/0/stage/0/command/{i}"), 127),
            serde_json::to_vec(command).unwrap()
        );
    }
    for (i, check) in s.stages[0].checks.iter().enumerate() {
        assert_eq!(
            detail(&store, &snap, &format!("s/0/stage/0/check/{i}"), 127),
            serde_json::to_vec(check).unwrap()
        );
    }
}

#[test]
fn log_budget_fits_full_envelope_and_retains_full_source_id_and_requested_limit() {
    let (_tmp, store) = fixture(b"{}");
    let mut report = rich_report();
    let id = "\0".repeat(4096);
    let command = &mut report.scenarios[0].stages[0].commands[0];
    command.id = id.clone();
    command.stdout = Some(
        store
            .write_log("logs/escaped", &vec![255; 4096], 4096)
            .unwrap(),
    );
    let snap = direct_snapshot(report);
    let a = args(
        serde_json::json!({"kind":"log","command_id":id,"stream":"stdout"}),
        Some(&snap.revision),
        4096,
    );
    for era in [Era::Legacy, Era::Modern] {
        let reply = ReplyContext {
            era,
            id: RpcId::String("\0".repeat(128)),
            ..reply()
        };
        let p = match evidence(&store, &snap, &a, &reply).unwrap() {
            EvidenceData::Log(p) => p,
            _ => panic!(),
        };
        assert!(!p.bytes.is_empty());
        assert!(p.bytes.len() < 4096);
        assert_eq!(p.command_id, id);
        let cursor = super::super::cursor::CursorRecord::decode(
            p.next_cursor.as_ref().unwrap(),
            &reply.instance,
        )
        .unwrap();
        assert_eq!(cursor.requested_limit, 4096);
        assert_eq!(cursor.next_position, p.bytes.len() as u64);
        let frame = super::super::budget::encode(
            era,
            &reply.id,
            &reply.instance,
            Some(&a.handle),
            &Ok(ToolData::RunEvidence(EvidenceData::Log(p))),
        )
        .unwrap();
        assert!(frame.bytes.len() <= 65536);
    }
    let p = catalog(
        &store,
        &snap,
        serde_json::json!({"kind":"catalog","collection":"commands","scenario_id":snap.report.scenarios[0].id,"stage_index":0}),
    );
    let preview = p.records[0].source_id.as_ref().unwrap();
    assert_eq!(preview.text.len(), 128);
    assert!(preview.truncated);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&detail(
            &store,
            &snap,
            "s/0/stage/0/command/0",
            4096
        ))
        .unwrap()["id"],
        id
    );
}

#[test]
fn malformed_index_selectors_and_failed_reload_do_not_mutate_existing_snapshot() {
    let report = rich_report();
    let (_tmp, store) = fixture(&serde_json::to_vec(&report).unwrap());
    let snap = load_snapshot(&store, 5).unwrap();
    let old = super::super::status_projection::project_snapshot(&snap).unwrap();
    let sid = &snap.report.scenarios[0].id;
    for selector in [
        serde_json::json!({"kind":"catalog","collection":"artifact_files","artifact_index":9007199254740991u64}),
        serde_json::json!({"kind":"catalog","collection":"commands","scenario_id":sid,"stage_index":9007199254740991u64}),
        serde_json::json!({"kind":"catalog","collection":"snapshot_entries","scenario_id":sid,"stage_index":0,"snapshot_index":9007199254740991u64}),
        serde_json::json!({"kind":"record","record_id":"s/0/stage/0/snapshot/0/entry/999"}),
    ] {
        let a = args(selector, Some(&snap.revision), 32);
        assert_eq!(
            evidence(&store, &snap, &a, &reply()).unwrap_err().code,
            ErrorCode::UnknownRecord
        );
    }
    std::fs::write(store.report_path(), b"invalid report").unwrap();
    assert_eq!(
        load_snapshot(&store, 9).unwrap_err().code,
        ErrorCode::EvidenceInvalid
    );
    let current = super::super::status_projection::project_snapshot(&snap).unwrap();
    assert_eq!(
        serde_json::to_value(current.saved).unwrap(),
        serde_json::to_value(old.saved).unwrap()
    );
    assert_eq!(snap.sampled_hint, 5);
}

#[test]
fn boundary_cursor_bindings_precede_selected_lookup() {
    let snap = direct_snapshot(rich_report());
    let (_tmp, store) = fixture(b"{}");
    let mut a = args(
        serde_json::json!({"kind":"record","record_id":"run"}),
        Some(&snap.revision),
        16,
    );
    let p = match evidence(&store, &snap, &a, &reply()).unwrap() {
        EvidenceData::Record(p) => p,
        _ => panic!(),
    };
    a.cursor = p.next_cursor;
    a.selector =
        serde_json::from_value(serde_json::json!({"kind":"record","record_id":"s/999/stage/0"}))
            .unwrap();
    assert_eq!(
        evidence(&store, &snap, &a, &reply()).unwrap_err().code,
        ErrorCode::InvalidCursor
    );
}
#[test]
fn boundary_missing_log_continuation_is_changed() {
    let (_tmp, store) = fixture(b"{}");
    let mut report = rich_report();
    let command = &mut report.scenarios[0].stages[0].commands[0];
    let id = command.id.clone();
    command.stdout = Some(store.write_log("logs/vanish", b"abcdef", 6).unwrap());
    let snap = direct_snapshot(report);
    let mut a = args(
        serde_json::json!({"kind":"log","command_id":id,"stream":"stdout"}),
        Some(&snap.revision),
        2,
    );
    let p = match evidence(&store, &snap, &a, &reply()).unwrap() {
        EvidenceData::Log(p) => p,
        _ => panic!(),
    };
    a.cursor = p.next_cursor;
    std::fs::rename(
        store.root().join("logs/vanish"),
        store.root().join("logs/moved"),
    )
    .unwrap();
    assert_eq!(
        evidence(&store, &snap, &a, &reply()).unwrap_err().code,
        ErrorCode::LogChanged
    );
}
#[test]
fn catalogs_headers_and_exact_model_children_are_reachable() {
    let bytes = std::fs::read("tests/fixtures/reports/schema6-report.json").unwrap();
    let (_tmp, store) = fixture(&bytes);
    let snap = load_snapshot(&store, 0).unwrap();
    let p = catalog(
        &store,
        &snap,
        serde_json::json!({"kind":"catalog","collection":"run"}),
    );
    assert_eq!(p.records[0].record_id.as_str(), "run");
    let value: serde_json::Value =
        serde_json::from_slice(&detail(&store, &snap, "run", 7)).unwrap();
    assert!(value.get("scenarios").is_none());
    assert_eq!(value["scenario_count"], snap.report.scenarios.len());
    let p = catalog(
        &store,
        &snap,
        serde_json::json!({"kind":"catalog","collection":"scenarios"}),
    );
    assert_eq!(p.records.len(), snap.report.scenarios.len());
    for (s, scenario) in snap.report.scenarios.iter().enumerate() {
        for (t, stage) in scenario.stages.iter().enumerate() {
            for (i, c) in stage.commands.iter().enumerate() {
                assert_eq!(
                    detail(&store, &snap, &format!("s/{s}/stage/{t}/command/{i}"), 13),
                    serde_json::to_vec(c).unwrap()
                );
            }
            for (i, c) in stage.checks.iter().enumerate() {
                assert_eq!(
                    detail(&store, &snap, &format!("s/{s}/stage/{t}/check/{i}"), 13),
                    serde_json::to_vec(c).unwrap()
                );
            }
        }
    }
}
#[test]
fn identity_revision_and_duplicate_source_ids_reject_before_lookup() {
    let mut run = RunReport::new("duplicates");
    let mut s = ScenarioReport::new("same", ProfileName::User, ScenarioKind::Clean);
    let mut stage = StageReport::new("stage");
    stage.commands.push(CommandRecord::new("cmd", "x"));
    stage.commands.push(CommandRecord::new("cmd", "x"));
    s.stages.push(stage);
    run.scenarios.extend([s.clone(), s]);
    let (_tmp, store) = fixture(&serde_json::to_vec(&run).unwrap());
    let snap = load_snapshot(&store, 0).unwrap();
    let a = args(
        serde_json::json!({"kind":"catalog","collection":"stages","scenario_id":"same"}),
        Some(&snap.revision),
        32,
    );
    assert_eq!(
        evidence(&store, &snap, &a, &reply()).unwrap_err().code,
        ErrorCode::AmbiguousRecord
    );
    let a = args(
        serde_json::json!({"kind":"log","command_id":"cmd","stream":"stdout"}),
        Some(&snap.revision),
        32,
    );
    assert_eq!(
        evidence(&store, &snap, &a, &reply()).unwrap_err().code,
        ErrorCode::AmbiguousRecord
    );
    let a = args(
        serde_json::json!({"kind":"record","record_id":"s/999/stage/0"}),
        Some(&snap.revision),
        32,
    );
    assert_eq!(
        evidence(&store, &snap, &a, &reply()).unwrap_err().code,
        ErrorCode::UnknownRecord
    );
    let a = args(
        serde_json::json!({"kind":"record","record_id":"run"}),
        Some(&"c".repeat(64)),
        32,
    );
    assert_eq!(
        evidence(&store, &snap, &a, &reply()).unwrap_err().code,
        ErrorCode::RevisionChanged
    );
    let mut a = args(
        serde_json::json!({"kind":"record","record_id":"run"}),
        Some(&snap.revision),
        32,
    );
    a.server_instance = Instance::parse(&"d".repeat(32)).unwrap();
    a.handle = Handle::parse(&format!("{}:1", "d".repeat(32))).unwrap();
    assert_eq!(
        evidence(&store, &snap, &a, &reply()).unwrap_err().code,
        ErrorCode::StaleInstance
    );
}
#[test]
fn logs_preserve_raw_bytes_reference_counts_and_detect_cross_page_replacement() {
    let (_tmp, store) = fixture(b"{}");
    let mut run = RunReport::new("log");
    let mut s = ScenarioReport::new("s", ProfileName::User, ScenarioKind::Clean);
    let mut stage = StageReport::new("stage");
    let mut c = CommandRecord::new("cmd", "x");
    c.stdout = Some(
        store
            .write_log("logs/cmd.stdout", &[0xff, 0xc3, 0xa9, 0, 65], 4)
            .unwrap(),
    );
    stage.commands.push(c);
    s.stages.push(stage);
    run.scenarios.push(s);
    store.save(&run).unwrap();
    let snap = load_snapshot(&store, 0).unwrap();
    let mut a = args(
        serde_json::json!({"kind":"log","command_id":"cmd","stream":"stdout"}),
        Some(&snap.revision),
        2,
    );
    let first = match evidence(&store, &snap, &a, &reply()).unwrap() {
        EvidenceData::Log(p) => p,
        _ => panic!(),
    };
    assert_eq!(first.bytes, [0xff, 0xc3]);
    assert_eq!(first.recorded_bytes.get(), 4);
    assert_eq!(first.current_bytes.get(), 4);
    assert!(first.capture_truncated);
    assert_eq!(first.change_token.len(), 32);
    let native = report::log_fingerprint(store.root(), "logs/cmd.stdout").unwrap();
    let digest = format!("{:x}", Sha256::digest(native.canonical_bytes()));
    assert_eq!(first.change_token, digest[..32]);
    a.cursor = first.next_cursor;
    let next = match evidence(&store, &snap, &a, &reply()).unwrap() {
        EvidenceData::Log(p) => p,
        _ => panic!(),
    };
    assert_eq!(next.bytes, [0xa9, 0]);
    assert!(next.next_cursor.is_none());
    let path = store.root().join("logs/cmd.stdout");
    std::fs::rename(&path, path.with_extension("old")).unwrap();
    std::fs::write(&path, [0xff, 0xc3, 0xa9, 0]).unwrap();
    assert_eq!(
        evidence(&store, &snap, &a, &reply()).unwrap_err().code,
        ErrorCode::LogChanged
    );
}

fn rich_report() -> RunReport {
    use crate::snapshot::*;
    let mut r: RunReport = serde_json::from_str(include_str!(
        "../../tests/fixtures/reports/schema6-report.json"
    ))
    .unwrap();
    r.schema_version = 8;
    r.artifacts.push(ArtifactDigest {
        name: "target".into(),
        sha256: "digest".into(),
        files: vec![ArtifactFileDigest {
            path: "all-fields".into(),
            sha256: "filedigest".into(),
            size: u64::MAX,
        }],
    });
    r.errors.push("run error".into());
    let s = &mut r.scenarios[0];
    s.findings.push(Finding {
        kind: FindingKind::Leftover,
        path: Some("/left".into()),
        message: "finding".into(),
    });
    s.errors.push("scenario error".into());
    let t = &mut s.stages[0];
    t.errors.push("stage error".into());
    t.restart = Some(RestartEvidence {
        container_id: "container".into(),
        running_verified: true,
    });
    let entry = FileEntry {
        kind: FileKind::Symlink,
        mode: 0o777,
        uid: u64::MAX,
        gid: 9,
        size: 12,
        sha256: Some("entrydigest".into()),
        link_target: Some("λ\n\t🐈".into()),
    };
    t.snapshots = vec![Snapshot {
        root: "/root".into(),
        completeness: ObservationStatus::Complete,
        entries: [("/a".into(), entry.clone()), ("/z".into(), entry.clone())].into(),
    }];
    let change = FileChange {
        path: "/changed".into(),
        kind: ChangeKind::Modified,
        before: Some(entry.clone()),
        after: Some(entry),
    };
    t.baseline_diff = vec![change.clone()];
    t.stage_diff = vec![change];
    t.commands[0].duration_ms = u64::MAX;
    r
}
fn direct_snapshot(report: RunReport) -> ReportSnapshot {
    ReportSnapshot {
        revision: "e".repeat(64),
        report,
        sampled_hint: 0,
    }
}
#[test]
fn every_catalog_uses_model_positions_and_detail_keeps_exact_child_serialization() {
    let (_tmp, store) = fixture(b"{}");
    let snap = direct_snapshot(rich_report());
    let sid = &snap.report.scenarios[0].id;
    let cases = vec![
        (
            serde_json::json!({"kind":"catalog","collection":"artifacts"}),
            vec!["artifact/0".to_string()],
        ),
        (
            serde_json::json!({"kind":"catalog","collection":"errors"}),
            vec!["error/0".to_string()],
        ),
        (
            serde_json::json!({"kind":"catalog","collection":"stages","scenario_id":sid}),
            (0..snap.report.scenarios[0].stages.len())
                .map(|i| format!("s/0/stage/{i}"))
                .collect(),
        ),
        (
            serde_json::json!({"kind":"catalog","collection":"findings","scenario_id":sid}),
            vec!["s/0/finding/0".to_string()],
        ),
        (
            serde_json::json!({"kind":"catalog","collection":"scenario_errors","scenario_id":sid}),
            vec!["s/0/error/0".to_string()],
        ),
        (
            serde_json::json!({"kind":"catalog","collection":"artifact_files","artifact_index":0}),
            vec!["artifact/0/file/0".to_string()],
        ),
        (
            serde_json::json!({"kind":"catalog","collection":"snapshot_entries","scenario_id":sid,"stage_index":0,"snapshot_index":0}),
            vec![
                "s/0/stage/0/snapshot/0/entry/0".to_string(),
                "s/0/stage/0/snapshot/0/entry/1".to_string(),
            ],
        ),
    ];
    for (selector, ids) in cases {
        let p = catalog(&store, &snap, selector);
        assert_eq!(
            p.records
                .iter()
                .map(|r| r.record_id.as_str())
                .collect::<Vec<_>>(),
            ids
        );
    }
    let t = &snap.report.scenarios[0].stages[0];
    for (collection, part, len) in [
        ("commands", "command", t.commands.len()),
        ("checks", "check", t.checks.len()),
        ("snapshots", "snapshot", t.snapshots.len()),
        ("baseline_changes", "baseline", t.baseline_diff.len()),
        ("stage_changes", "change", t.stage_diff.len()),
        ("stage_errors", "error", t.errors.len()),
    ] {
        let p = catalog(
            &store,
            &snap,
            serde_json::json!({"kind":"catalog","collection":collection,"scenario_id":sid,"stage_index":0}),
        );
        assert_eq!(
            p.records
                .iter()
                .map(|r| r.record_id.as_str())
                .collect::<Vec<_>>(),
            (0..len)
                .map(|i| format!("s/0/stage/0/{part}/{i}"))
                .collect::<Vec<_>>()
        );
    }
    for (id, want) in [
        (
            "artifact/0/file/0",
            serde_json::to_vec(&snap.report.artifacts[0].files[0]).unwrap(),
        ),
        (
            "s/0/finding/0",
            serde_json::to_vec(&snap.report.scenarios[0].findings[0]).unwrap(),
        ),
        (
            "s/0/stage/0/baseline/0",
            serde_json::to_vec(&t.baseline_diff[0]).unwrap(),
        ),
        (
            "s/0/stage/0/change/0",
            serde_json::to_vec(&t.stage_diff[0]).unwrap(),
        ),
        (
            "s/0/stage/0/command/0",
            serde_json::to_vec(&t.commands[0]).unwrap(),
        ),
        (
            "s/0/stage/0/check/0",
            serde_json::to_vec(&t.checks[0]).unwrap(),
        ),
        (
            "s/0/stage/0/snapshot/0/entry/0",
            serde_json::to_vec(
                &serde_json::json!({"path":"/a","entry":t.snapshots[0].entries["/a"]}),
            )
            .unwrap(),
        ),
        ("error/0", br#"{"message":"run error"}"#.to_vec()),
        ("s/0/error/0", br#"{"message":"scenario error"}"#.to_vec()),
        (
            "s/0/stage/0/error/0",
            br#"{"message":"stage error"}"#.to_vec(),
        ),
    ] {
        let got = detail(&store, &snap, id, 19);
        if id.contains("/entry/") {
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&got).unwrap(),
                serde_json::from_slice::<serde_json::Value>(&want).unwrap()
            );
        } else {
            assert_eq!(got, want, "{id}");
        }
    }
    assert!(
        String::from_utf8(detail(&store, &snap, "artifact/0/file/0", 31))
            .unwrap()
            .contains("18446744073709551615")
    );
    for (id, source, children, counts) in [
        (
            "run",
            serde_json::to_value(&snap.report).unwrap(),
            vec!["scenarios", "artifacts", "errors"],
            vec![
                ("scenario_count", snap.report.scenarios.len()),
                ("artifact_count", snap.report.artifacts.len()),
                ("error_count", snap.report.errors.len()),
            ],
        ),
        (
            "s/0",
            serde_json::to_value(&snap.report.scenarios[0]).unwrap(),
            vec!["stages", "findings", "errors"],
            vec![
                ("stage_count", snap.report.scenarios[0].stages.len()),
                ("finding_count", 1),
                ("error_count", 1),
            ],
        ),
        (
            "s/0/stage/0",
            serde_json::to_value(t).unwrap(),
            vec![
                "commands",
                "checks",
                "snapshots",
                "baseline_diff",
                "stage_diff",
                "errors",
            ],
            vec![
                ("command_count", t.commands.len()),
                ("check_count", t.checks.len()),
                ("snapshot_count", 1),
                ("baseline_diff_count", 1),
                ("stage_diff_count", 1),
                ("error_count", 1),
            ],
        ),
        (
            "artifact/0",
            serde_json::to_value(&snap.report.artifacts[0]).unwrap(),
            vec!["files"],
            vec![("file_count", 1)],
        ),
        (
            "s/0/stage/0/snapshot/0",
            serde_json::to_value(&t.snapshots[0]).unwrap(),
            vec!["entries"],
            vec![("entry_count", 2)],
        ),
    ] {
        let mut expected = source;
        let map = expected.as_object_mut().unwrap();
        for child in children {
            map.remove(child);
        }
        for (name, count) in counts {
            map.insert(name.into(), count.into());
        }
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&detail(&store, &snap, id, 23)).unwrap(),
            expected,
            "{id}"
        );
    }
}
#[test]
fn catalog_pagination_preserves_membership_and_rejects_mutated_bindings_and_endpoints() {
    let mut r = RunReport::new("pages");
    for i in 0..67 {
        r.errors.push(format!("error-{i}"));
    }
    let (_tmp, store) = fixture(b"{}");
    let snap = direct_snapshot(r);
    let mut a = args(
        serde_json::json!({"kind":"catalog","collection":"errors"}),
        Some(&snap.revision),
        32,
    );
    let mut ids = Vec::new();
    let first = match evidence(&store, &snap, &a, &reply()).unwrap() {
        EvidenceData::Catalog(p) => p,
        _ => panic!(),
    };
    let cursor = first.next_cursor.clone().unwrap();
    ids.extend(first.records.into_iter().map(|r| r.record_id));
    a.cursor = Some(cursor.clone());
    loop {
        let p = match evidence(&store, &snap, &a, &reply()).unwrap() {
            EvidenceData::Catalog(p) => p,
            _ => panic!(),
        };
        assert!(!p.records.is_empty());
        ids.extend(p.records.into_iter().map(|r| r.record_id));
        a.cursor = p.next_cursor;
        if a.cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        ids.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
        (0..67).map(|i| format!("error/{i}")).collect::<Vec<_>>()
    );
    a.cursor = Some(cursor.clone());
    a.limit = Some(UInt::new(31).unwrap());
    assert_eq!(
        evidence(&store, &snap, &a, &reply()).unwrap_err().code,
        ErrorCode::InvalidCursor
    );
    a.limit = Some(UInt::new(32).unwrap());
    a.selector = EvidenceSelector::Root(RootCatalog {
        kind: CatalogKind::Catalog,
        collection: RootCollection::Run,
    });
    assert_eq!(
        evidence(&store, &snap, &a, &reply()).unwrap_err().code,
        ErrorCode::InvalidCursor
    );
    a.selector = EvidenceSelector::Root(RootCatalog {
        kind: CatalogKind::Catalog,
        collection: RootCollection::Errors,
    });
    let mut c = super::super::cursor::CursorRecord::decode(&cursor, &reply().instance).unwrap();
    c.next_position = 67;
    a.cursor = Some(c.encode().unwrap());
    assert_eq!(
        evidence(&store, &snap, &a, &reply()).unwrap_err().code,
        ErrorCode::InvalidCursor
    );
    let empty = direct_snapshot(RunReport::new("empty"));
    let p = catalog(
        &store,
        &empty,
        serde_json::json!({"kind":"catalog","collection":"errors"}),
    );
    assert!(p.records.is_empty());
    assert!(p.next_cursor.is_none());
}
#[test]
fn bounded_record_window_keeps_exact_unicode_escapes_without_whole_detail_allocation() {
    let mut r = rich_report();
    r.scenarios[0].stages[0].commands[0].argv = vec![format!("{}TAIL", "λ\n\t🐈".repeat(300_000))];
    let snap = direct_snapshot(r);
    let (_tmp, store) = fixture(b"{}");
    let mut a = args(
        serde_json::json!({"kind":"record","record_id":"s/0/stage/0/command/0"}),
        Some(&snap.revision),
        7,
    );
    let p = match evidence(&store, &snap, &a, &reply()).unwrap() {
        EvidenceData::Record(p) => p,
        _ => panic!(),
    };
    assert_eq!(p.bytes, br#"{"id":""#);
    assert!(p.total_bytes.get() > 3_000_000);
    let mut c = super::super::cursor::CursorRecord::decode(
        p.next_cursor.as_ref().unwrap(),
        &reply().instance,
    )
    .unwrap();
    c.next_position = p.total_bytes.get() - 7;
    a.cursor = Some(c.encode().unwrap());
    let p = match evidence(&store, &snap, &a, &reply()).unwrap() {
        EvidenceData::Record(p) => p,
        _ => panic!(),
    };
    assert_eq!(p.bytes.len(), 7);
    assert!(p.next_cursor.is_none());
}
#[test]
fn log_empty_shortened_and_malformed_reference_fail_conservatively() {
    let (_tmp, store) = fixture(b"{}");
    let mut r = rich_report();
    let c = &mut r.scenarios[0].stages[0].commands[0];
    let id = c.id.clone();
    c.stdout = Some(store.write_log("logs/empty", b"", 1).unwrap());
    let snap = direct_snapshot(r.clone());
    let a = args(
        serde_json::json!({"kind":"log","command_id":id,"stream":"stdout"}),
        Some(&snap.revision),
        2,
    );
    let p = match evidence(&store, &snap, &a, &reply()).unwrap() {
        EvidenceData::Log(p) => p,
        _ => panic!(),
    };
    assert!(p.bytes.is_empty());
    assert!(p.next_cursor.is_none());
    r.scenarios[0].stages[0].commands[0].stdout =
        Some(store.write_log("logs/short", b"abcd", 4).unwrap());
    std::fs::write(store.root().join("logs/short"), b"x").unwrap();
    let snap = direct_snapshot(r.clone());
    let p = match evidence(&store, &snap, &a, &reply()).unwrap() {
        EvidenceData::Log(p) => p,
        _ => panic!(),
    };
    assert_eq!(p.bytes, b"x");
    assert_eq!(p.recorded_bytes.get(), 4);
    assert_eq!(p.current_bytes.get(), 1);
    assert_eq!(p.readable_total.get(), 1);
    r.scenarios[0].stages[0].commands[0]
        .stdout
        .as_mut()
        .unwrap()
        .path = "../outside".into();
    assert!(evidence(&store, &direct_snapshot(r), &a, &reply()).is_err());
}
