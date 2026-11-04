use theseus_host::fs::HostDir;
use theseus_native_helper_store::{LedgerStore, StoreBinding};
use theseus_native_protocol::v2::{
    BindingId, Digest, IntentId, LeaseId, Os, WorkspaceMarker,
    broker::*,
    ledger::{Entry, Record},
};
fn header() -> IntegrationBrokerHeader {
    IntegrationBrokerHeader {
        linux_gio_qualification: None,
        macos_security_session_id: None,
        profile: "native_integration_broker_v1".into(),
        binding: "11".repeat(16),
        manifest_digest: "22".repeat(32),
        workspace_marker: "33".repeat(16),
        os: "linux".into(),
        target_id: "owned-target".into(),
        ownership_id: "44".repeat(16),
        provider_epoch: "55".repeat(32),
        session_id: "supplied-session".into(),
        session_epoch: "66".repeat(32),
        broker_endpoint: theseus_native_protocol::v2::LocalEndpoint::UnixPath {
            path: "/fixture/broker.sock".into(),
        },
        broker_principal: BrokerPrincipal::Posix {
            effective_uid: 1000,
            effective_gid: 1000,
        },
        helper_principal: BrokerPrincipal::Posix {
            effective_uid: 1000,
            effective_gid: 1000,
        },
        display: SuppliedDisplay::X11 {
            display: ":92".into(),
            authority_file: "/fixture/auth".into(),
            bus: "/fixture/bus.sock".into(),
        },
        allowed_modes: vec![DesktopMode::OwnedExecutable],
    }
}
#[test]
fn original_broker_profile_uses_existing_locked_store_and_never_becomes_native_catalog() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = HostDir::create_private(&temp.path().join("ledger")).unwrap();
    let workspace = HostDir::create_private(&temp.path().join("workspace")).unwrap();
    let h = header();
    let receipt = LedgerStore::bootstrap_broker(&ledger, &workspace, &h).unwrap();
    assert_eq!(
        receipt.workspace_marker(),
        WorkspaceMarker::parse(&h.workspace_marker).unwrap()
    );
    let mut store = LedgerStore::open_broker(&ledger, &workspace, &h).unwrap();
    assert!(LedgerStore::open_broker(&ledger, &workspace, &h).is_err());
    let mut request = DesktopRequest {
        helper_lease: "77".repeat(16),
        helper_operation: "88".repeat(16),
        broker_lease: "99".repeat(16),
        binding_digest: h.digest().unwrap().to_hex(),
        activation_nonce: "aa".repeat(16),
        slot: theseus_native_protocol::v2::ResourceSlot::new("desktop").unwrap(),
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
                    intent: IntentId::from_bytes([1; 16]),
                    lease: LeaseId::parse(&request.broker_lease).unwrap(),
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
    drop(store);
    let mut changed = h.clone();
    changed.session_epoch = "ee".repeat(32);
    assert!(LedgerStore::open_broker(&ledger, &workspace, &changed).is_err());
    let binding = || StoreBinding {
        binding: BindingId::parse(&h.binding).unwrap(),
        manifest_digest: Digest::parse(&h.manifest_digest).unwrap(),
        workspace_marker: WorkspaceMarker::parse(&h.workspace_marker).unwrap(),
        os: Os::Linux,
    };
    assert!(LedgerStore::open(&ledger, &workspace, binding()).is_err());
    assert!(
        LedgerStore::open_native(
            &ledger,
            &workspace,
            binding(),
            Digest::parse(&h.provider_epoch).unwrap()
        )
        .is_err()
    );
    let mut store = LedgerStore::open_broker(&ledger, &workspace, &h).unwrap();
    let before = std::fs::read(temp.path().join("ledger/ledger.bin")).unwrap();
    for _ in 0..3 {
        let view = BrokerLedgerView::replay(store.records()).unwrap();
        assert_eq!(view.query(&request).unwrap().effect, BrokerEffect::Unknown);
        assert_eq!(view.query(&request).unwrap().owner, BrokerOwner::Lost);
        assert_eq!(view.accepted_count(), 1);
    }
    assert_eq!(
        std::fs::read(temp.path().join("ledger/ledger.bin")).unwrap(),
        before
    );
    store
        .append(
            Record::new(
                3,
                Entry::Broker {
                    event: BrokerEvent::Quarantined {
                        operation: request.helper_operation.clone(),
                    },
                },
            )
            .unwrap(),
        )
        .unwrap();
    let mut fresh = request.clone();
    fresh.helper_operation = "ff".repeat(16);
    let close_digest = fresh.close_request().unwrap().digest().unwrap().to_hex();
    if let DesktopAction::Activate { close_binding, .. } = &mut fresh.action {
        close_binding.action_digest = close_digest;
    }
    let count = store.records().len();
    assert!(
        store
            .append(
                Record::new(
                    4,
                    Entry::Broker {
                        event: BrokerEvent::Accepted {
                            request_digest: fresh.digest().unwrap().to_hex(),
                            request: fresh
                        }
                    }
                )
                .unwrap()
            )
            .is_err()
    );
    assert_eq!(store.records().len(), count);
}
