//! Opt-in tests: only the explicitly selected cached image and synthetic guests.
#[path = "support/docker_absence.rs"]
mod docker_absence;
use std::{
    collections::BTreeMap,
    process::Command,
    time::{Duration, Instant},
};
use theseus::{
    engine::CancellationToken,
    mcp::*,
    model::*,
    recipe, report,
    runner::{CommandRequest, Docker, Environment, ExecutedMcp},
};
fn immutable_image_id(image: &str) -> bool {
    image.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
struct Guest {
    docker: Docker,
    resource: OwnedResource,
    store: report::ReportStore,
    _root: tempfile::TempDir,
    image_id: String,
}
impl Guest {
    fn new() -> Self {
        let image = std::env::var("THESEUS_MCP_TEST_IMAGE").expect(
            "explicit prepared image ID THESEUS_MCP_TEST_IMAGE required; no pull or fallback",
        );
        assert!(
            immutable_image_id(&image),
            "explicit full immutable sha256 image ID required"
        );
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("target")).unwrap();
        std::fs::write(
            root.path().join("target/server.sh"),
            include_bytes!("fixtures/mcp/server.sh"),
        )
        .unwrap();
        let text =
            include_str!("fixtures/recipes/v2_clean.toml").replace("debian:bookworm-slim", &image);
        let recipe =
            recipe::parse_bytes(&root.path().join("recipe.toml"), text.as_bytes()).unwrap();
        let mut docker = Docker::default();
        let cancel = CancellationToken::default();
        let prepared = docker.prepare(&recipe, false, &cancel).unwrap();
        assert_eq!(prepared.image_id, image);
        let resource = docker
            .create("mcp-transport-test", "user-clean", &recipe)
            .unwrap();
        eprintln!("created owned {}", resource.container_id);
        let mut guest = Self {
            docker,
            resource,
            store: report::create(&root.path().join("run")).unwrap(),
            _root: root,
            image_id: prepared.image_id,
        };
        guest
            .docker
            .setup(&guest.resource, &recipe, &cancel)
            .unwrap();
        guest
    }
    fn probe(
        &mut self,
        era: McpProtocol,
        mode: &str,
        status: i32,
        cancel: &CancellationToken,
    ) -> ExecutedMcp {
        let env = BTreeMap::from([
            (
                "ERA".into(),
                if era == McpProtocol::Modern20260728 {
                    "modern"
                } else {
                    "legacy"
                }
                .into(),
            ),
            ("MODE".into(), mode.into()),
            ("APP_EXIT".into(), status.to_string()),
        ]);
        let probe = McpProbe {
            argv: vec!["/bin/sh".into(), "/inputs/target/server.sh".into()],
            env: env.clone(),
            timeout_seconds: 3,
            protocol: era,
            server_name: "fixture".into(),
            server_version: "2".into(),
            tools: vec!["hello_v2".into()],
        };
        let request = CommandRequest {
            id: format!("probe-{mode}-{status}-{}", era.version()),
            phase: "check:discovery".into(),
            identity: ExecutionIdentity::runtime(),
            argv: probe.argv.clone(),
            env,
            pass_env: vec![],
            timeout_seconds: 3,
            expected_exit: 0,
            log_limit: if mode == "truncated-ignore" {
                408
            } else {
                262144
            },
        };
        let observed = self
            .docker
            .probe_mcp(&self.resource, request, probe, &self.store, cancel)
            .unwrap();
        if let Some(root) = std::env::var_os("THESEUS_MCP_TEST_EVIDENCE") {
            let directory = std::path::PathBuf::from(root)
                .join(&self.resource.container_id)
                .join(&observed.command.record.id);
            std::fs::create_dir_all(&directory).unwrap();
            for (name, log) in [
                ("stdout.log", &observed.command.record.stdout),
                ("stderr.log", &observed.command.record.stderr),
            ] {
                if let Some(log) = log {
                    std::fs::copy(self.store.root().join(&log.path), directory.join(name)).unwrap();
                }
            }
            std::fs::write(directory.join("observation.json"),serde_json::to_vec_pretty(&serde_json::json!({"image_id":self.image_id,"record":observed.command.record,"evidence":observed.evidence,"container_available":observed.container_available})).unwrap()).unwrap();
        }
        observed
    }
}
impl Drop for Guest {
    fn drop(&mut self) {
        self.docker.remove(&self.resource).unwrap();
        docker_absence::absent(&self.resource.container_id);
        eprintln!("verified absent {}", self.resource.container_id);
    }
}
fn outcome(o: &ExecutedMcp) -> CheckOutcome {
    let check = recipe::Check {
        id: "discovery".into(),
        checkpoint: Checkpoint::Configured,
        expectation: recipe::Expectation::McpStdio {
            probe: o.evidence.probe.clone(),
        },
    };
    theseus::checks::evaluate_mcp(&check, o).outcome
}
#[test]
#[ignore = "requires explicit cached image and reachable Docker"]
fn interactive_both_eras_prove_real_eof_and_application_receipt() {
    for era in [McpProtocol::Modern20260728, McpProtocol::Legacy20251125] {
        let mut guest = Guest::new();
        for status in [0, 17] {
            let o = guest.probe(era, "clean", status, &CancellationToken::default());
            assert!(o.container_available, "{:?}", o.command.record.error);
            assert_eq!(o.evidence.shutdown, McpShutdown::Graceful);
            assert_eq!(o.command.record.exit_status, Some(status));
            assert!(o.evidence.verified_pass());
            assert_eq!(
                outcome(&o),
                if status == 0 {
                    CheckOutcome::Passed
                } else {
                    CheckOutcome::Failed
                }
            );
            assert!(
                std::fs::read_to_string(
                    guest
                        .store
                        .root()
                        .join(&o.command.record.stderr.unwrap().path)
                )
                .unwrap()
                .contains("observed stdin EOF")
            );
        }
    }
}
#[test]
#[ignore = "requires explicit cached image and reachable Docker"]
fn wrong_inventory_is_failed_with_clean_receipt() {
    let mut guest = Guest::new();
    let o = guest.probe(
        McpProtocol::Modern20260728,
        "wrong",
        0,
        &CancellationToken::default(),
    );
    assert_eq!(outcome(&o), CheckOutcome::Failed);
    assert_eq!(o.command.record.exit_status, Some(0));
    assert!(o.command.record.error.is_none() && o.container_available);
    assert_eq!(o.evidence.issue.unwrap().kind, McpIssueKind::Inventory);
}
#[test]
#[ignore = "requires explicit cached image and reachable Docker"]
fn ignored_eof_removes_owned_guest_without_claiming_application_exit() {
    for mode in ["ignore", "wrong-ignore"] {
        let mut guest = Guest::new();
        let started = Instant::now();
        let o = guest.probe(
            McpProtocol::Modern20260728,
            mode,
            0,
            &CancellationToken::default(),
        );
        assert!(started.elapsed() < Duration::from_secs(6));
        assert!(!o.container_available);
        assert_eq!(o.command.record.exit_status, None);
        assert_eq!(o.evidence.shutdown, McpShutdown::ForcedOwnedRemoval);
        assert_eq!(outcome(&o), CheckOutcome::Incomplete);
        if mode == "wrong-ignore" {
            assert_eq!(o.evidence.issue.unwrap().kind, McpIssueKind::Inventory);
        }
        docker_absence::absent(&guest.resource.container_id);
    }
}
#[test]
#[ignore = "requires explicit cached image and reachable Docker"]
fn cancellation_preserves_precedence_and_removes_owned_guest() {
    let mut guest = Guest::new();
    let cancel = CancellationToken::default();
    let signal = cancel.clone();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(350));
        signal.cancel();
    });
    let started = Instant::now();
    let o = guest.probe(McpProtocol::Legacy20251125, "silent", 0, &cancel);
    worker.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(6));
    assert!(o.command.record.cancelled);
    assert_eq!(o.command.record.outcome, CommandOutcome::Cancelled);
    assert_eq!(o.command.record.exit_status, None);
    assert!(!o.container_available);
    assert_eq!(outcome(&o), CheckOutcome::Incomplete);
    docker_absence::absent(&guest.resource.container_id);
}
#[test]
#[ignore = "requires explicit cached image and reachable Docker"]
fn ownership_mismatch_refuses_removal_and_unrelated_guest_survives() {
    let mut guest = Guest::new();
    let unrelated = Guest::new();
    let authentic = guest.resource.clone();
    guest
        .resource
        .labels
        .insert("io.theseus.run".into(), "wrong-owner".into());
    let o = guest.probe(
        McpProtocol::Modern20260728,
        "ignore",
        0,
        &CancellationToken::default(),
    );
    guest.resource = authentic;
    assert!(!o.container_available);
    assert_eq!(o.command.record.exit_status, None);
    assert!(
        o.command
            .record
            .error
            .unwrap()
            .contains("ownership label mismatch")
    );
    assert_eq!(o.evidence.shutdown, McpShutdown::Unverified);
    for id in [
        &guest.resource.container_id,
        &unrelated.resource.container_id,
    ] {
        let output = Command::new("docker")
            .args(["inspect", "--format", "{{.State.Running}}", id])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"true\n");
        eprintln!("verified surviving {id}");
    }
}

#[test]
#[ignore = "requires explicit cached image and reachable Docker"]
fn truncated_successful_prefix_with_cancellation_remains_saveable_incomplete_evidence() {
    let mut guest = Guest::new();
    let cancel = CancellationToken::default();
    let signal = cancel.clone();
    let stdout = guest
        .store
        .root()
        .join("logs/probe-truncated-ignore-0-2026-07-28-stdout.log");
    let cancellation = std::thread::spawn(move || {
        let until = Instant::now() + Duration::from_secs(2);
        // The retained complete response prefix is exactly 408 bytes; let the
        // worker also acquire the extra discarded byte before cancelling.
        while Instant::now() < until && !std::fs::metadata(&stdout).is_ok_and(|m| m.len() == 408) {
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(50));
        signal.cancel();
    });
    let o = guest.probe(McpProtocol::Modern20260728, "truncated-ignore", 0, &cancel);
    cancellation.join().unwrap();
    assert!(o.command.record.cancelled && o.command.record.stdout.as_ref().unwrap().truncated);
    assert_eq!(o.evidence.stdout_bytes, 409);
    assert_eq!(outcome(&o), CheckOutcome::Incomplete);
    let check = recipe::Check {
        id: "discovery".into(),
        checkpoint: Checkpoint::Configured,
        expectation: recipe::Expectation::McpStdio {
            probe: o.evidence.probe.clone(),
        },
    };
    let mut result = theseus::checks::evaluate_mcp(&check, &o);
    result.command_ids = vec![o.command.record.id.clone()];
    let mut stage = StageReport::new("configured");
    stage.outcome = StageOutcome::Cancelled;
    stage.commands.push(o.command.record);
    stage.checks.push(result);
    let mut scenario = ScenarioReport::new("user-clean", ProfileName::User, ScenarioKind::Clean);
    scenario.stages.push(stage);
    let mut run = RunReport::new("truncated");
    run.cancelled = true;
    run.scenarios.push(scenario);
    guest.store.save(&run).unwrap();
    let loaded = report::load(guest.store.root()).unwrap();
    let evidence = loaded.scenarios[0].stages[0].checks[0]
        .mcp
        .as_ref()
        .unwrap();
    assert!(evidence.valid() && !evidence.protocol_complete);
    assert_eq!(evidence.issue.as_ref().unwrap().kind, McpIssueKind::Limit);
    assert_eq!(loaded.outcome, RunOutcome::Cancelled);
}

#[test]
fn transport_image_requires_explicit_full_immutable_identity() {
    assert!(immutable_image_id(&format!("sha256:{}", "a".repeat(64))));
    for bad in [
        "",
        "ubuntu:24.04",
        "sha256:abc",
        "sha256:",
        &format!("sha256:{}", "A".repeat(64)),
        &format!("sha256:{}\n", "a".repeat(64)),
    ] {
        assert!(!immutable_image_id(bad), "accepted {bad:?}");
    }
}
