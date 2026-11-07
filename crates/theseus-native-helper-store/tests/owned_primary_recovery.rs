//! Test-owned durable ledger only; no process/session/SDK/IPC operations.
use serde_json::json;
use theseus_host::fs::HostDir;
use theseus_native_helper_store::LedgerStore;
use theseus_native_protocol::v2::{self, broker::*, ledger::*};
fn owned_records() -> (DesktopRequest, Vec<Record>) {
    let header:IntegrationBrokerHeader=serde_json::from_value(json!({"profile":"native_integration_broker_v1","binding":"11".repeat(16),"manifest_digest":"22".repeat(32),"workspace_marker":"33".repeat(16),"os":"macos","target_id":"fixture","ownership_id":"44".repeat(16),"provider_epoch":"55".repeat(32),"session_id":"gui-fixture","session_epoch":"66".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":501,"effective_gid":20},"helper_principal":{"kind":"posix","effective_uid":501,"effective_gid":20},"display":{"kind":"mac_os_gui","domain":"gui/501"},"allowed_modes":["owned_executable"],"macos_security_session_id":123})).unwrap();
    header.validate().unwrap();
    let mut action:DesktopRequest=serde_json::from_value(json!({"helper_lease":"77".repeat(16),"helper_operation":"88".repeat(16),"broker_lease":"99".repeat(16),"binding_digest":header.digest().unwrap().to_hex(),"activation_nonce":"aa".repeat(16),"slot":"desktop","prior_binding_digest":"bb".repeat(32),"action":{"kind":"activate","owned_lifetime":{"kind":"macos_original_primary_v1"},"mode":"owned_executable","entry_digest":"cc".repeat(32),"expectation_digest":"dd".repeat(32),"close_binding":{"operation":"ee".repeat(16),"action_digest":"00".repeat(32),"nonce":"ff".repeat(16)}}})).unwrap();
    let digest = action.close_request().unwrap().digest().unwrap().to_hex();
    if let DesktopAction::Activate { close_binding, .. } = &mut action.action {
        close_binding.action_digest = digest;
    }
    let records = vec![
        Record::new(0, Entry::IntegrationBrokerHeader { header }).unwrap(),
        Record::new(
            1,
            Entry::LeaseReserved {
                intent: v2::IntentId::from_bytes([1; 16]),
                lease: v2::LeaseId::parse(&action.broker_lease).unwrap(),
            },
        )
        .unwrap(),
        Record::new(
            2,
            Entry::Broker {
                event: BrokerEvent::Accepted {
                    request: action.clone(),
                    request_digest: action.digest().unwrap().to_hex(),
                },
            },
        )
        .unwrap(),
    ];
    (action, records)
}
fn push(records: &mut Vec<Record>, event: BrokerEvent) {
    records.push(Record::new(records.len() as u16, Entry::Broker { event }).unwrap());
}

#[test]
fn owned_primary_store_reopen_preserves_original_pending_and_retirement_without_tree_claim() {
    let (request, mut records) = owned_records();
    let Entry::IntegrationBrokerHeader { header } = records[0].entry().clone() else {
        panic!()
    };
    let temp = tempfile::tempdir().unwrap();
    let ledger = HostDir::create_private(&temp.path().join("ledger")).unwrap();
    let workspace = HostDir::create_private(&temp.path().join("workspace")).unwrap();
    LedgerStore::bootstrap_broker(&ledger, &workspace, &header).unwrap();
    let mut store = LedgerStore::open_broker(&ledger, &workspace, &header).unwrap();
    for record in &records[1..] {
        store.append(record.clone()).unwrap();
    }
    drop(store);
    let mut store = LedgerStore::open_broker(&ledger, &workspace, &header).unwrap();
    let pending = BrokerLedgerView::replay(store.records())
        .unwrap()
        .query(&request)
        .unwrap();
    assert_eq!(
        pending.owned_lifetime.as_ref().unwrap().primary,
        OwnedPrimaryState::Pending
    );
    assert!(!pending.original_retired());
    // Physical reads go through the store's own locked handle while it is open;
    // Windows byte-range locks refuse reads through any other handle.
    let bytes = store.read_physical().unwrap();
    let bad = Record::new(
        3,
        Entry::Broker {
            event: BrokerEvent::LifetimeRecorded {
                operation: request.helper_operation.clone(),
                primary: BrokerPrimary::NotCreated,
                descendants: BrokerDescendants::NotCreated,
                cleanup_complete: true,
            },
        },
    )
    .unwrap();
    assert!(store.append(bad).is_err());
    assert_eq!(store.read_physical().unwrap(), bytes);
    push(
        &mut records,
        BrokerEvent::EffectRecorded {
            operation: request.helper_operation.clone(),
            receipt: BrokerEffectReceipt {
                evidence: DesktopEvidence::OwnedExecutable,
                acknowledged: true,
                receipt_digest: request.digest().unwrap().to_hex(),
            },
        },
    );
    store.append(records.last().unwrap().clone()).unwrap();
    let mut axis = pending.owned_lifetime.unwrap();
    axis.primary = OwnedPrimaryState::Exited;
    axis.local_resources = OwnedLocalState::Retired;
    push(
        &mut records,
        BrokerEvent::OwnedLifetimeRecorded {
            operation: request.helper_operation.clone(),
            state: axis,
        },
    );
    store.append(records.last().unwrap().clone()).unwrap();
    drop(store);
    let mut store = LedgerStore::open_broker(&ledger, &workspace, &header).unwrap();
    let view = BrokerLedgerView::replay(store.records()).unwrap();
    let retired = view.query(&request).unwrap();
    assert!(retired.original_retired());
    assert!(!retired.cleanup_complete);
    assert_eq!(retired.descendants, BrokerDescendants::Unknown);
    let close = request.close_request().unwrap();
    push(
        &mut records,
        BrokerEvent::Accepted {
            request: close.clone(),
            request_digest: close.digest().unwrap().to_hex(),
        },
    );
    store.append(records.last().unwrap().clone()).unwrap();
    let view = BrokerLedgerView::replay(store.records()).unwrap();
    assert_eq!(view.query(&request).unwrap(), retired);
    assert_eq!(
        view.query(&close).unwrap().owned_lifetime,
        retired.owned_lifetime
    );
    push(
        &mut records,
        BrokerEvent::Quarantined {
            operation: request.helper_operation.clone(),
        },
    );
    store.append(records.last().unwrap().clone()).unwrap();
    drop(store);
    let store = LedgerStore::open_broker(&ledger, &workspace, &header).unwrap();
    assert!(
        !BrokerLedgerView::replay(store.records())
            .unwrap()
            .query(&request)
            .unwrap()
            .original_retired()
    );
}
