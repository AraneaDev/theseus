use super::*;
use theseus_native_protocol::v2::{self, broker};

// Pure saved DTOs: no application, broker, manager, or helper is executed.
pub(super) fn pair(
    scenario: ScenarioKind,
    activated: bool,
) -> (NativeIntegrationReport, NativeIntegrationJournal) {
    let mut report = crate::native::integration::view::tests::report();
    let slot = ResourceSlot::new("desktop").unwrap();
    report.requirements = IntegrationRequirements {
        service: None,
        desktop: Some(DesktopRequirement {
            resource: slot.clone(),
            modes: vec![DesktopMode::OwnedExecutable],
            readiness: ReadinessRequirement {
                kind: ReadyKind::LocalChallenge,
                protocol: ReadyProtocol::TheseusFixtureReadyV1,
                deadline_ms: 1000,
            },
            lifetime: LifetimeRequirement::OwnedProcessTreeSettled,
        }),
    };
    report.compiled_requirements_digest = report.requirements.digest().unwrap();
    report.selected_scenarios = vec![scenario];
    report.catalog.scenarios[0].scenario = scenario;
    report.catalog.scenarios[0].ownership_id = "16".repeat(16);
    report.integrations[0].ownership_id = "16".repeat(16);
    let command = CatalogCommand {
        id: if scenario == ScenarioKind::Clean {
            "configure"
        } else {
            "install_target"
        }
        .into(),
        program: if scenario == ScenarioKind::Clean {
            CatalogProgram::Configure
        } else {
            CatalogProgram::InstallTarget
        },
        expected_status: NativeStatus::PosixExit { code: 0 },
        stdout_literal: None,
    };
    let mut operation = NativeOperation {
        ownership_id: "16".repeat(16),
        lease: "01".repeat(16),
        operation_id: "02".repeat(16),
        request_digest: String::new(),
        command: command.clone(),
        completion: Disposition::Complete {
            receipt: "original successful Run".into(),
        },
    };
    operation.request_digest = v2::request_digest(&operation.request().unwrap())
        .unwrap()
        .to_hex();
    let generation = broker::DesktopActivationTransition {
        definition: v2::DesktopDefinitionKey::CONFIGURED_TARGET,
        tuple_digest: "03".repeat(32),
        previous_activation: None,
        sut: broker::DesktopSutRunWitness {
            lease: operation.lease.clone(),
            operation: operation.operation_id.clone(),
            request_digest: operation.request_digest.clone(),
            program: command.program.program_id(),
        },
    };
    report.catalog.scenarios[0].stages = vec![NativeStage {
        name: "original SUT".into(),
        step: Some(match scenario {
            ScenarioKind::Clean => crate::model::StepName::Configure,
            ScenarioKind::Upgrade => crate::model::StepName::Upgrade,
            _ => crate::model::StepName::Reinstall,
        }),
        checkpoint: None,
        outcome: crate::model::StageOutcome::Passed,
        commands: vec![NativeCommandEvidence {
            command,
            operation_id: operation.operation_id.clone(),
            request_digest: operation.request_digest.clone(),
            status: Some(NativeStatus::PosixExit { code: 0 }),
            stdout: String::new(),
            stdout_complete: true,
            stdout_utf8: true,
            stderr_complete: true,
            stdout_log: None,
            stderr_log: None,
            failure: None,
        }],
        checks: vec![],
        native_files: vec![],
        failures: vec![],
    }];
    let header: broker::IntegrationBrokerHeader = serde_json::from_value(serde_json::json!({
            "profile":"native_integration_broker_v1","binding":"04".repeat(16),"manifest_digest":report.manifest_digest,"workspace_marker":"05".repeat(16),"os":"linux","target_id":"original-target","ownership_id":"16".repeat(16),"provider_epoch":"06".repeat(32),"session_id":"original-session","session_epoch":"07".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"x11","display":":92","authority_file":"/fixture/auth","bus":"/fixture/bus.sock"},"allowed_modes":["owned_executable"]
        })).unwrap();
    let mut request: broker::DesktopRequest = serde_json::from_value(serde_json::json!({
            "helper_lease":operation.lease,"helper_operation":"08".repeat(16),"broker_lease":"09".repeat(16),"binding_digest":header.digest().unwrap().to_hex(),"activation_nonce":"0a".repeat(16),"slot":"desktop","prior_binding_digest":"0b".repeat(32),"action":{"kind":"activate","transition":generation,"mode":"owned_executable","entry_digest":"0c".repeat(32),"expectation_digest":"0d".repeat(32),"close_binding":{"operation":"0e".repeat(16),"action_digest":"00".repeat(32),"nonce":"0f".repeat(16)}}
        })).unwrap();
    let close_digest = request.close_request().unwrap().digest().unwrap().to_hex();
    if let broker::DesktopAction::Activate { close_binding, .. } = &mut request.action {
        close_binding.action_digest = close_digest;
    }
    let original = broker::BrokerBind {
        ownership_id: "16".repeat(16),
        helper_lease: operation.lease.clone(),
        helper_operation: "10".repeat(16),
        slot: v2::ResourceSlot::new("desktop").unwrap(),
        broker_intent: "11".repeat(16),
        expected_header: header.clone(),
    };
    let unavailable = || IntegrationProvenance::Unavailable {
        reason: "not sampled by this observation".into(),
    };
    let state = broker::BrokerState {
        owned_lifetime: None,
        native_lifetime: None,
        effect: broker::BrokerEffect::Receipt {
            receipt: broker::BrokerEffectReceipt {
                evidence: broker::DesktopEvidence::SourceOwnedExecutable,
                acknowledged: true,
                receipt_digest: "12".repeat(32),
            },
        },
        owner: broker::BrokerOwner::Settled,
        primary: broker::BrokerPrimary::Exited,
        descendants: broker::BrokerDescendants::Joined,
        cleanup_complete: true,
        quarantined: false,
    };
    let observation = BoundDesktopObservation {
        generation: Some(desktop_generation_evidence(&generation)),
        entry_valid: Some(true),
        broker_state: activated.then_some(state),
        readiness: None,
        evidence: activated.then_some(broker::DesktopEvidence::SourceOwnedExecutable),
        binding: ObservationBinding {
            id: "original-observation".into(),
            run_id: report.run_id.clone(),
            scenario_id: "clean".into(),
            ownership_id: "16".repeat(16),
            lease: operation.lease.clone(),
            slot: slot.clone(),
            request_id: "original-query".into(),
            action: Some(if activated {
                request.helper_operation.clone()
            } else {
                operation.operation_id.clone()
            }),
            binding_digest: request.prior_binding_digest.clone(),
            source_profile: super::super::PROFILE.into(),
            source_version: "fixture".into(),
            phase: match scenario {
                ScenarioKind::Clean => IntegrationPhase::Configured,
                ScenarioKind::Upgrade => IntegrationPhase::TargetInstalledAfterUpgrade,
                _ => IntegrationPhase::Reinstalled,
            },
            definition: Some(DefinitionKey {
                role: DefinitionRole::Configured,
                artifact: ArtifactRole::Target,
            }),
            sequence: 1,
            started_ms: 0,
            finished_ms: 1,
            complete: true,
        },
        entry_present: IntegrationProvenance::Observed {
            value: true,
            source: "original inspected entry".into(),
        },
        activated: unavailable(),
        primary_exited: unavailable(),
        descendant_scope_settled: unavailable(),
    };
    report.integrations[0].services.clear();
    report.integrations[0].checks.clear();
    report.integrations[0].desktops = vec![DesktopEvidence {
        slot: slot.clone(),
        observations: vec![observation],
        actions: vec![],
    }];
    let mut journal = NativeIntegrationJournal {
        kind: "theseus_native_journal".into(),
        schema: 2,
        profile: report.profile.clone(),
        run_id: report.run_id.clone(),
        recipe_digest: report.recipe_digest.clone(),
        core_recipe_digest: report.core_recipe_digest.clone(),
        core_manifest_digest: report.core_manifest_digest.clone(),
        integration_manifest_digest: report.integration_manifest_digest.clone(),
        manifest_digest: report.manifest_digest.clone(),
        requirements: report.requirements.clone(),
        compiled_requirements_digest: report.compiled_requirements_digest.clone(),
        selected_scenarios: report.selected_scenarios.clone(),
        launch_fingerprint: Some("13".repeat(32)),
        ownership: vec![NativeResource {
            scenario_id: "clean".into(),
            ownership_id: "16".repeat(16),
            catalog_authority: NativeCatalogAuthority::Valid,
            ownership: NativeOwnership::Acquired {
                lease: operation.lease.clone(),
                prepared: NativeIntent {
                    intent: "14".repeat(16),
                    binding: header.binding.clone(),
                    manifest_digest: report.manifest_digest.clone(),
                    workspace_marker: header.workspace_marker.clone(),
                    reset_epoch: "original-epoch".into(),
                    publication_scope: "file_and_directory_synced".into(),
                },
            },
        }],
        operations: vec![operation],
        provider_calls: vec![],
        releases: vec![],
        recovery_attempts: vec![],
        failures: vec![],
        integration_ownership: vec![],
        integration_actions: vec![],
        broker_bindings: vec![],
        readiness_probes: vec![],
        integration_recoveries: vec![],
    };
    if activated {
        let binding = ResourceBindingSnapshot {
            digest: request.prior_binding_digest.clone(),
            basis: BindingBasis::AdmittedActivation,
            evidence_ids: vec!["original-observation".into()],
            definition: Some(DefinitionKey::from_desktop_wire(generation.definition)),
            artifact_commitment: "17".repeat(32),
            namespace_or_session_receipt_digest: header.digest().unwrap().to_hex(),
            original_action: Some(generation.sut.operation.clone()),
        };
        journal
            .integration_ownership
            .push(IntegrationResourceRecord {
                key: IntegrationResourceKey {
                    scenario_id: "clean".into(),
                    ownership_id: "16".repeat(16),
                    slot: slot.clone(),
                },
                kind: IntegrationResourceKind::Desktop,
                manifest_digest: report.manifest_digest.clone(),
                initial_binding: binding.clone(),
                binding_history: vec![binding.clone()],
                current_binding_digest: binding.digest,
                revocation: None,
                disposition: Disposition::Unknown {
                    reason: "original local retirement not published".into(),
                },
            });
        let wire = v2::Request::DesktopAction {
            request: Box::new(request.clone()),
        };
        journal.integration_actions.push(IntegrationActionRecord {
            ownership_id: "16".repeat(16),
            lease: request.helper_lease.clone(),
            slot: slot.clone(),
            operation: request.helper_operation.clone(),
            request_digest: v2::request_digest(&wire).unwrap().to_hex(),
            prior_binding_digest: request.prior_binding_digest.clone(),
            broker_identity: Some(
                serde_json::from_value(serde_json::to_value(request.identity().unwrap()).unwrap())
                    .unwrap(),
            ),
            request: IntegrationRequest::Desktop {
                lease: request.helper_lease.clone(),
                operation: request.helper_operation.clone(),
                slot: slot.clone(),
                prior_binding_digest: request.prior_binding_digest.clone(),
                mode: DesktopMode::OwnedExecutable,
                request: Box::new(request),
            },
            completion: Disposition::Unknown {
                reason: "original reply not cached".into(),
            },
            terminal: None,
        });
        journal.broker_bindings.push(BrokerBindingRecord {
            original: original.clone(),
            ownership_id: original.ownership_id.clone(),
            helper_lease: original.helper_lease.clone(),
            slot,
            original_intent: BrokerIntentRecord {
                intent: original.broker_intent.clone(),
                binding: header.binding.clone(),
                manifest_digest: header.manifest_digest.clone(),
                workspace_marker: header.workspace_marker.clone(),
            },
            publication_scope: Some(broker::IntentPublicationScope::FileAndDirectorySynced),
            original_intent_reference: format!("{}.json", original.broker_intent),
            expected_header_digest: header.digest().unwrap().to_hex(),
            session_id: header.session_id,
            session_epoch: header.session_epoch,
            broker_lease: Some("09".repeat(16)),
            disposition: Disposition::Complete {
                receipt: "original durable broker mapping".into(),
            },
        });
    }
    validate_pair(&report, &journal).unwrap();
    (report, journal)
}

#[test]
fn ordinary_saved_entry_generation_cannot_disappear() {
    for scenario in [ScenarioKind::Upgrade, ScenarioKind::Reinstall] {
        let (mut report, journal) = pair(scenario, false);
        report.integrations[0].desktops[0].observations[0].generation = None;
        assert!(
            report.validate().is_err(),
            "ordinary public entry lost its keyed generation"
        );
        assert!(validate_pair(&report, &journal).is_err());
    }
}

#[test]
fn keyed_original_activation_requires_public_generation_even_with_legacy_binding() {
    let (mut report, journal) = pair(ScenarioKind::Clean, true);
    let observation = &mut report.integrations[0].desktops[0].observations[0];
    // Clearing the public key as well must not disguise the private keyed Activate.
    observation.binding.definition = None;
    observation.generation = None;
    report.validate().unwrap();
    assert!(
        validate_pair(&report, &journal).is_err(),
        "keyed original activation lost its public projection"
    );
}

#[test]
fn keyed_original_activation_requires_exact_nonce_free_projection() {
    for scenario in [ScenarioKind::Upgrade, ScenarioKind::Reinstall] {
        let (mut report, journal) = pair(scenario, true);
        report.integrations[0].desktops[0].observations[0]
            .generation
            .as_mut()
            .unwrap()
            .tuple_digest = "15".repeat(32);
        assert!(
            validate_pair(&report, &journal).is_err(),
            "changed tuple borrowed original Activate"
        );
    }
}

#[test]
fn baseline_uninstalled_and_legacy_clean_allow_absent_generation() {
    for (scenario, phase) in [
        (ScenarioKind::Upgrade, IntegrationPhase::Baseline),
        (ScenarioKind::Reinstall, IntegrationPhase::Uninstalled),
        (ScenarioKind::Clean, IntegrationPhase::Configured),
    ] {
        let (mut report, journal) = pair(scenario, false);
        let observation = &mut report.integrations[0].desktops[0].observations[0];
        observation.generation = None;
        observation.binding.definition = None;
        observation.binding.phase = phase;
        validate_pair(&report, &journal).unwrap();
    }
}

#[test]
fn legacy_singleton_activate_keeps_absent_public_generation() {
    let (mut report, mut journal) = pair(ScenarioKind::Clean, true);
    let observation = &mut report.integrations[0].desktops[0].observations[0];
    observation.binding.definition = None;
    observation.generation = None;
    let action = &mut journal.integration_actions[0];
    let IntegrationRequest::Desktop { request, .. } = &mut action.request else {
        unreachable!()
    };
    if let broker::DesktopAction::Activate { transition, .. } = &mut request.action {
        *transition = None;
    }
    let close_digest = request.close_request().unwrap().digest().unwrap().to_hex();
    if let broker::DesktopAction::Activate { close_binding, .. } = &mut request.action {
        close_binding.action_digest = close_digest;
    }
    action.broker_identity = Some(
        serde_json::from_value(serde_json::to_value(request.identity().unwrap()).unwrap()).unwrap(),
    );
    action.request_digest = v2::request_digest(&action.request.wire().unwrap())
        .unwrap()
        .to_hex();
    validate_pair(&report, &journal).unwrap();
}
