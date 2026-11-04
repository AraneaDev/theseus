#![cfg(feature = "test-support")]
use std::{
    io,
    time::{Duration, Instant},
};
use theseus_host::{
    desktop::{
        owned::ExecutableBinding,
        session::{BoundSession, SessionAuthority, SessionBinding},
    },
    fs::HostDir,
    principal::{CurrentPrincipal, current_principal},
    process::{ChildEnvironment, InputMode, NativeCommand, OwnedChild},
    services::readiness::{
        ArtifactRole, LocalChallenge, ReadinessChallenge, ReadinessExpectation, ReadyProtocol,
    },
};
use theseus_native_helper::broker::{BrokerRuntime, OwnedLaunch, SourceInterruption};
use theseus_native_helper_store::LedgerStore;
use theseus_native_protocol::v2::{self, broker::*};

fn header() -> IntegrationBrokerHeader {
    let principal = match current_principal().unwrap() {
        CurrentPrincipal::Posix {
            effective_uid,
            effective_gid,
        } => BrokerPrincipal::Posix {
            effective_uid,
            effective_gid,
        },
        CurrentPrincipal::Windows { token_user_sid } => BrokerPrincipal::Windows { token_user_sid },
    };
    #[cfg(unix)]
    let endpoint = v2::LocalEndpoint::UnixPath {
        path: "/tmp/theseus-source-broker.sock".into(),
    };
    #[cfg(windows)]
    let endpoint = v2::LocalEndpoint::NamedPipe {
        path: "source-broker".into(),
    };
    IntegrationBrokerHeader {
        linux_gio_qualification: None,
        macos_security_session_id: None,
        profile: "native_integration_broker_v1".into(),
        binding: "11".repeat(16),
        manifest_digest: "22".repeat(32),
        workspace_marker: "33".repeat(16),
        os: if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "windows"
        }
        .into(),
        target_id: "source-target".into(),
        ownership_id: "44".repeat(16),
        provider_epoch: "55".repeat(32),
        session_id: "source-only".into(),
        session_epoch: "66".repeat(32),
        broker_endpoint: endpoint,
        broker_principal: principal.clone(),
        helper_principal: principal,
        display: SuppliedDisplay::SourceOwnedHeadless,
        allowed_modes: vec![DesktopMode::OwnedExecutable],
    }
}
fn session(h: &IntegrationBrokerHeader) -> BoundSession {
    use theseus_host::desktop::session as s;
    #[cfg(unix)]
    let endpoint = s::BrokerEndpoint::Unix(
        theseus_host::services::readiness::UnixPath::new("/tmp/theseus-source-broker.sock".into())
            .unwrap(),
    );
    #[cfg(windows)]
    let endpoint = s::BrokerEndpoint::Pipe(
        theseus_host::services::readiness::NamedPipe::new(r"\\.\pipe\theseus.source-broker".into())
            .unwrap(),
    );
    let principal = current_principal().unwrap();
    let binding = SessionBinding {
        target_id: h.target_id.clone(),
        ownership_id: [0x44; 16],
        provider_epoch: [0x55; 32],
        session_id: h.session_id.clone(),
        session_epoch: [0x66; 32],
        broker_header_digest: *h.digest().unwrap().as_bytes(),
        manifest_digest: [0x22; 32],
        broker_endpoint: endpoint,
        broker_principal: principal.clone(),
        helper_principal: principal,
        display: s::SuppliedDisplay::SourceOwnedHeadless,
    };
    BoundSession::admit(
        SessionAuthority {
            kind: if cfg!(target_os = "linux") {
                s::DesktopKind::Linux
            } else if cfg!(target_os = "macos") {
                s::DesktopKind::MacOs
            } else {
                s::DesktopKind::Windows
            },
            binding: binding.clone(),
            allowed_modes: vec![s::DesktopMode::OwnedExecutable],
        },
        &binding,
    )
    .unwrap()
}
fn command(role: &str) -> NativeCommand {
    let mut command = NativeCommand::new(std::env::current_exe().unwrap());
    command.args(["--exact", "broker_source_worker", "--nocapture"]);
    command.environment =
        ChildEnvironment::Replace(vec![("THESEUS_BROKER_SOURCE_ROLE".into(), role.into())]);
    command
}
#[cfg(unix)]
fn channel(root: &std::path::Path, name: &str) -> LocalChallenge {
    LocalChallenge::bind(
        theseus_host::services::readiness::UnixPath::new(root.join(name).to_str().unwrap().into())
            .unwrap(),
    )
    .unwrap()
}
#[cfg(windows)]
fn channel(_root: &std::path::Path, name: &str) -> LocalChallenge {
    let CurrentPrincipal::Windows { token_user_sid } = current_principal().unwrap() else {
        unreachable!()
    };
    LocalChallenge::bind(
        theseus_host::services::readiness::NamedPipe::new(format!(
            r"\\.\pipe\theseus.broker-{}-{name}",
            std::process::id()
        ))
        .unwrap(),
        theseus_host::services::readiness::Sid::new(token_user_sid).unwrap(),
    )
    .unwrap()
}
fn actual_broker(point: Option<SourceInterruption>, native_io: bool) {
    #[cfg(target_os = "linux")]
    // SAFETY: Only this explicitly spawned isolated worker enters here. It has
    // default SIGCHLD, no existing child, and owns exactly one activation.
    let scope = unsafe {
        theseus_host::desktop::scope::DedicatedBrokerScope::establish_for_exclusive_process()
    }
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let ledger = HostDir::create_private(&temp.path().join("l")).unwrap();
    let workspace = HostDir::create_private(&temp.path().join("w")).unwrap();
    let channel_path = temp.path().join("channels");
    let _channels = HostDir::create_private(&channel_path).unwrap();
    let h = header();
    LedgerStore::bootstrap_broker(&ledger, &workspace, &h).unwrap();
    let mut broker =
        BrokerRuntime::open(LedgerStore::open_broker(&ledger, &workspace, &h).unwrap()).unwrap();
    let intent = v2::IntentId::from_bytes([1; 16]);
    let binding = v2::BindingId::parse(&h.binding).unwrap();
    let manifest = v2::Digest::parse(&h.manifest_digest).unwrap();
    let acquired = broker.acquire(intent, binding, manifest).unwrap();
    let v2::AcquireView::Lease { lease, .. } = acquired.clone() else {
        panic!("lease")
    };
    assert_eq!(broker.acquire(intent, binding, manifest).unwrap(), acquired);
    assert!(
        broker
            .acquire(v2::IntentId::from_bytes([2; 16]), binding, manifest)
            .is_err()
    );
    let mut request = DesktopRequest {
        helper_lease: "77".repeat(16),
        helper_operation: "88".repeat(16),
        broker_lease: lease.to_hex(),
        binding_digest: h.digest().unwrap().to_hex(),
        activation_nonce: "aa".repeat(16),
        slot: v2::ResourceSlot::new("desktop").unwrap(),
        prior_binding_digest: "bb".repeat(32),
        action: DesktopAction::Activate {
            transition: None,
            mode: DesktopMode::OwnedExecutable,
            entry_digest: "cc".repeat(32),
            expectation_digest: "dd".repeat(32),
            close_binding: CooperativeCloseBinding {
                operation: "ee".repeat(16),
                action_digest: "00".repeat(32),
                nonce: "fe".repeat(16),
            },
        },
    };
    let close_digest = request.close_request().unwrap().digest().unwrap();
    if let DesktopAction::Activate { close_binding, .. } = &mut request.action {
        close_binding.action_digest = close_digest.to_hex();
    }
    let identity = request.identity().unwrap();
    let ready = ReadinessChallenge {
        protocol: ReadyProtocol::TheseusFixtureReadyV1,
        lease: lease.to_hex(),
        slot: "desktop".into(),
        resource_binding_digest: [0xbb; 32],
        start_or_activation_operation: request.helper_operation.clone(),
        action_digest: *request.digest().unwrap().as_bytes(),
        nonce: [0xaa; 16],
        expectation_digest: [0xdd; 32],
    };
    let mut close = ready.clone();
    close.start_or_activation_operation = "ee".repeat(16);
    close.action_digest = *close_digest.as_bytes();
    close.nonce = [0xfe; 16];
    let executable = ExecutableBinding {
        ready,
        close,
        close_original_activation_operation: request.helper_operation.clone(),
        expectation: ReadinessExpectation {
            artifact: ArtifactRole::Target,
            artifact_length: 1,
            artifact_sha256: [1; 32],
            version_literal: "source-v1".into(),
            data_sha256: [2; 32],
            configuration_digest: [3; 32],
        },
        cooperative_instance: "original-source".into(),
        instance_binding_digest: [0xbb; 32],
    };
    let mut launch = Some(OwnedLaunch {
        command: if native_io {
            let mut c = NativeCommand::new(temp.path().join("source-absent-executable"));
            c.environment = ChildEnvironment::Replace(vec![]);
            c
        } else {
            command("app")
        },
        session: session(&h),
        binding: executable,
        ready_channel: channel(&channel_path, "ready.sock"),
        close_channel: channel(&channel_path, "close.sock"),
        #[cfg(target_os = "linux")]
        scope,
    });
    if let Some(point) = point {
        broker.source_interrupt_once(point);
    }
    // Source-only cutpoints drop replies after durable acceptance, actual spawn,
    // and effect receipt. They never substitute a launcher or native owner.
    let reply = broker.activate(request.clone(), &mut launch);
    if point.is_some() {
        assert!(reply.is_err());
    } else {
        drop(reply.unwrap());
    }
    let state = broker.query(&identity).unwrap();
    if native_io {
        assert!(state.quarantined);
        assert!(!broker.retained_errors().is_empty());
        assert!(matches!(
            state.effect,
            BrokerEffect::Receipt {
                receipt: BrokerEffectReceipt {
                    acknowledged: false,
                    ..
                }
            }
        ));
        assert!(broker.release_original(lease).is_err());
        let until = Instant::now() + Duration::from_secs(4);
        loop {
            let state = broker.cancel(&identity).unwrap();
            if state.cleanup_complete {
                break;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(broker.query(&identity).unwrap().quarantined);
        assert!(broker.release_original(lease).is_err());
        drop(broker);
        let mut broker =
            BrokerRuntime::open(LedgerStore::open_broker(&ledger, &workspace, &h).unwrap())
                .unwrap();
        assert!(broker.query(&identity).unwrap().quarantined);
        assert!(broker.release_original(lease).is_err());
        assert_eq!(
            BrokerLedgerView::replay(broker.store().records())
                .unwrap()
                .accepted_count(),
            1
        );
        return;
    }
    if point == Some(SourceInterruption::AfterAcceptance) {
        assert!(launch.is_some());
        assert_eq!(state.owner, BrokerOwner::Lost);
        assert_eq!(state.effect, BrokerEffect::Unknown);
        assert!(broker.release_original(lease).is_err());
        assert_eq!(broker.activate(request.clone(), &mut None).unwrap(), state);
        assert_eq!(broker.cancel(&identity).unwrap(), state);
        drop(launch);
        drop(broker);
        let mut broker =
            BrokerRuntime::open(LedgerStore::open_broker(&ledger, &workspace, &h).unwrap())
                .unwrap();
        assert!(broker.query(&identity).unwrap().quarantined);
        assert_eq!(
            broker.activate(request.clone(), &mut None).unwrap().owner,
            BrokerOwner::Lost
        );
        assert_eq!(
            BrokerLedgerView::replay(broker.store().records())
                .unwrap()
                .accepted_count(),
            1
        );
        return;
    }
    assert!(launch.is_none());
    assert_eq!(state.owner, BrokerOwner::Retained);
    if point == Some(SourceInterruption::AfterSpawn) {
        assert_eq!(state.effect, BrokerEffect::Unknown);
    } else {
        assert!(matches!(
            state.effect,
            BrokerEffect::Receipt {
                receipt: BrokerEffectReceipt {
                    evidence: DesktopEvidence::SourceOwnedExecutable,
                    acknowledged: true,
                    ..
                }
            }
        ));
    }
    assert_ne!(state.descendants, BrokerDescendants::Joined);
    assert!(broker.release_original(lease).is_err());
    // A helper restart reconnects using this exact original identity. Repeated
    // same-digest Activate is cached and requires no new launch resource.
    assert_eq!(broker.activate(request.clone(), &mut None).unwrap(), state);
    let mut changed = request.clone();
    changed.prior_binding_digest = "12".repeat(32);
    assert!(broker.activate(changed, &mut None).is_err());
    assert_eq!(
        BrokerLedgerView::replay(broker.store().records())
            .unwrap()
            .accepted_count(),
        1
    );
    let until = Instant::now() + Duration::from_secs(4);
    loop {
        let state = broker.cancel(&identity).unwrap();
        if state.cleanup_complete {
            break;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(2));
    }
    let terminal = broker.query(&identity).unwrap();
    assert_eq!(terminal.owner, BrokerOwner::Settled);
    assert_eq!(terminal.descendants, BrokerDescendants::Joined);
    if point == Some(SourceInterruption::AfterSpawn) {
        assert!(broker.release_original(lease).is_err());
    } else {
        assert!(matches!(
            broker.release_original(lease).unwrap(),
            v2::LeaseView::AwaitingExternalReset { .. }
        ));
        let count = broker.store().records().len();
        assert!(matches!(
            broker.release_original(lease).unwrap(),
            v2::LeaseView::AwaitingExternalReset { .. }
        ));
        assert_eq!(broker.store().records().len(), count);
        assert!(matches!(
            broker.query_acquire(intent, binding, manifest).unwrap(),
            v2::AcquireView::Lease {
                state: v2::LeaseView::AwaitingExternalReset { .. },
                ..
            }
        ));
    }
    drop(broker);
    let mut broker =
        BrokerRuntime::open(LedgerStore::open_broker(&ledger, &workspace, &h).unwrap()).unwrap();
    let recovered = broker.query(&identity).unwrap();
    if point == Some(SourceInterruption::AfterSpawn) {
        assert_eq!(recovered.effect, BrokerEffect::Unknown);
        assert!(recovered.quarantined);
        assert_eq!(recovered.owner, BrokerOwner::Settled);
        assert!(broker.release_original(lease).is_err());
    } else {
        assert_eq!(recovered, terminal);
        assert!(matches!(
            broker.release_original(lease).unwrap(),
            v2::LeaseView::AwaitingExternalReset { .. }
        ));
        assert!(matches!(
            broker.query_acquire(intent, binding, manifest).unwrap(),
            v2::AcquireView::Lease {
                state: v2::LeaseView::AwaitingExternalReset { .. },
                ..
            }
        ));
    }
}
#[test]
fn broker_source_worker() {
    match std::env::var("THESEUS_BROKER_SOURCE_ROLE").as_deref() {
        Ok("worker") => actual_broker(None, false),
        Ok("worker-acceptance") => actual_broker(Some(SourceInterruption::AfterAcceptance), false),
        Ok("worker-spawn") => actual_broker(Some(SourceInterruption::AfterSpawn), false),
        Ok("worker-receipt") => actual_broker(Some(SourceInterruption::AfterReceipt), false),
        Ok("worker-native-io") => actual_broker(None, true),
        Ok("app") => loop {
            std::thread::sleep(Duration::from_millis(50));
        },
        _ => {}
    }
}
#[test]
fn broker_lost_activation_reply_and_each_side_restart_never_duplicate_activation() {
    for role in [
        "worker",
        "worker-acceptance",
        "worker-spawn",
        "worker-receipt",
        "worker-native-io",
    ] {
        let mut child = OwnedChild::spawn(&command(role), InputMode::Null).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        let mut output = Vec::new();
        loop {
            let mut bytes = [0; 1024];
            for pipe in [&mut child.stdout, &mut child.stderr] {
                match pipe.try_read(&mut bytes) {
                    Ok(n) => {
                        if output.len() < 16384 {
                            output.extend_from_slice(&bytes[..n]);
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("{e}"),
                }
            }
            if child.child.exited_reserved().unwrap() {
                break;
            }
            assert!(
                Instant::now() < until,
                "{}",
                String::from_utf8_lossy(&output)
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            child.child.reap_and_release().unwrap().success(),
            "{}",
            String::from_utf8_lossy(&output)
        );
    }
}
#[test]
fn broker_restart_persists_quarantine_for_every_unfinished_acceptance_boundary() {
    use v2::ledger::{Entry, Record};
    for completion_receipt in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let ledger = HostDir::create_private(&temp.path().join("l")).unwrap();
        let workspace = HostDir::create_private(&temp.path().join("w")).unwrap();
        let h = header();
        LedgerStore::bootstrap_broker(&ledger, &workspace, &h).unwrap();
        let mut store = LedgerStore::open_broker(&ledger, &workspace, &h).unwrap();
        let mut request = DesktopRequest {
            helper_lease: "77".repeat(16),
            helper_operation: "88".repeat(16),
            broker_lease: "99".repeat(16),
            binding_digest: h.digest().unwrap().to_hex(),
            activation_nonce: "aa".repeat(16),
            slot: v2::ResourceSlot::new("desktop").unwrap(),
            prior_binding_digest: "bb".repeat(32),
            action: DesktopAction::Activate {
                transition: None,
                mode: DesktopMode::OwnedExecutable,
                entry_digest: "cc".repeat(32),
                expectation_digest: "dd".repeat(32),
                close_binding: CooperativeCloseBinding {
                    operation: "ee".repeat(16),
                    action_digest: "00".repeat(32),
                    nonce: "fe".repeat(16),
                },
            },
        };
        let close_digest = request.close_request().unwrap().digest().unwrap().to_hex();
        if let DesktopAction::Activate { close_binding, .. } = &mut request.action {
            close_binding.action_digest = close_digest;
        }
        store
            .append(
                Record::new(
                    1,
                    Entry::LeaseReserved {
                        intent: v2::IntentId::from_bytes([1; 16]),
                        lease: v2::LeaseId::parse(&request.broker_lease).unwrap(),
                    },
                )
                .unwrap(),
            )
            .unwrap();
        store
            .append(
                Record::new(
                    2,
                    Entry::Broker {
                        event: BrokerEvent::Accepted {
                            request: request.clone(),
                            request_digest: request.digest().unwrap().to_hex(),
                        },
                    },
                )
                .unwrap(),
            )
            .unwrap();
        if completion_receipt {
            store
                .append(
                    Record::new(
                        3,
                        Entry::Broker {
                            event: BrokerEvent::EffectRecorded {
                                operation: request.helper_operation.clone(),
                                receipt: BrokerEffectReceipt {
                                    evidence: DesktopEvidence::SourceOwnedExecutable,
                                    acknowledged: true,
                                    receipt_digest: request.digest().unwrap().to_hex(),
                                },
                            },
                        },
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        drop(store);
        let mut broker =
            BrokerRuntime::open(LedgerStore::open_broker(&ledger, &workspace, &h).unwrap())
                .unwrap();
        let identity = request.identity().unwrap();
        let original = broker.query(&identity).unwrap();
        assert!(original.quarantined);
        assert_eq!(original.owner, BrokerOwner::Lost);
        assert_eq!(original.primary, BrokerPrimary::Unknown);
        assert_eq!(original.descendants, BrokerDescendants::Unknown);
        assert_eq!(
            matches!(original.effect, BrokerEffect::Receipt { .. }),
            completion_receipt
        );
        let count = broker.store().records().len();
        assert!(
            broker
                .release_original(v2::LeaseId::parse(&request.broker_lease).unwrap())
                .is_err()
        );
        assert_eq!(
            broker.activate(request.clone(), &mut None).unwrap(),
            original
        );
        assert_eq!(broker.cancel(&identity).unwrap(), original);
        assert_eq!(broker.store().records().len(), count);
        let mut reset = h.clone();
        reset.provider_epoch = "12".repeat(32);
        assert_eq!(
            BrokerLedgerView::replay(broker.store().records())
                .unwrap()
                .accepted_count(),
            1
        );
        assert!(
            BrokerLedgerView::replay(broker.store().records())
                .unwrap()
                .is_quarantined(&request.helper_operation)
        );
        drop(broker);
        assert!(LedgerStore::open_broker(&ledger, &workspace, &reset).is_err());
        let broker =
            BrokerRuntime::open(LedgerStore::open_broker(&ledger, &workspace, &h).unwrap())
                .unwrap();
        assert_eq!(broker.query(&identity).unwrap(), original);
        assert_eq!(broker.store().records().len(), count);
    }
}
