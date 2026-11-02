//! Required real Linux Docker parity for the actual production stdio server.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path, process::Command};
use theseus::{model::RunOutcome, report};

fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
#[ignore = "requires native Linux, trusted production binary and prepared immutable Docker image"]
fn production_serve_matches_direct_engine_success_failure_cancel_and_recovery() {
    assert_eq!(
        std::env::consts::OS,
        "linux",
        "native Linux Docker acceptance required"
    );
    let fixture = std::env::var_os("THESEUS_HOST_TEST_FIXTURE").expect("required trusted fixture");
    let metadata = read(&Path::new(&fixture).parent().unwrap().join("artifacts.json"));
    let binary = metadata["production"]["path"].as_str().unwrap();
    let binary_hash = format!("{:x}", Sha256::digest(std::fs::read(binary).unwrap()));
    assert_eq!(binary_hash, metadata["production"]["sha256"]);
    let image = std::env::var("THESEUS_MCP_TEST_IMAGE")
        .expect("explicit prepared immutable image required");
    let temporary = tempfile::tempdir().unwrap();
    let evidence = std::env::var_os("THESEUS_ACCEPTANCE_EVIDENCE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_path_buf())
        .join("production-server-parity");
    std::fs::create_dir_all(&evidence).unwrap();
    let output = Command::new("python3")
        .arg("tests/support/mcp_server_docker.py")
        .args([binary, evidence.to_str().unwrap(), &image])
        .output()
        .expect("required Python supervisor must spawn");
    std::fs::write(evidence.join("supervisor.stdout"), &output.stdout).unwrap();
    std::fs::write(evidence.join("supervisor.stderr"), &output.stderr).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let cases = read(&evidence.join("cases.json"));
    assert_eq!(cases.as_array().unwrap().len(), 4);
    for case in cases.as_array().unwrap() {
        let direct = report::load(
            Path::new(case["direct_report"].as_str().unwrap())
                .parent()
                .unwrap(),
        )
        .unwrap();
        let served = report::load(
            Path::new(case["server_report"].as_str().unwrap())
                .parent()
                .unwrap(),
        )
        .unwrap();
        let expected = match case["case"].as_str().unwrap() {
            "success" => RunOutcome::Passed,
            "command-failure" => RunOutcome::Failed,
            "cancellation" => RunOutcome::Cancelled,
            "teardown-recovery" => RunOutcome::Incomplete,
            other => panic!("unexpected case {other}"),
        };
        assert_eq!(direct.outcome, expected, "{case}");
        assert_eq!(served.outcome, direct.outcome, "{case}");
        assert_eq!(
            serde_json::to_value(served.counts()).unwrap(),
            serde_json::to_value(direct.counts()).unwrap(),
            "{case}"
        );
        assert_eq!(served.schema_version, direct.schema_version);
        assert_eq!(served.recipe_digest, direct.recipe_digest);
        assert_eq!(served.image, direct.image);
        assert_eq!(served.cancelled, direct.cancelled);
        assert_eq!(served.selected_profiles, direct.selected_profiles);
        assert_eq!(served.selected_scenarios, direct.selected_scenarios);
        assert_eq!(served.scenarios.len(), direct.scenarios.len());
        for (left, right) in served.scenarios.iter().zip(&direct.scenarios) {
            assert_eq!(left.outcome, right.outcome);
            assert_eq!(left.teardown, right.teardown);
            assert_eq!(left.stages.len(), right.stages.len());
            for (left, right) in left.stages.iter().zip(&right.stages) {
                assert_eq!(left.name, right.name);
                assert_eq!(left.outcome, right.outcome);
                assert_eq!(left.commands.len(), right.commands.len());
                assert_eq!(
                    left.checks
                        .iter()
                        .map(|check| (&check.id, &check.outcome))
                        .collect::<Vec<_>>(),
                    right
                        .checks
                        .iter()
                        .map(|check| (&check.id, &check.outcome))
                        .collect::<Vec<_>>()
                );
            }
        }
        let status = read(Path::new(case["status"].as_str().unwrap()));
        assert_eq!(
            status["engine_result"],
            json!({"outcome":served.outcome, "counts":served.counts()})
        );
        assert_eq!(status["state"], "finished");
        assert_eq!(status["cleanup"]["native"], "complete");
        assert_eq!(
            status["cleanup"]["recovery_required"],
            case["case"] == "teardown-recovery"
        );
        let journals = read(Path::new(case["journals"].as_str().unwrap()));
        let direct_resources = journals[0]["resources"].as_array().unwrap();
        let served_resources = journals[1]["resources"].as_array().unwrap();
        assert_eq!(direct_resources.len(), served_resources.len());
        for (direct, served) in direct_resources.iter().zip(served_resources) {
            assert_eq!(direct["removed"], served["removed"]);
            for resource in [direct, served] {
                assert_eq!(resource["labels"]["io.theseus.owner"], "v1");
                assert_eq!(resource["labels"]["io.theseus.scenario"], "user-clean");
                assert!(
                    resource["labels"]["io.theseus.run"]
                        .as_str()
                        .is_some_and(|id| !id.is_empty())
                );
            }
        }
    }
    assert_eq!(
        binary_hash,
        format!("{:x}", Sha256::digest(std::fs::read(binary).unwrap()))
    );
}
