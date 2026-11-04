#![cfg(feature = "native-integration-test-support")]
//! Mandatory actual native manager acceptance; missing supplied authority fails setup.
#[path = "support/native_service_setup.rs"]
#[allow(dead_code)]
mod setup;
#[path = "support/native_service_views.rs"]
mod views;

#[test]
fn native_supplied_receipt_requires_explicit_platform_layout_and_instance() {
    setup::qualify_native_receipt_bytes(
        br#"{"kind":"theseus_supplied_manager_receipt","schema":1}"#,
    )
    .expect_err("no implicit namespace, root, native authority or instance selection");
}

#[test]
fn native_supplied_receipt_preserves_exact_launchd_authority_and_rejects_implicit_inputs() {
    // Synthetic grammar test only: never a native qualification receipt.
    let mut receipt = serde_json::json!({
        "kind":"theseus_supplied_manager_receipt", "schema":1, "manager":"launchd_user",
        "manager_unique_name":"supplied-user-domain", "namespace_receipt":"operator-original-reservation",
        "native_fixture":{"root":"/tmp/theseus-fixture", "ready_root":"/tmp/th-ready-fixture", "cooperative_instance":"operator-original-instance"},
        "authority":{"kind":"launchd", "slot":"service", "domain":{"kind":"user","uid":501},
        "label":"theseus.fixture", "definition_root":"/tmp/theseus-definitions", "exclusive_load_roots":["/tmp/theseus-definitions"],
        "launchctl":"/bin/launchctl", "qualification":{"os_version":"synthetic-grammar", "os_build":"synthetic-grammar",
        "api_profile":"launchctl-qualified-v1", "receipt_sha256":"11".repeat(32),
        "launchctl_fingerprint":std::iter::once(1u8).chain(std::iter::repeat_n(0,56)).collect::<Vec<_>>(),
        "absence_profile":{"profile":"launchctl-user-no-service-v1", "exit_code":113,
        "stderr":"Could not find service \"theseus.fixture\" in domain for user: 501\n", "receipt_sha256":"22".repeat(32)}},
        "managed_by":"installer", "sut_effects":["registration","configuration","removal"], "allowed_actions":["inspect","start","stop"]}
    });
    setup::qualify_native_receipt_bytes(&serde_json::to_vec(&receipt).unwrap()).unwrap();
    for field in ["root", "ready_root", "cooperative_instance"] {
        let mut missing = receipt.clone();
        missing["native_fixture"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            setup::qualify_native_receipt_bytes(&serde_json::to_vec(&missing).unwrap()).is_err(),
            "{field}"
        );
    }
    receipt["authority"]["domain"]["kind"] = "gui".into();
    assert!(setup::qualify_native_receipt_bytes(&serde_json::to_vec(&receipt).unwrap()).is_err());
}

#[test]
fn native_acl_policy_refuses_inherited_extra_or_content_write_executable_grants() {
    setup::validate_native_acl_policy(
        "S-1-5-21-10-20-30-1001",
        false,
        &[
            ("S-1-5-18", 0x1200a9, 0),
            ("S-1-5-21-10-20-30-1001", 0x1301a9, 0),
        ],
    )
    .expect_err("unprotected DACL");
    for grants in [
        vec![
            ("S-1-5-18", 0x1200a9, 0),
            ("S-1-5-21-10-20-30-1001", 0x1301a9, 0x10),
        ],
        vec![
            ("S-1-5-18", 0x1200a9, 0),
            ("S-1-5-21-10-20-30-1001", 0x1301a9, 0),
            ("S-1-5-32-545", 0x1200a9, 0),
        ],
        vec![
            ("S-1-5-18", 0x1200a9, 0),
            ("S-1-5-21-10-20-30-1001", 0x1f01ff, 0),
        ],
    ] {
        assert!(
            setup::validate_native_acl_policy("S-1-5-21-10-20-30-1001", true, &grants).is_err()
        );
    }
    setup::validate_native_acl_policy(
        "S-1-5-21-10-20-30-1001",
        true,
        &[
            ("S-1-5-18", 0x1200a9, 0),
            ("S-1-5-21-10-20-30-1001", 0x1301a9, 0),
        ],
    )
    .unwrap();
}

#[cfg(any(target_os = "macos", windows))]
mod native {
    use super::*;
    use setup::{Fault, IntegrationFixture};
    use std::{
        path::PathBuf,
        process::{Command, Output},
    };
    use theseus::{
        engine::domain::StoredRun,
        model::{RunOutcome, ScenarioKind, StepName},
        native::{integration::model::*, storage},
    };

    fn manager() -> ManagerKind {
        #[cfg(target_os = "macos")]
        {
            ManagerKind::LaunchdUser
        }
        #[cfg(windows)]
        {
            ManagerKind::WindowsScm
        }
    }
    pub(super) fn run(fixture: &IntegrationFixture) -> (Output, NativeIntegrationReport, PathBuf) {
        run_scenario(fixture, ScenarioKind::Clean)
    }
    fn run_scenario(
        fixture: &IntegrationFixture,
        scenario: ScenarioKind,
    ) -> (Output, NativeIntegrationReport, PathBuf) {
        fixture.scenario_recipe(scenario).unwrap();
        fixture.authority.validate().unwrap();
        fixture.recipe.recipe.validate().unwrap();
        let output = Command::new(&fixture.controller_path)
            .arg("--fixture-graph")
            .arg(&fixture.graph_path)
            .args(["--no-animation", "run", "--headless", "--recipe"])
            .arg(&fixture.recipe_path)
            .arg("--native-target")
            .arg(fixture.root.path().join("launch.json"))
            .args(["--scenario", &scenario.to_string()])
            .output()
            .unwrap();
        let directories = std::fs::read_dir(&fixture.output)
            .unwrap_or_else(|error| {
                panic!(
                    "missing saved run: {error}; stderr={}",
                    String::from_utf8_lossy(&output.stderr)
                )
            })
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(directories.len(), 1, "one original product run");
        let StoredRun::NativeIntegration(report) = storage::load_run(&directories[0]).unwrap()
        else {
            panic!("actual schema2 report required")
        };
        (output, report, directories[0].clone())
    }
    fn clean() {
        let fixture = IntegrationFixture::supplied_manager(manager(), Fault::None).unwrap();
        let (output, report, directory) = run(&fixture);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(report.outcome, RunOutcome::Passed);
        assert!(
            report
                .integrations
                .iter()
                .all(|scenario| scenario.dispositions.complete())
        );
        assert!(
            report.integrations[0]
                .checks
                .iter()
                .all(|check| check.outcome == theseus::model::CheckOutcome::Passed)
        );
        views::assert_service_views(report, &directory);
        assert!(!fixture.finish_native_fixture().unwrap());
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn launchd_user_clean_keeps_loaded_configuration_runtime_and_readiness_distinct() {
        clean();
    }
    #[cfg(windows)]
    #[test]
    fn scm_own_process_clean_observes_config_status_readiness_and_delete_pending() {
        clean();
    }

    fn assert_successful_sut(
        report: &NativeIntegrationReport,
        step: StepName,
    ) -> &theseus::native::model::NativeCommandEvidence {
        let command = report.catalog.scenarios[0]
            .stages
            .iter()
            .find(|s| s.step == Some(step))
            .expect("matching original SUT Run")
            .commands
            .first()
            .unwrap();
        assert!(
            command.failure.is_none(),
            "intended SUT must complete successfully"
        );
        assert_eq!(
            command.status.as_ref(),
            Some(&command.command.expected_status)
        );
        command
    }
    pub(super) fn assert_readiness_fault(
        fixture: &IntegrationFixture,
        report: &NativeIntegrationReport,
        expected: &str,
    ) {
        assert_successful_sut(report, StepName::InstallTarget);
        assert_successful_sut(report, StepName::Configure);
        let evidence = &report.integrations[0];
        let observation = evidence.services[0]
            .observations
            .iter()
            .find(|o| {
                o.readiness
                    .as_ref()
                    .is_some_and(|r| r.failure.as_deref() == Some(expected))
            })
            .expect("selected readiness fault must be observed");
        assert!(
            matches!(
                observation.state,
                IntegrationProvenance::ManagerReported {
                    value: ServiceState::Present {
                        registered: true,
                        configured: true,
                        running: true,
                        ..
                    },
                    ..
                }
            ),
            "same bound observation must carry manager Running"
        );
        assert!(observation.binding.complete);
        let ready = observation.readiness.as_ref().unwrap();
        let (manifest, _) = fixture.native_input().unwrap();
        let configured_peer = manifest.readiness.as_ref().unwrap();
        assert_eq!(
            ready.state,
            theseus_native_protocol::v2::ReadinessState::Failed
        );
        assert_eq!(
            ready.resource_binding_digest,
            observation.binding.binding_digest
        );
        assert_eq!(
            ready.instance_binding_digest,
            observation.binding.binding_digest
        );
        assert_eq!(
            observation.binding.action.as_deref(),
            Some(ready.operation.as_str())
        );
        assert!(!ready.challenge_digest.is_empty());
        if expected == "WrongPeer" {
            assert!(!ready.authenticated_instance);
            assert!(
                ready.response_digest.is_none(),
                "peer rejected before challenge disclosure"
            );
            assert!(
                ready.peer_uid.is_some(),
                "actual Unix peer credentials measured"
            );
            assert_ne!(
                ready.peer_uid, configured_peer.peer_uid,
                "measured peer must differ from the deliberately wrong expected UID"
            );
        } else {
            assert!(
                ready.response_digest.is_some(),
                "complete decoded response required"
            );
            assert_eq!(ready.authenticated_instance, expected != "WrongNonce");
            assert!(ready.peer_uid.is_some() || ready.peer_sid.as_deref() == Some("S-1-5-18"));
            assert_eq!(ready.peer_uid, configured_peer.peer_uid);
            assert_eq!(ready.peer_sid, configured_peer.peer_sid);
            if expected == "WrongVersion" {
                assert_ne!(
                    ready.version_literal.as_deref(),
                    Some(ready.expectation.version_literal.as_str())
                );
            }
            if expected == "WrongData" {
                assert_ne!(
                    ready.data_sha256.as_deref(),
                    Some(ready.expectation.data_sha256.as_str())
                );
            }
        }
        assert!(
            evidence.checks.iter().any(|c| c.predicate
                == IntegrationPredicate::Service(ServicePredicate::ApplicationReady)
                && c.outcome == theseus::model::CheckOutcome::Failed
                && c.observation_ids.contains(&observation.binding.id)),
            "Failed readiness check references this exact observed rejection"
        );
    }
    #[test]
    fn native_upgrade_sut_transitions_from_running_and_preserves_data() {
        let fixture = IntegrationFixture::supplied_manager(manager(), Fault::None).unwrap();
        let (output, report, directory) = run_scenario(&fixture, ScenarioKind::Upgrade);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(report.outcome, RunOutcome::Passed);
        let transition = assert_successful_sut(&report, StepName::Upgrade);
        let ready = report.integrations[0].services[0]
            .observations
            .iter()
            .find(|o| {
                o.binding.phase == IntegrationPhase::TargetInstalledAfterUpgrade
                    && o.readiness.is_some()
            })
            .unwrap()
            .readiness
            .as_ref()
            .unwrap();
        assert_eq!(ready.operation, transition.operation_id);
        assert_eq!(ready.expectation.role, "configured");
        assert_eq!(ready.expectation.artifact, "target");
        views::assert_service_views(report, &directory);
        assert!(!fixture.finish_native_fixture().unwrap());
    }
    #[test]
    fn native_reinstall_compares_original_definition_reader_and_data() {
        let fixture = IntegrationFixture::supplied_manager(manager(), Fault::None).unwrap();
        let (output, report, directory) = run_scenario(&fixture, ScenarioKind::Reinstall);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(report.outcome, RunOutcome::Passed);
        let transition = assert_successful_sut(&report, StepName::Reinstall);
        let observations = &report.integrations[0].services[0].observations;
        let original = observations
            .iter()
            .find(|o| o.binding.phase == IntegrationPhase::Configured && o.readiness.is_some())
            .unwrap()
            .readiness
            .as_ref()
            .unwrap();
        let repeated = observations
            .iter()
            .find(|o| o.binding.phase == IntegrationPhase::Reinstalled && o.readiness.is_some())
            .unwrap()
            .readiness
            .as_ref()
            .unwrap();
        assert_eq!(repeated.operation, transition.operation_id);
        assert_eq!(repeated.expectation, original.expectation);
        for phase in [
            IntegrationPhase::ExerciseBeforeReinstall,
            IntegrationPhase::ExerciseAfterReinstall,
        ] {
            assert!(
                observations
                    .iter()
                    .any(|o| o.binding.phase == phase && o.readiness.is_some())
            );
        }
        views::assert_service_views(report, &directory);
        assert!(!fixture.finish_native_fixture().unwrap());
    }
    #[test]
    fn native_installer_missing_and_leaked_registration_never_passes() {
        for fault in [Fault::MissingService, Fault::LeakedService] {
            let fixture = IntegrationFixture::supplied_manager(manager(), fault).unwrap();
            let (output, report, directory) = run(&fixture);
            assert!(!output.status.success());
            assert_ne!(report.outcome, RunOutcome::Passed);
            let missing = matches!(fault, Fault::MissingService);
            let command = assert_successful_sut(
                &report,
                if missing {
                    StepName::InstallTarget
                } else {
                    StepName::Uninstall
                },
            );
            let phase = if missing {
                IntegrationPhase::TargetInstalledInitial
            } else {
                IntegrationPhase::Uninstalled
            };
            let predicate = if missing {
                ServicePredicate::Registration
            } else {
                ServicePredicate::RegistrationAbsent
            };
            assert!(
                report.integrations[0]
                    .checks
                    .iter()
                    .any(|c| c.phase == phase
                        && c.predicate == IntegrationPredicate::Service(predicate)
                        && c.outcome == theseus::model::CheckOutcome::Failed
                        && c.sut_operation.as_deref() == Some(command.operation_id.as_str()))
            );
            if missing {
                assert!(
                    report.integrations[0].services[0].actions.is_empty(),
                    "missing registration grants no Start"
                );
            } else {
                assert!(
                    !report.catalog.provider_claims.is_empty(),
                    "provider disposition retains prior failed removal"
                );
            }
            views::assert_service_views(report, &directory);
            assert_eq!(
                fixture.finish_native_fixture().unwrap(),
                !missing,
                "independent teardown retains the actual leaked creator; missing registration grants no owner"
            );
        }
    }
    #[test]
    fn native_running_wrong_version_data_or_nonce_never_passes_readiness() {
        for fault in [Fault::WrongVersion, Fault::WrongData, Fault::WrongNonce] {
            let fixture = IntegrationFixture::supplied_manager(manager(), fault).unwrap();
            let (output, report, directory) = run(&fixture);
            assert!(!output.status.success());
            assert_ne!(report.outcome, RunOutcome::Passed);
            assert_readiness_fault(
                &fixture,
                &report,
                match fault {
                    Fault::WrongVersion => "WrongVersion",
                    Fault::WrongData => "WrongData",
                    Fault::WrongNonce => "WrongNonce",
                    _ => unreachable!(),
                },
            );
            views::assert_service_views(report, &directory);
            assert!(
                fixture.finish_native_fixture().unwrap(),
                "readiness rejection leaves the original fixture creator for separate teardown"
            );
        }
    }
}

#[cfg(any(target_os = "macos", windows))]
#[path = "support/native_service_platform_owners.rs"]
mod owners;

#[cfg(target_os = "macos")]
#[test]
fn launchd_running_wrong_native_peer_never_passes_readiness() {
    let fixture = setup::IntegrationFixture::supplied_manager(
        theseus::native::integration::model::ManagerKind::LaunchdUser,
        setup::Fault::WrongPeer,
    )
    .unwrap();
    let (output, report, directory) = native::run(&fixture);
    assert!(!output.status.success());
    assert_ne!(report.outcome, theseus::model::RunOutcome::Passed);
    native::assert_readiness_fault(&fixture, &report, "WrongPeer");
    views::assert_service_views(report, &directory);
    assert!(
        fixture.finish_native_fixture().unwrap(),
        "readiness rejection leaves the original fixture creator for separate teardown"
    );
}
