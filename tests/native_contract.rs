use theseus::native::{model::*, recipe::*};

#[test]
fn native_namespaces_cannot_be_read_as_legacy_journals() {
    let journal = NativeJournal::new("run-1");
    let bytes = journal.encode().unwrap();
    assert!(NativeJournal::decode(&bytes).is_ok());
    assert!(serde_json::from_slice::<theseus::model::RunJournal>(&bytes).is_err());
    let mut mixed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    mixed["resources"] = serde_json::json!([]);
    assert!(NativeJournal::decode(&serde_json::to_vec(&mixed).unwrap()).is_err());
    assert!(NativeJournal::decode(br#"{"kind":"theseus_native_journal","kind":"theseus_native_journal","schema":1,"run_id":"r","ownership":[],"failures":[]}"#).is_err());
}

#[test]
fn native_status_keeps_os_domains_and_rejects_unknown_fields() {
    let status = NativeStatus::WindowsExit { code: u32::MAX };
    let encoded = serde_json::to_vec(&status).unwrap();
    assert_eq!(
        serde_json::from_slice::<NativeStatus>(&encoded).unwrap(),
        status
    );
    assert!(serde_json::from_str::<NativeStatus>(r#"{"kind":"posix_exit","code":256}"#).is_err());
    assert!(
        serde_json::from_str::<NativeStatus>(r#"{"kind":"windows_exit","code":0,"uid":0}"#)
            .is_err()
    );
}

#[test]
fn prefix_revocation_is_sticky_and_blocks_cleanup_commands_too() {
    let mut authority = NativeCatalogAuthority::Valid;
    authority.revoke(PrefixAuthorityFailure::Renamed);
    authority.revoke(PrefixAuthorityFailure::MappingUnproved);
    assert_eq!(
        authority,
        NativeCatalogAuthority::Revoked(PrefixAuthorityFailure::Renamed)
    );
    for program in CATALOG_IDS {
        assert!(authority.authorize(program).is_err());
    }
}

#[test]
fn recipe_refuses_shell_and_unsupported_scenarios() {
    assert!(
        ValidatedNativeRecipe::decode(
            br#"{"kind":"theseus_native_recipe","schema":1,"shell":"true"}"#,
            "native.json".into()
        )
        .is_err()
    );
}

fn command(program: CatalogProgram) -> CatalogCommand {
    CatalogCommand {
        id: program.as_str().into(),
        program,
        expected_status: NativeStatus::PosixExit { code: 0 },
        stdout_literal: None,
    }
}
fn initial_report() -> NativeReport {
    NativeReport {
        kind: "theseus_native_report".into(),
        schema: 1,
        run_id: "run-1".into(),
        recipe_digest: "1".repeat(64),
        scenarios: vec![],
        identities: vec![],
        provider_claims: vec![],
        failures: vec![],
        outcome: theseus::model::RunOutcome::Incomplete,
    }
}
fn preparing() -> NativeResource {
    NativeResource {
        scenario_id: "native-clean".into(),
        ownership_id: "owner-1".into(),
        ownership: NativeOwnership::Preparing {
            provider: "inert".into(),
            invocation_id: "prepare-1".into(),
            receipt: None,
        },
        catalog_authority: NativeCatalogAuthority::Valid,
    }
}

#[test]
fn projector_publishes_only_native_filenames_before_provider_token() {
    use theseus::engine::{domain::*, projector::RunProjector};
    let temp = tempfile::tempdir().unwrap();
    let store = theseus::report::create(&temp.path().join("run")).unwrap();
    let mut projector = RunProjector::new(
        &store,
        StoredRun::Native(initial_report()),
        StoredJournal::Native(NativeJournal::new("run-1")),
    )
    .unwrap();
    projector.bind_launch(&"a".repeat(64)).unwrap();
    let invocation = ProviderInvocation {
        ownership_id: "owner-1".into(),
        provider: "inert".into(),
        invocation_id: "prepare-1".into(),
        action: ProviderAction::Prepare,
        target_id: "inert-target".into(),
        baseline_label: "inert-baseline".into(),
        launch_fingerprint: "a".repeat(64),
        prior_resource: preparing(),
    };
    let token = projector
        .publish_provider(preparing(), invocation.clone())
        .unwrap();
    assert_eq!(token.into_invocation(), invocation);
    let saved = NativeJournal::decode(&std::fs::read(store.root().join(JOURNAL_FILENAME)).unwrap())
        .unwrap();
    assert_eq!(saved.ownership[0].ownership_id, "owner-1");
    assert_eq!(saved.provider_calls[0].invocation, invocation);
    assert!(!store.root().join("report.json").exists());
    assert!(!store.root().join("journal.json").exists());
    assert!(projector.publish_provider(preparing(), invocation).is_err());
}

#[test]
fn failed_retained_publication_never_returns_provider_authority() {
    use theseus::engine::{domain::*, projector::RunProjector};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("run");
    let store = theseus::report::create(&root).unwrap();
    let mut projector = RunProjector::new(
        &store,
        StoredRun::Native(initial_report()),
        StoredJournal::Native(NativeJournal::new("run-1")),
    )
    .unwrap();
    std::fs::rename(&root, temp.path().join("original")).unwrap();
    std::fs::create_dir(&root).unwrap();
    projector.bind_launch(&"a".repeat(64)).unwrap();
    let invocation = ProviderInvocation {
        ownership_id: "owner-1".into(),
        provider: "inert".into(),
        invocation_id: "prepare-1".into(),
        action: ProviderAction::Prepare,
        target_id: "inert-target".into(),
        baseline_label: "inert-baseline".into(),
        launch_fingerprint: "a".repeat(64),
        prior_resource: preparing(),
    };
    // Retained directory publication may remain valid after rename; prevent its leaf
    // replacement with an incompatible directory to inject a real publication error.
    std::fs::remove_file(temp.path().join("original").join(JOURNAL_FILENAME)).unwrap();
    std::fs::create_dir(temp.path().join("original").join(JOURNAL_FILENAME)).unwrap();
    assert!(projector.publish_provider(preparing(), invocation).is_err());
}

#[test]
fn native_operation_digest_must_match_the_exact_catalog_request() {
    let mut journal = NativeJournal::new("run-1");
    let mut resource = preparing();
    resource.ownership = NativeOwnership::Acquired {
        prepared: NativeIntent {
            intent: "1".repeat(32),
            binding: "2".repeat(32),
            manifest_digest: "3".repeat(64),
            workspace_marker: "4".repeat(32),
            reset_epoch: "epoch-1".into(),
            publication_scope: "file_and_directory_synced".into(),
        },
        lease: "5".repeat(32),
    };
    journal.launch_fingerprint = Some("a".repeat(64));
    journal.ownership.push(resource);
    journal.operations.push(NativeOperation {
        ownership_id: "owner-1".into(),
        lease: "5".repeat(32),
        operation_id: "6".repeat(32),
        request_digest: "0".repeat(64),
        command: command(CatalogProgram::Uninstall),
        completion: Disposition::Unknown {
            reason: "published".into(),
        },
    });
    assert!(journal.validate().is_err());
}

fn native_recipe_json() -> serde_json::Value {
    let mut steps = serde_json::Map::new();
    for program in [
        CatalogProgram::InstallTarget,
        CatalogProgram::Configure,
        CatalogProgram::Exercise,
        CatalogProgram::Uninstall,
    ] {
        steps.insert(
            program.as_str().into(),
            serde_json::to_value(command(program)).unwrap(),
        );
    }
    serde_json::json!({"kind":"theseus_native_recipe","schema":1,"os":"linux","manifest_digest":"1".repeat(64),"leaves":{"reader":"reader","app":"app","data":"data"},"base":{"length":1,"sha256":"2".repeat(64)},"target":{"length":1,"sha256":"3".repeat(64)},"scenarios":["clean"],"steps":steps,"checks":[{"id":"target-version","checkpoint":"target_installed","command":{"id":"target-version","program":"check_target","expected_status":{"kind":"posix_exit","code":0},"stdout_literal":"target"}},{"id":"configured-data","checkpoint":"configured","command":{"id":"configured-data","program":"check_data","expected_status":{"kind":"posix_exit","code":0},"stdout_literal":"data"}}],"limits":{"run_seconds":30,"inspection_seconds":30,"max_operations":128,"output_bytes":16384}})
}
#[test]
fn strict_native_recipe_requires_reader_and_data_checks_and_current_role_selection() {
    use theseus::engine::domain::*;
    let fixture = native_recipe_json();
    let recipe =
        ValidatedNativeRecipe::decode(&serde_json::to_vec(&fixture).unwrap(), "native.json".into())
            .unwrap();
    let native = SelectedRecipe::Native(recipe.clone());
    let plan = compile(
        &native,
        &SelectedRunOptions::Native(NativeRunOptions {
            output_root: std::path::PathBuf::from("unused-native-contract-output"),
            scenarios: vec![theseus::model::ScenarioKind::Clean],
            limits: NativeRunLimits::default(),
        }),
    )
    .unwrap();
    assert_eq!(plan.scenarios[0].role, LifecycleRole::NativeCurrentAccount);
    let docker = SelectedRunOptions::Docker(theseus::engine::RunOptions {
        profiles: vec![],
        scenarios: vec![],
        output_root: "unused".into(),
        pull: false,
        log_limit: 1,
    });
    assert!(compile(&native, &docker).is_err());
    for mutation in ["checks", "scenario", "shell", "limit"] {
        let mut invalid = fixture.clone();
        match mutation {
            "checks" => invalid["checks"] = serde_json::json!([]),
            "scenario" => invalid["scenarios"] = serde_json::json!(["rollback"]),
            "shell" => invalid["steps"]["install_target"]["shell"] = serde_json::json!("true"),
            _ => invalid["limits"]["output_bytes"] = serde_json::json!(16385),
        }
        assert!(
            ValidatedNativeRecipe::decode(
                &serde_json::to_vec(&invalid).unwrap(),
                "native.json".into()
            )
            .is_err(),
            "accepted {mutation}"
        );
    }
}

#[test]
fn acquired_token_cannot_bypass_persisted_revocation_for_any_catalog_program() {
    use theseus::engine::{domain::*, projector::RunProjector};
    use theseus_native_helper_store::{IntentRecord, IntentStore};
    use theseus_native_protocol::v2;
    let temp = tempfile::tempdir().unwrap();
    let store = theseus::report::create(&temp.path().join("run")).unwrap();
    let mut projector = RunProjector::new(
        &store,
        StoredRun::Native(initial_report()),
        StoredJournal::Native(NativeJournal::new("run-1")),
    )
    .unwrap();
    let mut resource = preparing();
    projector.bind_launch(&"a".repeat(64)).unwrap();
    let invocation = ProviderInvocation {
        ownership_id: resource.ownership_id.clone(),
        provider: "inert".into(),
        invocation_id: "prepare-1".into(),
        action: ProviderAction::Prepare,
        target_id: "inert-target".into(),
        baseline_label: "inert-baseline".into(),
        launch_fingerprint: "a".repeat(64),
        prior_resource: preparing(),
    };
    let _published = projector
        .publish_provider(resource.clone(), invocation.clone())
        .unwrap();
    let receipt = ProviderReceipt {
        provider: "inert".into(),
        invocation_id: "prepare-1".into(),
        invocation_digest: invocation.digest().unwrap(),
        binding: "2".repeat(32),
        manifest_digest: "3".repeat(64),
        workspace_marker: "4".repeat(32),
        reset_epoch: "epoch-1".into(),
        receipt: "inert fixture receipt; no provider invoked".into(),
    };
    projector
        .record_provider_receipt(&invocation, receipt)
        .unwrap();
    let mut intents = IntentStore::new(
        theseus_host::fs::HostDir::create_private(&temp.path().join("intents")).unwrap(),
    );
    let prepared = intents
        .prepare(IntentRecord {
            intent: v2::IntentId::from_bytes([0x11; 16]),
            binding: v2::BindingId::parse(&"2".repeat(32)).unwrap(),
            manifest_digest: v2::Digest::parse(&"3".repeat(64)).unwrap(),
            workspace_marker: v2::WorkspaceMarker::parse(&"4".repeat(32)).unwrap(),
        })
        .unwrap();
    let scope = match prepared.commit_scope() {
        theseus_host::fs::CommitScope::FileAndDirectorySynced => "file_and_directory_synced",
        theseus_host::fs::CommitScope::FileFlushedAtomicReplace => "file_flushed_atomic_replace",
    };
    let intent = NativeIntent {
        intent: prepared.record().intent.to_hex(),
        binding: "2".repeat(32),
        manifest_digest: "3".repeat(64),
        workspace_marker: "4".repeat(32),
        reset_epoch: "epoch-1".into(),
        publication_scope: scope.into(),
    };
    resource.ownership = NativeOwnership::Intent {
        prepared: intent.clone(),
    };
    let intent_token = projector
        .publish_intent(resource.clone(), prepared)
        .unwrap();
    let _original_receipt = intent_token.into_prepared();
    // Contract simulation only: no helper Acquire or Run takes place in this test.
    resource.ownership = NativeOwnership::Acquired {
        prepared: intent,
        lease: "5".repeat(32),
    };
    let acquired = projector.publish_acquired(resource).unwrap();
    projector
        .revoke_catalog("owner-1", PrefixAuthorityFailure::Replaced)
        .unwrap();
    for (index, program) in CATALOG_IDS.into_iter().enumerate() {
        let mut op = NativeOperation {
            ownership_id: "owner-1".into(),
            lease: "5".repeat(32),
            operation_id: format!("{index:032x}"),
            request_digest: String::new(),
            command: command(program),
            completion: Disposition::Unknown {
                reason: "published before submission".into(),
            },
        };
        op.request_digest = v2::request_digest(&op.request().unwrap()).unwrap().to_hex();
        assert!(
            projector.publish_command(&acquired, op).is_err(),
            "revocation authorized {program:?}"
        );
    }
    let saved = NativeJournal::decode(&std::fs::read(store.root().join(JOURNAL_FILENAME)).unwrap())
        .unwrap();
    assert!(saved.operations.is_empty());
    let release = projector.publish_release(&acquired).unwrap().into_release();
    assert_eq!(release.lease, "5".repeat(32));
    assert!(projector.publish_release(&acquired).is_err());
    assert_eq!(
        saved.ownership[0].catalog_authority,
        NativeCatalogAuthority::Revoked(PrefixAuthorityFailure::Replaced)
    );
}

#[test]
fn release_journal_rejects_unknown_owner_and_repeated_lease() {
    let mut journal = NativeJournal::new("run-1");
    journal.releases.push(NativeRelease {
        ownership_id: "missing-owner".into(),
        lease: "5".repeat(32),
        completion: Disposition::Unknown {
            reason: "published".into(),
        },
    });
    assert!(journal.validate().is_err());
}

#[test]
fn launch_binding_is_required_before_effects_and_is_immutable() {
    use theseus::engine::{domain::*, projector::RunProjector};
    let temp = tempfile::tempdir().unwrap();
    let store = theseus::report::create(&temp.path().join("run")).unwrap();
    let mut projector = RunProjector::new(
        &store,
        StoredRun::Native(initial_report()),
        StoredJournal::Native(NativeJournal::new("run-1")),
    )
    .unwrap();
    let invocation = ProviderInvocation {
        ownership_id: "owner-1".into(),
        provider: "inert".into(),
        invocation_id: "prepare-1".into(),
        action: ProviderAction::Prepare,
        target_id: "fixture".into(),
        baseline_label: "baseline".into(),
        launch_fingerprint: "a".repeat(64),
        prior_resource: preparing(),
    };
    assert!(
        projector
            .publish_provider(preparing(), invocation.clone())
            .is_err()
    );
    assert!(projector.bind_launch("not-a-canonical-digest").is_err());
    projector.bind_launch(&"a".repeat(64)).unwrap();
    assert!(projector.bind_launch(&"a".repeat(64)).is_err());
    projector.publish_provider(preparing(), invocation).unwrap();
    assert!(projector.bind_launch(&"b".repeat(64)).is_err());
}

#[test]
fn provider_receipt_digest_binds_every_original_context_field() {
    let original = ProviderInvocation {
        ownership_id: "owner-1".into(),
        provider: "inert".into(),
        invocation_id: "prepare-1".into(),
        action: ProviderAction::Prepare,
        target_id: "fixture".into(),
        baseline_label: "baseline".into(),
        launch_fingerprint: "a".repeat(64),
        prior_resource: preparing(),
    };
    let digest = original.digest().unwrap();
    for field in [
        "ownership_id",
        "provider",
        "invocation_id",
        "action",
        "target_id",
        "baseline_label",
        "launch_fingerprint",
        "prior_resource",
    ] {
        let mut changed = serde_json::to_value(&original).unwrap();
        match field {
            "action" => changed[field] = serde_json::json!("dispose"),
            "ownership_id" => {
                changed[field] = serde_json::json!("changed");
                changed["prior_resource"]["ownership_id"] = serde_json::json!("changed");
            }
            "launch_fingerprint" => changed[field] = serde_json::json!("b".repeat(64)),
            "prior_resource" => changed[field]["scenario_id"] = serde_json::json!("changed"),
            _ => changed[field] = serde_json::json!("changed"),
        }
        let changed: ProviderInvocation = serde_json::from_value(changed).unwrap();
        assert_ne!(changed.digest().unwrap(), digest, "unbound field {field}");
    }
}

#[test]
fn selected_native_missing_authority_and_backend_mismatch_have_no_store_effect() {
    use theseus::engine::{CancellationToken, domain::*, run_selected};
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("must-not-exist");
    let recipe = ValidatedNativeRecipe::decode(
        &serde_json::to_vec(&native_recipe_json()).unwrap(),
        "native.json".into(),
    )
    .unwrap();
    let selected = SelectedRecipe::Native(recipe);
    let native = SelectedRunOptions::Native(NativeRunOptions {
        output_root: output.clone(),
        scenarios: vec![theseus::model::ScenarioKind::Clean],
        limits: NativeRunLimits::default(),
    });
    assert!(
        run_selected(
            &selected,
            native,
            None,
            CancellationToken::default(),
            |_| panic!("admission emitted an event")
        )
        .is_err()
    );
    assert!(!output.exists());
    let docker = SelectedRunOptions::Docker(theseus::engine::RunOptions {
        profiles: vec![],
        scenarios: vec![],
        output_root: output.clone(),
        pull: false,
        log_limit: 1,
    });
    assert!(
        run_selected(
            &selected,
            docker,
            None,
            CancellationToken::default(),
            |_| panic!("mismatch emitted an event")
        )
        .is_err()
    );
    assert!(!output.exists());
}

#[test]
fn provider_receipt_from_another_invocation_is_rejected_before_intent_authority() {
    use theseus::engine::{domain::*, projector::RunProjector};
    let temp = tempfile::tempdir().unwrap();
    let store = theseus::report::create(&temp.path().join("run")).unwrap();
    let mut projector = RunProjector::new(
        &store,
        StoredRun::Native(initial_report()),
        StoredJournal::Native(NativeJournal::new("run-1")),
    )
    .unwrap();
    projector.bind_launch(&"a".repeat(64)).unwrap();
    let invocation = ProviderInvocation {
        ownership_id: "owner-1".into(),
        provider: "inert".into(),
        invocation_id: "prepare-1".into(),
        action: ProviderAction::Prepare,
        target_id: "fixture".into(),
        baseline_label: "baseline".into(),
        launch_fingerprint: "a".repeat(64),
        prior_resource: preparing(),
    };
    projector
        .publish_provider(preparing(), invocation.clone())
        .unwrap();
    let mut receipt = ProviderReceipt {
        provider: "inert".into(),
        invocation_id: "prepare-1".into(),
        invocation_digest: "0".repeat(64),
        binding: "2".repeat(32),
        manifest_digest: "3".repeat(64),
        workspace_marker: "4".repeat(32),
        reset_epoch: "epoch-1".into(),
        receipt: "fixture".into(),
    };
    assert!(
        projector
            .record_provider_receipt(&invocation, receipt.clone())
            .is_err()
    );
    receipt.invocation_digest = invocation.digest().unwrap();
    projector
        .record_provider_receipt(&invocation, receipt)
        .unwrap();
    let StoredJournal::Native(journal) = projector.journal() else {
        panic!("wrong backend")
    };
    assert!(journal.provider_calls[0].receipt.is_some());
    let mut unbound = journal.clone();
    unbound.launch_fingerprint = None;
    assert!(unbound.validate().is_err());
}

#[test]
fn native_status_rejects_invalid_signal_domains_in_commands_and_actual_evidence() {
    for signal in [0, 256, u16::MAX] {
        let mut catalog = command(CatalogProgram::CheckTarget);
        catalog.expected_status = NativeStatus::PosixSignal { signal };
        assert!(catalog.validate().is_err());
        let evidence = NativeCommandEvidence {
            command: command(CatalogProgram::CheckTarget),
            operation_id: "1".repeat(32),
            request_digest: "2".repeat(64),
            status: Some(NativeStatus::PosixSignal { signal }),
            stdout: String::new(),
            stdout_complete: true,
            stdout_utf8: true,
            stderr_complete: true,
            stdout_log: None,
            stderr_log: None,
            failure: None,
        };
        assert!(evidence.validate().is_err());
    }
    for signal in [1, 255] {
        assert!(NativeStatus::PosixSignal { signal }.validate().is_ok());
    }
    assert!(
        NativeStatus::WindowsExit { code: u32::MAX }
            .validate()
            .is_ok()
    );
}
