#![cfg(feature = "native-catalog-test-support")]
//! Actual inert helper/catalog acceptance. This does not exercise SSH or reset.
#[path = "support/native_catalog_setup.rs"]
mod artifact_setup;
use sha2::{Digest as _, Sha256};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use theseus_host::{
    fs::HostDir,
    process::{ChildEnvironment, NativeCommand},
};
use theseus_native_client::{Action, Client, Event, ExpectedHelper, Poll};
use theseus_native_helper_store::{
    BootstrapRequest, IntentRecord, IntentStore, StoreBinding, bootstrap_native,
};
use theseus_native_protocol::v2;
fn binary(name: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(name).unwrap_or_else(|| panic!("required explicit inert binary {name}")),
    );
    assert!(path.is_absolute() && path.is_file());
    path
}
#[test]
fn unchanged_catalog_manifest_cannot_authorize_different_recipe_artifacts() {
    use theseus::{
        engine::{self, domain::*},
        model::ScenarioKind,
    };
    let fixture = artifact_setup::NativeCatalogFixture::new();
    let launch = fixture.launch();
    let mut original = fixture.recipe(ScenarioKind::Clean);
    let replacement = if original.recipe.target.sha256.starts_with('a') {
        "b"
    } else {
        "a"
    };
    original
        .recipe
        .target
        .sha256
        .replace_range(..1, replacement);
    let recipe = theseus::native::recipe::ValidatedNativeRecipe::decode(
        &serde_json::to_vec(&original.recipe).unwrap(),
        original.path,
    )
    .unwrap();
    assert_eq!(recipe.recipe.manifest_digest, fixture.digest.to_hex());
    let output = fixture.root.path().join("mismatched-artifacts");
    let result = engine::run_selected(
        &SelectedRecipe::Native(recipe.clone()),
        SelectedRunOptions::Native(NativeRunOptions {
            output_root: output.clone(),
            scenarios: vec![ScenarioKind::Clean],
            limits: recipe.recipe.limits.clone(),
        }),
        Some(&launch),
        engine::CancellationToken::default(),
        |_| {},
    );
    assert!(
        result.is_err(),
        "same manifest must not authorize changed reader expectations"
    );
    assert!(!fixture.workspace.join("reader").exists());
    assert!(!fixture.workspace.join("reader.exe").exists());
    for run in std::fs::read_dir(output).unwrap() {
        let store = theseus::report::open_existing(&run.unwrap().path()).unwrap();
        let journal = store.load_native_journal().unwrap();
        assert!(journal.operations.is_empty());
        assert!(
            journal.ownership.iter().all(|owner| matches!(
                owner.ownership,
                theseus::native::model::NativeOwnership::Preparing { .. }
            )),
            "artifact mismatch must refuse before Acquire authority"
        );
        assert_eq!(
            std::fs::read_dir(store.root().join("native-intents"))
                .unwrap()
                .count(),
            0
        );
    }
}
fn next(client: &mut Client) -> Event {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "bounded actual helper wait");
        match client.poll().unwrap() {
            Poll::Event(e) => return e,
            Poll::TransportSettled => panic!("helper closed"),
            _ => std::thread::yield_now(),
        }
    }
}
fn reply(client: &mut Client, id: theseus_native_client::RequestId) -> v2::Reply {
    loop {
        if let Event::Reply { request, reply } = next(client)
            && request == id
        {
            return reply;
        }
    }
}
#[test]
fn actual_native_helper_executes_installed_reader_and_hashes_settled_absence() {
    let temp = tempfile::tempdir().unwrap();
    let workspace_path = temp.path().join("workspace");
    let workspace = HostDir::create_private(&workspace_path).unwrap();
    let ledger_path = temp.path().join("ledger");
    let ledger = HostDir::create_private(&ledger_path).unwrap();
    let intents = HostDir::create_private(&temp.path().join("intents")).unwrap();
    let installer = binary("THESEUS_NATIVE_CATALOG_FIXTURE");
    let base = binary("THESEUS_NATIVE_BASE_READER");
    let target = binary("THESEUS_NATIVE_TARGET_READER");
    let helper = binary("THESEUS_NATIVE_HELPER_BINARY");
    let reader = if cfg!(windows) {
        "reader.exe"
    } else {
        "reader"
    };
    let installed = workspace_path.join(reader);
    let data = workspace_path.join("data");
    let ids = [
        "check_base",
        "check_data",
        "check_target",
        "configure",
        "exercise",
        "install_base",
        "install_target",
        "uninstall",
    ];
    let programs=ids.iter().map(|id|{let check=matches!(*id,"check_base"|"check_data"|"check_target"|"exercise");serde_json::json!({"id":id,"program":if check{&installed}else{&installer},"args":if check{vec![id.to_string(),data.to_str().unwrap().into()]}else{vec![id.to_string(),workspace_path.to_str().unwrap().into(),base.to_str().unwrap().into(),target.to_str().unwrap().into(),reader.into()]},"environment":{"kind":"replace","entries":[]}})}).collect::<Vec<_>>();
    let artifact = |path: &PathBuf| {
        let bytes = std::fs::read(path).unwrap();
        serde_json::json!({"length":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))})
    };
    let binding = v2::BindingId::from_bytes([1; 16]);
    let epoch = v2::Digest::from_bytes([8; 32]);
    let os = if cfg!(windows) {
        v2::Os::Windows
    } else if cfg!(target_os = "macos") {
        v2::Os::Macos
    } else {
        v2::Os::Linux
    };
    let mut config = serde_json::json!({"schema":2,"profile":"native_catalog_v1","capabilities":v2::NativeCapabilities::NAMES,"provider_epoch":epoch.to_hex(),"artifacts":{"base":artifact(&base),"target":artifact(&target)},"ledger_root":ledger_path,"workspace_root":workspace_path,"binding":binding.to_hex(),"manifest_digest":"00".repeat(32),"workspace_marker":"00".repeat(16),"os":if cfg!(windows){"windows"}else if cfg!(target_os="macos"){"macos"}else{"linux"},"programs":programs});
    let digest =
        theseus_native_helper::config_manifest_digest(&serde_json::to_vec(&config).unwrap())
            .unwrap();
    let marker = bootstrap_native(
        &ledger,
        &workspace,
        BootstrapRequest {
            binding,
            manifest_digest: digest,
            os,
        },
        epoch,
    )
    .unwrap()
    .workspace_marker();
    config["manifest_digest"] = digest.to_hex().into();
    config["workspace_marker"] = marker.to_hex().into();
    let config_path = temp.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let mut store = IntentStore::new(intents);
    let prepared = store
        .prepare(IntentRecord {
            intent: v2::IntentId::from_bytes([3; 16]),
            binding,
            manifest_digest: digest,
            workspace_marker: marker,
        })
        .unwrap();
    let expected = ExpectedHelper {
        profile: v2::HelperProfile::Native {
            provider_epoch: epoch,
        },
        binding: StoreBinding {
            binding,
            manifest_digest: digest,
            workspace_marker: marker,
            os,
        },
        programs: v2::ProgramList::new(
            ids.iter()
                .map(|id| v2::ProgramId::new(id).unwrap())
                .collect(),
        )
        .unwrap(),
    };
    let mut command = NativeCommand::new(helper);
    command.args(["serve", "--config", config_path.to_str().unwrap()]);
    command.environment = ChildEnvironment::Replace(vec![]);
    let recovery_expected = ExpectedHelper {
        profile: expected.profile,
        binding: StoreBinding {
            binding,
            manifest_digest: digest,
            workspace_marker: marker,
            os,
        },
        programs: expected.programs.clone(),
    };
    let mut client = match Client::start(command.clone(), expected) {
        Ok(c) => c,
        Err(mut f) => {
            f.request_stop();
            while !f.poll_settle().unwrap() {}
            panic!("actual helper start failed")
        }
    };
    assert!(matches!(next(&mut client), Event::Ready));
    let id = client.submit(Action::Acquire(&prepared)).unwrap();
    let lease = match reply(&mut client, id) {
        v2::Reply::Acquired {
            state: v2::AcquireView::Lease { lease, .. },
            ..
        } => lease,
        other => panic!("{other:?}"),
    };
    for leaf in [reader, "app", "data"] {
        let id = client
            .submit(Action::InspectNativeFile {
                lease,
                path: v2::LeafName::new(leaf).unwrap(),
            })
            .unwrap();
        assert!(matches!(
            reply(&mut client, id),
            v2::Reply::FileAbsent { .. }
        ));
    }
    let sequence = [
        ("install_base", None),
        ("check_base", Some("base\n")),
        ("configure", None),
        ("check_data", Some("data\n")),
        ("install_target", None),
        ("check_target", Some("target\n")),
        ("check_data", Some("data\n")),
        ("exercise", Some("target\n")),
        ("install_target", None),
        ("check_target", Some("target\n")),
        ("check_data", Some("data\n")),
    ];
    for (index, (program, literal)) in sequence.into_iter().enumerate() {
        let operation = v2::OperationId::from_bytes([index as u8 + 10; 16]);
        client
            .submit(Action::Run {
                lease,
                operation,
                program: v2::ProgramId::new(program).unwrap(),
            })
            .unwrap();
        let mut output = Vec::new();
        loop {
            match next(&mut client) {
                Event::Output {
                    operation: o,
                    channel: v2::Channel::Stdout,
                    offset,
                    bytes,
                    ..
                } if o == operation => {
                    assert_eq!(offset, output.len() as u64);
                    output.extend_from_slice(bytes.as_slice())
                }
                Event::Reply {
                    reply:
                        v2::Reply::Operation {
                            operation: o,
                            state:
                                v2::OperationView::Finished {
                                    outcome:
                                        v2::Outcome::RunCompleted {
                                            status,
                                            truncated,
                                            stdout_bytes,
                                            ..
                                        },
                                },
                            ..
                        },
                    ..
                } if o == operation => {
                    assert!(!truncated);
                    assert_eq!(stdout_bytes, output.len() as u64);
                    assert!(matches!(
                        status,
                        v2::NativeStatus::PosixExit { code: 0 }
                            | v2::NativeStatus::WindowsExit { code: 0 }
                    ));
                    break;
                }
                Event::Reply {
                    reply:
                        v2::Reply::Operation {
                            lease: l,
                            operation: o,
                            state: v2::OperationView::Accepted { .. } | v2::OperationView::Running,
                        },
                    ..
                } if l == lease && o == operation => {
                    client.submit(Action::Query { lease, operation }).unwrap();
                }
                Event::Reply {
                    reply:
                        v2::Reply::Refused {
                            code: v2::RefusalCode::OwnershipPending,
                        },
                    ..
                } => {
                    client.submit(Action::Query { lease, operation }).unwrap();
                }
                _ => {}
            }
        }
        if let Some(literal) = literal {
            assert_eq!(output, literal.as_bytes())
        }
    }
    let id = client
        .submit(Action::InspectNativeFile {
            lease,
            path: v2::LeafName::new(reader).unwrap(),
        })
        .unwrap();
    match reply(&mut client, id) {
        v2::Reply::NativeFileInfo { length, sha256, .. } => {
            let bytes = std::fs::read(&target).unwrap();
            assert_eq!(length, bytes.len() as u64);
            assert_eq!(sha256, v2::Digest::from_bytes(Sha256::digest(bytes).into()))
        }
        other => panic!("{other:?}"),
    }
    let operation = v2::OperationId::from_bytes([99; 16]);
    client
        .submit(Action::Run {
            lease,
            operation,
            program: v2::ProgramId::new("uninstall").unwrap(),
        })
        .unwrap();
    loop {
        match next(&mut client) {
            Event::Reply {
                reply:
                    v2::Reply::Operation {
                        operation: o,
                        state: v2::OperationView::Finished { .. },
                        ..
                    },
                ..
            } if o == operation => break,
            Event::Reply { .. } => {
                client.submit(Action::Query { lease, operation }).unwrap();
            }
            _ => {}
        }
    }
    for leaf in [reader, "app", "data"] {
        let id = client
            .submit(Action::InspectNativeFile {
                lease,
                path: v2::LeafName::new(leaf).unwrap(),
            })
            .unwrap();
        assert!(matches!(
            reply(&mut client, id),
            v2::Reply::FileAbsent { .. }
        ));
    }
    // Actual controller/helper transport restart imports exact durable identities.
    // It never replaces original checkpoint/output evidence with current state.
    let image_bytes = client.export_recovery().unwrap().encode().unwrap();
    let original = store.verify_original(prepared.record()).unwrap();
    client.request_stop();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !client.poll_settle().unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now()
    }
    let mode = theseus_native_client::ClientMode::Recover {
        image: Box::new(theseus_native_client::RecoveryImage::decode(&image_bytes).unwrap()),
        original,
    };
    let mut client = match Client::start_with(command, recovery_expected, mode) {
        Ok(c) => c,
        Err(mut f) => {
            f.request_stop();
            while !f.poll_settle().unwrap() {}
            panic!("recovered native helper start failed")
        }
    };
    assert!(matches!(next(&mut client), Event::Ready));
    assert_eq!(
        client.submit(Action::Run {
            lease,
            operation: v2::OperationId::from_bytes([100; 16]),
            program: v2::ProgramId::new("install_target").unwrap()
        }),
        Err(theseus_native_client::ClientError::RecoveryRequired)
    );
    assert_eq!(
        client.submit(Action::Acquire(&prepared)),
        Err(theseus_native_client::ClientError::RecoveryRequired)
    );
    assert_eq!(
        client.submit(Action::InspectNativeFile {
            lease,
            path: v2::LeafName::new(reader).unwrap()
        }),
        Err(theseus_native_client::ClientError::RecoveryRequired)
    );
    let id = client.submit(Action::Query { lease, operation }).unwrap();
    assert!(matches!(
        reply(&mut client, id),
        v2::Reply::Operation {
            state: v2::OperationView::Finished { .. },
            ..
        }
    ));
    let id = client.submit(Action::Release(lease)).unwrap();
    assert!(matches!(
        reply(&mut client, id),
        v2::Reply::Lease {
            state: v2::LeaseView::AwaitingExternalReset { .. },
            ..
        }
    ));
    client.request_stop();
    let end = Instant::now() + Duration::from_secs(5);
    while !client.poll_settle().unwrap() {
        assert!(Instant::now() < end);
        std::thread::yield_now()
    }
    assert!(client.is_settled());
}

#[test]
fn fake_ssh_argv_preparation_uses_only_synthetic_private_material() {
    let temp = tempfile::tempdir().unwrap();
    let key = temp.path().join("synthetic-key");
    std::fs::write(&key,b"-----BEGIN RSA PRIVATE KEY-----\nINERT TEST BYTES; NOT A REAL KEY\n-----END RSA PRIVATE KEY-----\n").unwrap();
    let pins = temp.path().join("synthetic-pins");
    std::fs::write(&pins, b"fixture.example ssh-ed25519 AAAA\n").unwrap();
    let runtime = temp.path().join("private-runtime");
    let config = serde_json::json!({"kind":"theseus_native_launch","schema":1,"profile":"openssh_subsystem_v1","target_id":"fake-argv-only","baseline_label":"inert-base","os":"linux","ssh_program":binary("THESEUS_NATIVE_CATALOG_FIXTURE"),"openssh_version":"OpenSSH_10.0 fixture-only","provider":{"name":"inert-provider","program":binary("THESEUS_NATIVE_CATALOG_FIXTURE"),"args":[]},"host":"fixture.example","port":22,"user":"helper","subsystem":"theseus","private_key":key,"host_keys":pins,"runtime_directory":runtime,"binding":"01".repeat(16),"manifest_digest":"02".repeat(32),"workspace_marker":"03".repeat(16),"provider_epoch":"08".repeat(32)});
    let launch =
        theseus::native::launch::LaunchAuthority::decode(&serde_json::to_vec(&config).unwrap())
            .unwrap();
    let mut preparation = launch.prepare();
    assert!(
        preparation.error().is_none(),
        "fake -V/-G qualification failed"
    );
    assert!(preparation.probe().unwrap().settled());
    let sessions = std::fs::read_dir(&runtime)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(sessions.len(), 1);
    let runtime = &sessions[0];
    assert_eq!(
        std::fs::read(runtime.join("identity")).unwrap(),
        std::fs::read(key).unwrap()
    );
    assert!(!runtime.join("identity-cert.pub").exists());
    assert!(!runtime.join("identity.pub").exists());
    assert!(
        std::fs::read(runtime.join("global_empty"))
            .unwrap()
            .is_empty()
    );
    preparation.request_stop();
    assert!(preparation.poll_settle().unwrap());
    assert!(preparation.is_settled());
    for name in ["identity", "host_keys", "global_empty"] {
        assert!(!runtime.join(name).exists())
    }
    let mut changed = launch.prepare();
    assert!(changed.error().is_none());
    let changed_root = std::fs::read_dir(temp.path().join("private-runtime"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.join("identity").exists())
        .unwrap();
    std::fs::rename(
        changed_root.join("identity"),
        changed_root.join("retained-original"),
    )
    .unwrap();
    std::fs::write(changed_root.join("identity"), b"synthetic replacement").unwrap();
    assert!(changed.poll_settle().is_err());
    assert!(changed.cleanup_error().is_some());
    assert!(!changed.is_settled());
    assert_eq!(
        std::fs::read(changed_root.join("identity")).unwrap(),
        b"synthetic replacement"
    );
    assert!(changed.poll_settle().is_err());
    // This fake executable only echoes the argv option profile. It proves no
    // real OpenSSH option handling, host authentication, connection, or reset.
}

#[test]
fn actual_fake_provider_reserves_before_reply_and_queries_exact_identity() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("provider");
    std::fs::create_dir(&root).unwrap();
    let template = temp.path().join("template.json");
    std::fs::write(&template,serde_json::to_vec(&serde_json::json!({"provider":"inert_provider","invocation_id":"placeholder","binding":"01".repeat(16),"manifest_digest":"02".repeat(32),"workspace_marker":"03".repeat(16),"reset_epoch":"08".repeat(32),"receipt":"placeholder"})).unwrap()).unwrap();
    let invoke = |mode: &str, owner: &str, id: &str, action: &str, template: &std::path::Path| {
        std::process::Command::new(binary("THESEUS_NATIVE_CATALOG_FIXTURE")).arg("provider").arg(&root).arg(template).arg(mode).arg(serde_json::to_string(&serde_json::json!({"ownership_id":owner,"provider":"inert_provider","invocation_id":id,"action":action,"target_id":"inert-target","baseline_label":"inert-base","launch_fingerprint":"09".repeat(32),"prior_resource":{"scenario_id":"inert","ownership_id":owner,"ownership":{"phase":"preparing","provider":"inert_provider","invocation_id":if action=="prepare"{id}else{"prepare_a"},"receipt":null},"catalog_authority":{"state":"valid"}}})).unwrap()).env_clear().output().unwrap()
    };
    let prepared = invoke("invoke", "owner_a", "prepare_a", "prepare", &template);
    assert!(prepared.status.success());
    assert!(
        !invoke("invoke", "owner_b", "prepare_b", "prepare", &template)
            .status
            .success()
    );
    let queried = invoke("query", "owner_a", "prepare_a", "prepare", &template);
    assert!(queried.status.success());
    assert_eq!(queried.stdout, prepared.stdout);
    assert!(
        !invoke("query", "owner_b", "prepare_a", "prepare", &template)
            .status
            .success()
    );
    assert!(
        !invoke("invoke", "owner_b", "reset_b", "reset", &template)
            .status
            .success()
    );
    assert!(
        invoke("invoke", "owner_a", "reset_a", "reset", &template)
            .status
            .success()
    );
    assert!(!root.join("owner").exists());
    // Failed metadata publication after reservation leaves exclusive ownership.
    assert!(
        !invoke(
            "invoke",
            "owner_unknown",
            "prepare_unknown",
            "prepare",
            &temp.path().join("missing-template")
        )
        .status
        .success()
    );
    assert_eq!(std::fs::read(root.join("owner")).unwrap(), b"owner_unknown");
    assert!(
        !invoke("invoke", "owner_new", "prepare_new", "prepare", &template)
            .status
            .success()
    );
    assert_eq!(
        invoke(
            "query",
            "owner_unknown",
            "prepare_unknown",
            "prepare",
            &template
        )
        .stdout,
        b"null"
    );
    // Fake receipt/ownership proof only; no real target reset/disposal acceptance.
}
