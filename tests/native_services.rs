#![cfg(all(feature = "native-integration-test-support", target_os = "linux"))]
//! Real supplied private-manager product acceptance. An absent fixture is failure.
#[path = "support/native_service_setup.rs"]
mod setup;
#[path = "support/native_service_views.rs"]
mod views;
use setup::{Fault, IntegrationFixture};
use std::{
    path::PathBuf,
    process::{Command, Output},
};
use theseus::{
    engine::domain::StoredRun,
    model::{Checkpoint, RunOutcome, ScenarioKind, StepName},
    native::{integration::model::*, storage},
};
fn run(fixture: &IntegrationFixture) -> (Output, NativeIntegrationReport, PathBuf) {
    run_scenario(fixture, ScenarioKind::Clean)
}
fn run_scenario(
    fixture: &IntegrationFixture,
    scenario: ScenarioKind,
) -> (Output, NativeIntegrationReport, PathBuf) {
    fixture.scenario_recipe(scenario).unwrap();
    fixture.authority.validate().unwrap();
    fixture.recipe.recipe.validate().unwrap();
    assert!(fixture.ready_root.path().is_dir());
    let controller = &fixture.controller_path;
    let output = Command::new(controller)
        .args(["--fixture-graph"])
        .arg(&fixture.graph_path)
        .args(["--no-animation", "run", "--headless", "--recipe"])
        .arg(&fixture.recipe_path)
        .args(["--native-target"])
        .arg(fixture.root.path().join("launch.json"))
        .args(["--scenario", &scenario.to_string()])
        .output()
        .unwrap();
    let dirs = std::fs::read_dir(&fixture.output)
        .unwrap_or_else(|error| {
            panic!(
                "missing run: {error}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            )
        })
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(dirs.len(), 1, "one original run");
    let StoredRun::NativeIntegration(report) = storage::load_run(&dirs[0]).unwrap() else {
        panic!("schema2 report required")
    };
    if !output.status.success() {
        eprintln!(
            "saved failed service report: {}",
            serde_json::to_string_pretty(&report).unwrap()
        );
    }
    (output, report, dirs[0].clone())
}
#[test]
fn systemd_user_clean_installer_managed_service_uses_shared_product_path() {
    let fixture =
        IntegrationFixture::supplied_manager(ManagerKind::SystemdUser, Fault::None).unwrap();
    let (output, report, _) = run(&fixture);
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
            .all(|i| i.dispositions.complete())
    );
    assert!(
        report.integrations[0]
            .checks
            .iter()
            .any(|c| c.predicate
                == IntegrationPredicate::Service(ServicePredicate::ApplicationReady))
    );
}
#[test]
fn systemd_installer_missing_service_is_failed_without_harness_repair() {
    let fixture =
        IntegrationFixture::supplied_manager(ManagerKind::SystemdUser, Fault::MissingService)
            .unwrap();
    let (output, report, _) = run(&fixture);
    assert!(!output.status.success());
    assert_ne!(report.outcome, RunOutcome::Passed);
    assert!(report.integrations[0].checks.iter().any(|c| c.predicate
        == IntegrationPredicate::Service(ServicePredicate::Registration)
        && c.outcome == theseus::model::CheckOutcome::Failed));
    assert!(
        report.integrations[0].services[0].actions.is_empty(),
        "missing registration cannot grant helper start"
    );
}
#[test]
fn systemd_uninstaller_leaves_service_is_failed_before_and_after_provider_disposal() {
    let fixture =
        IntegrationFixture::supplied_manager(ManagerKind::SystemdUser, Fault::LeakedService)
            .unwrap();
    let (output, report, _) = run(&fixture);
    assert!(!output.status.success());
    assert_ne!(report.outcome, RunOutcome::Passed);
    assert!(
        report.integrations[0]
            .checks
            .iter()
            .any(|c| c.phase == IntegrationPhase::Uninstalled
                && c.predicate
                    == IntegrationPredicate::Service(ServicePredicate::RegistrationAbsent)
                && c.outcome == theseus::model::CheckOutcome::Failed)
    );
    let claims = &report.catalog.provider_claims;
    assert!(
        claims.iter().any(|r| r
            .receipt
            .starts_with("model-provider-reservation-released-")),
        "provider disposition is separate from failed SUT removal"
    );
}

#[test]
fn systemd_model_clean_exercises_real_helper_installer_and_application() {
    let receipt = PathBuf::from(
        std::env::var_os("THESEUS_NATIVE_SYSTEMD_MODEL_RECEIPT")
            .expect("explicit local model receipt required; cannot replace supplied-manager tests"),
    );
    let fixture = IntegrationFixture::model_manager(&receipt, Fault::None).unwrap();
    let (output, report, directory) = run(&fixture);
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(report.outcome, RunOutcome::Passed);
    views::assert_service_views(report.clone(), &directory);
    assert!(
        report.integrations[0]
            .checks
            .iter()
            .all(|c| c.outcome == theseus::model::CheckOutcome::Passed)
    );
}

#[test]
fn systemd_model_missing_and_leaked_registration_never_passes() {
    let receipt = PathBuf::from(
        std::env::var_os("THESEUS_NATIVE_SYSTEMD_MODEL_RECEIPT")
            .expect("explicit local model receipt required"),
    );
    for fault in [Fault::MissingService, Fault::LeakedService] {
        let fixture = IntegrationFixture::model_manager(&receipt, fault).unwrap();
        let (output, report, directory) = run(&fixture);
        assert!(!output.status.success());
        assert_ne!(report.outcome, RunOutcome::Passed);
        assert!(
            report.integrations[0]
                .checks
                .iter()
                .any(|c| c.outcome == theseus::model::CheckOutcome::Failed)
        );
        views::assert_service_views(report, &directory);
    }
}
#[test]
fn systemd_model_running_wrong_version_data_peer_or_nonce_never_passes_readiness() {
    let receipt = PathBuf::from(
        std::env::var_os("THESEUS_NATIVE_SYSTEMD_MODEL_RECEIPT")
            .expect("explicit local model receipt required"),
    );
    for fault in [
        Fault::WrongVersion,
        Fault::WrongData,
        Fault::WrongPeer,
        Fault::WrongNonce,
    ] {
        let fixture = IntegrationFixture::model_manager(&receipt, fault).unwrap();
        let (output, report, directory) = run(&fixture);
        assert!(!output.status.success());
        assert_ne!(report.outcome, RunOutcome::Passed);
        let observations = &report.integrations[0].services[0].observations;
        assert!(
            observations.iter().any(|o| matches!(
                &o.state,
                IntegrationProvenance::ManagerReported {
                    value: ServiceState::Present { running: true, .. },
                    ..
                }
            )),
            "manager really reported Running"
        );
        assert!(
            observations
                .iter()
                .filter_map(|o| o.readiness.as_ref())
                .any(|r| r.state != theseus_native_protocol::v2::ReadinessState::Ready),
            "independent application readiness must fail"
        );
        views::assert_service_views(report, &directory);
    }
}

fn assert_preexisting_is_untouched(fixture: IntegrationFixture) {
    let (unit, original) = fixture.place_preexisting_definition().unwrap();
    let (output, report, directory) = run(&fixture);
    assert!(!output.status.success());
    assert_ne!(report.outcome, RunOutcome::Passed);
    assert_eq!(
        std::fs::read(unit).unwrap(),
        original,
        "foreign preexisting definition remains untouched"
    );
    let journal = theseus::report::open_existing(&directory)
        .unwrap()
        .load_selected_journal()
        .unwrap();
    let theseus::engine::domain::StoredJournal::NativeIntegration(journal) = journal else {
        panic!("schema2 journal expected")
    };
    assert!(
        journal.operations.is_empty() && journal.integration_actions.is_empty(),
        "no SUT install or manager control can adopt an existing registration"
    );
}
#[test]
fn systemd_preexisting_or_wrong_owned_unit_is_untouched() {
    assert_preexisting_is_untouched(
        IntegrationFixture::supplied_manager(ManagerKind::SystemdUser, Fault::None).unwrap(),
    );
}
#[test]
fn systemd_model_preexisting_or_wrong_owned_unit_is_untouched() {
    let receipt = PathBuf::from(
        std::env::var_os("THESEUS_NATIVE_SYSTEMD_MODEL_RECEIPT")
            .expect("explicit local model receipt required"),
    );
    assert_preexisting_is_untouched(
        IntegrationFixture::model_manager(&receipt, Fault::None).unwrap(),
    );
}

fn assert_original_sut_transition(
    fixture: &IntegrationFixture,
    report: &NativeIntegrationReport,
    directory: &std::path::Path,
    scenario: ScenarioKind,
) {
    assert_eq!(report.outcome, RunOutcome::Passed);
    assert_eq!(report.catalog.scenarios.len(), 1);
    assert_eq!(report.integrations.len(), 1);
    let core = &report.catalog.scenarios[0];
    let transition_step = if scenario == ScenarioKind::Upgrade {
        StepName::Upgrade
    } else {
        StepName::Reinstall
    };
    let transition = core
        .stages
        .iter()
        .find(|s| s.step == Some(transition_step))
        .expect("original SUT transition")
        .commands
        .first()
        .unwrap();
    assert!(transition.failure.is_none());
    assert_eq!(
        transition.status.as_ref(),
        Some(&transition.command.expected_status)
    );
    let evidence = &report.integrations[0];
    assert!(
        evidence
            .checks
            .iter()
            .all(|c| c.outcome == theseus::model::CheckOutcome::Passed)
    );
    let phase = if scenario == ScenarioKind::Upgrade {
        IntegrationPhase::TargetInstalledAfterUpgrade
    } else {
        IntegrationPhase::Reinstalled
    };
    let ready = evidence.services[0]
        .observations
        .iter()
        .find(|o| o.binding.phase == phase && o.readiness.is_some())
        .expect("fresh transition readiness");
    let ready_fact = ready.readiness.as_ref().unwrap();
    assert_eq!(ready_fact.operation, transition.operation_id);
    assert_eq!(ready_fact.action_digest, transition.request_digest);
    assert_eq!(ready_fact.expectation.role, "configured");
    assert_eq!(ready_fact.expectation.artifact, "target");
    assert_eq!(
        ready_fact.expectation.artifact_sha256,
        fixture.recipe.core.recipe.target.sha256
    );
    assert!(evidence.checks.iter().any(|c| c.id
        == format!(
            "{}-runtime-precondition",
            if scenario == ScenarioKind::Upgrade {
                "upgrade"
            } else {
                "reinstall"
            }
        )
        && c.predicate == IntegrationPredicate::Service(ServicePredicate::Running)));
    let configured = core
        .stages
        .iter()
        .find(|s| s.checkpoint == Some(Checkpoint::Configured))
        .unwrap();
    let after = core
        .stages
        .iter()
        .find(|s| {
            s.checkpoint
                == Some(if scenario == ScenarioKind::Upgrade {
                    Checkpoint::TargetInstalled
                } else {
                    Checkpoint::Reinstalled
                })
        })
        .unwrap();
    let data = |stage: &theseus::native::model::NativeStage| {
        stage
            .native_files
            .iter()
            .find(|f| f.leaf == "data")
            .unwrap()
            .state
            .clone()
    };
    assert_eq!(
        data(configured),
        data(after),
        "configured data survives SUT transition"
    );
    let theseus::engine::domain::StoredJournal::NativeIntegration(journal) =
        theseus::report::open_existing(directory)
            .unwrap()
            .load_selected_journal()
            .unwrap()
    else {
        panic!("schema2 journal")
    };
    assert_eq!(journal.ownership.len(), 1, "one original ownership lease");
    let theseus::native::model::NativeOwnership::Acquired { lease, .. } =
        &journal.ownership[0].ownership
    else {
        panic!("original acquired lease")
    };
    assert!(journal.operations.iter().all(|o| &o.lease == lease));
    assert_eq!(
        journal.integration_actions.len(),
        1,
        "only initial declared rehearsal Start, no transition repair"
    );
    assert!(matches!(
        journal.integration_actions[0].request,
        IntegrationRequest::Service {
            control: ServiceAction::Start,
            ..
        }
    ));
    if scenario == ScenarioKind::Reinstall {
        let original = evidence.services[0]
            .observations
            .iter()
            .find(|o| o.binding.phase == IntegrationPhase::Configured && o.readiness.is_some())
            .unwrap();
        assert_eq!(original.binding.binding_digest.len(), 64);
        let digest = |o: &BoundServiceObservation| match &o.state {
            IntegrationProvenance::ManagerReported {
                value:
                    ServiceState::Present {
                        definition_digest, ..
                    },
                ..
            } => definition_digest.clone(),
            _ => panic!("complete original definition"),
        };
        assert_eq!(
            digest(original),
            digest(ready),
            "reinstall preserves exact configured definition"
        );
        for phase in [
            IntegrationPhase::ExerciseBeforeReinstall,
            IntegrationPhase::ExerciseAfterReinstall,
        ] {
            assert!(
                evidence.services[0]
                    .observations
                    .iter()
                    .any(|o| o.binding.phase == phase && o.readiness.is_some())
            );
        }
    }
}
#[test]
fn systemd_model_upgrade_sut_transitions_from_running_and_preserves_data() {
    let receipt = PathBuf::from(
        std::env::var_os("THESEUS_NATIVE_SYSTEMD_MODEL_RECEIPT")
            .expect("explicit private model receipt required"),
    );
    let fixture = IntegrationFixture::model_manager(&receipt, Fault::None).unwrap();
    let (output, report, directory) = run_scenario(&fixture, ScenarioKind::Upgrade);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_original_sut_transition(&fixture, &report, &directory, ScenarioKind::Upgrade);
    views::assert_service_views(report, &directory);
}
#[test]
fn systemd_model_reinstall_compares_original_definition_reader_and_data() {
    let receipt = PathBuf::from(
        std::env::var_os("THESEUS_NATIVE_SYSTEMD_MODEL_RECEIPT")
            .expect("explicit private model receipt required"),
    );
    let fixture = IntegrationFixture::model_manager(&receipt, Fault::None).unwrap();
    let (output, report, directory) = run_scenario(&fixture, ScenarioKind::Reinstall);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_original_sut_transition(&fixture, &report, &directory, ScenarioKind::Reinstall);
    views::assert_service_views(report, &directory);
}

#[test]
fn systemd_model_harness_stopped_transitions_are_explicit_and_do_not_claim_installer_stop() {
    let receipt = PathBuf::from(
        std::env::var_os("THESEUS_NATIVE_SYSTEMD_MODEL_RECEIPT")
            .expect("explicit private model receipt required"),
    );
    for scenario in [ScenarioKind::Upgrade, ScenarioKind::Reinstall] {
        let mut fixture = IntegrationFixture::model_manager(&receipt, Fault::None).unwrap();
        let service = fixture.recipe.recipe.integrations.service.as_mut().unwrap();
        if scenario == ScenarioKind::Upgrade {
            service.controls.upgrade_precondition = RuntimePrecondition::HarnessStopped;
        } else {
            service.controls.reinstall_precondition = RuntimePrecondition::HarnessStopped;
        }
        let bytes = serde_json::to_vec_pretty(&fixture.recipe.recipe).unwrap();
        std::fs::write(&fixture.recipe_path, &bytes).unwrap();
        fixture.recipe = theseus::native::integration::recipe::ValidatedIntegrationRecipe::decode(
            &bytes,
            fixture.recipe_path.clone(),
        )
        .unwrap();
        let (output, report, directory) = run_scenario(&fixture, scenario);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(report.outcome, RunOutcome::Passed);
        let evidence = &report.integrations[0];
        let precondition = evidence
            .checks
            .iter()
            .find(|c| {
                c.id == format!(
                    "{}-runtime-precondition",
                    if scenario == ScenarioKind::Upgrade {
                        "upgrade"
                    } else {
                        "reinstall"
                    }
                )
            })
            .unwrap();
        assert_eq!(precondition.origin, CheckOrigin::RehearsalControl);
        assert_eq!(
            precondition.predicate,
            IntegrationPredicate::Service(ServicePredicate::Stopped)
        );
        assert!(precondition.sut_operation.is_none());
        assert!(precondition.detail.contains("no installer-stop proof"));
        let theseus::engine::domain::StoredJournal::NativeIntegration(journal) =
            theseus::report::open_existing(&directory)
                .unwrap()
                .load_selected_journal()
                .unwrap()
        else {
            panic!("schema2 journal")
        };
        assert_eq!(
            journal.integration_actions.len(),
            2,
            "initial Start and explicit original-owner Stop only"
        );
        assert_eq!(
            journal
                .integration_actions
                .iter()
                .filter(|a| matches!(
                    a.request,
                    IntegrationRequest::Service {
                        control: ServiceAction::Stop,
                        ..
                    }
                ))
                .count(),
            1
        );
        views::assert_service_views(report, &directory);
    }
}

#[test]
fn systemd_model_failed_configure_cleanup_runs_original_uninstall_without_boundary_controls() {
    let receipt = PathBuf::from(
        std::env::var_os("THESEUS_NATIVE_SYSTEMD_MODEL_RECEIPT")
            .expect("explicit private model receipt required"),
    );
    for precondition in [
        RuntimePrecondition::Running,
        RuntimePrecondition::HarnessStopped,
    ] {
        let mut fixture =
            IntegrationFixture::model_manager(&receipt, Fault::ConfigureFails).unwrap();
        fixture
            .recipe
            .recipe
            .integrations
            .service
            .as_mut()
            .unwrap()
            .controls
            .uninstall_precondition = precondition;
        let (output, report, directory) = run_scenario(&fixture, ScenarioKind::Clean);
        assert!(
            !output.status.success(),
            "known Configure mismatch remains failed"
        );
        let stages = &report.catalog.scenarios[0].stages;
        assert!(stages.iter().any(|s| s.step == Some(StepName::Configure)
            && s.outcome == theseus::model::StageOutcome::Failed));
        let uninstalls = stages
            .iter()
            .filter(|s| s.step == Some(StepName::Uninstall))
            .collect::<Vec<_>>();
        assert_eq!(
            uninstalls.len(),
            1,
            "benign forward failure retains original SUT Uninstall"
        );
        assert_eq!(
            uninstalls[0].commands[0].status,
            Some(theseus::native::model::NativeStatus::PosixExit { code: 0 })
        );
        assert!(
            report.integrations[0].services[0].actions.is_empty(),
            "no Start or HarnessStopped Stop after forward failure"
        );
        assert!(report.integrations[0].checks.iter().any(|c| c.phase
            == IntegrationPhase::Uninstalled
            && c.predicate == IntegrationPredicate::Service(ServicePredicate::RegistrationAbsent)
            && c.outcome == theseus::model::CheckOutcome::Passed));
        let theseus::engine::domain::StoredJournal::NativeIntegration(journal) =
            theseus::report::open_existing(&directory)
                .unwrap()
                .load_selected_journal()
                .unwrap()
        else {
            panic!("schema2 journal")
        };
        assert!(journal.integration_actions.is_empty());
    }
}
