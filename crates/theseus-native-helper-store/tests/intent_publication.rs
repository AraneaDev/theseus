use std::fs;
use theseus_host::fs::{CommitScope, HostDir};
use theseus_native_helper_store::{IntentError, IntentRecord, IntentStore};
use theseus_native_protocol::v2::{BindingId, Digest, IntentId, Request, WorkspaceMarker};

const NAME: &str = "11111111111111111111111111111111.json";
const BODY: &str = "{\"schema\":1,\"intent\":\"11111111111111111111111111111111\",\"binding\":\"22222222222222222222222222222222\",\"manifest_digest\":\"3333333333333333333333333333333333333333333333333333333333333333\",\"workspace_marker\":\"44444444444444444444444444444444\"}";
fn record() -> IntentRecord {
    IntentRecord {
        intent: IntentId::from_bytes([0x11; 16]),
        binding: BindingId::from_bytes([0x22; 16]),
        manifest_digest: Digest::from_bytes([0x33; 32]),
        workspace_marker: WorkspaceMarker::from_bytes([0x44; 16]),
    }
}
fn fixture() -> (tempfile::TempDir, IntentStore) {
    let temp = tempfile::tempdir().unwrap();
    let root = HostDir::create_private(&temp.path().join("intent")).unwrap();
    (temp, IntentStore::new(root))
}
#[test]
fn fresh_prepare_commits_canonical_bytes_before_same_id_acquire() {
    let (temp, mut store) = fixture();
    let prepared = store.prepare(record()).unwrap();
    assert_eq!(
        fs::read(temp.path().join("intent").join(NAME)).unwrap(),
        BODY.as_bytes()
    );
    assert_eq!(prepared.record(), &record());
    assert_eq!(
        prepared.request(),
        Request::Acquire {
            intent: record().intent,
            binding: record().binding,
            manifest_digest: record().manifest_digest
        }
    );
    #[cfg(unix)]
    assert_eq!(prepared.commit_scope(), CommitScope::FileAndDirectorySynced);
    #[cfg(windows)]
    assert_eq!(
        prepared.commit_scope(),
        CommitScope::FileFlushedAtomicReplace
    );
}
#[test]
fn verified_equal_existing_intent_can_observe_another_publication_barrier() {
    let (temp, mut store) = fixture();
    let path = temp.path().join("intent").join(NAME);
    fs::write(&path, BODY).unwrap();
    let prepared = store.prepare(record()).unwrap();
    assert_eq!(fs::read(path).unwrap(), BODY.as_bytes());
    assert_eq!(prepared.record(), &record());
}
#[test]
fn changed_binding_manifest_or_marker_refuses_without_replacement() {
    for field in 0..3 {
        let (temp, mut store) = fixture();
        let path = temp.path().join("intent").join(NAME);
        fs::write(&path, BODY).unwrap();
        let mut changed = record();
        match field {
            0 => changed.binding = BindingId::from_bytes([5; 16]),
            1 => changed.manifest_digest = Digest::from_bytes([5; 32]),
            _ => changed.workspace_marker = WorkspaceMarker::from_bytes([5; 16]),
        }
        assert!(matches!(store.prepare(changed), Err(IntentError::Conflict)));
        assert_eq!(fs::read(path).unwrap(), BODY.as_bytes());
    }
}
#[test]
fn malformed_noncanonical_and_wrong_filename_identity_are_preserved() {
    let invalid = [
        String::new(),
        "{}".into(),
        format!("{BODY}\n"),
        BODY.replace("\"schema\":1", "\"schema\": 1"),
        BODY.replace("\"schema\":1", "\"schema\":1,\"schema\":1"),
        BODY.replace("\"schema\":1", "\"schema\":1,\"unknown\":0"),
        BODY.replace(
            "11111111111111111111111111111111",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        BODY.replace(
            "44444444444444444444444444444444",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ),
        "x".repeat(512),
        "x".repeat(513),
        "x".repeat(4096),
    ];
    for bytes in invalid {
        let (temp, mut store) = fixture();
        let path = temp.path().join("intent").join(NAME);
        fs::write(&path, bytes.as_bytes()).unwrap();
        assert!(matches!(
            store.prepare(record()),
            Err(IntentError::InvalidExisting)
        ));
        assert_eq!(fs::read(path).unwrap(), bytes.as_bytes());
    }
}

#[test]
fn lost_acquire_reply_reopens_reservation_for_original_intent_and_same_lease() {
    use theseus_native_helper_store::{BootstrapRequest, LedgerStore, StoreBinding, bootstrap};
    use theseus_native_protocol::v2::{
        AcquireView, LeaseId, LeaseView, Os,
        ledger::{Entry, Record},
    };
    let (temp, mut intents) = fixture();
    let ledger = HostDir::create_private(&temp.path().join("ledger")).unwrap();
    let workspace = HostDir::create_private(&temp.path().join("workspace")).unwrap();
    let bootstrap = bootstrap(
        &ledger,
        &workspace,
        BootstrapRequest {
            binding: record().binding,
            manifest_digest: record().manifest_digest,
            os: Os::Linux,
        },
    )
    .unwrap();
    let intent = IntentRecord {
        workspace_marker: bootstrap.workspace_marker(),
        ..record()
    };
    let prepared = intents.prepare(intent.clone()).unwrap();
    let binding = || StoreBinding {
        binding: intent.binding,
        manifest_digest: intent.manifest_digest,
        workspace_marker: intent.workspace_marker,
        os: Os::Linux,
    };
    let lease = LeaseId::from_bytes([0x55; 16]);
    {
        let mut helper = LedgerStore::open(&ledger, &workspace, binding()).unwrap();
        let Request::Acquire {
            intent: sent_intent,
            binding: sent_binding,
            manifest_digest,
        } = prepared.request()
        else {
            panic!("expected acquire")
        };
        assert_eq!(
            (sent_intent, sent_binding, manifest_digest),
            (intent.intent, intent.binding, intent.manifest_digest)
        );
        // Reservation reaches its file barrier; its acquisition reply is lost.
        helper
            .append(
                Record::new(
                    1,
                    Entry::LeaseReserved {
                        intent: sent_intent,
                        lease,
                    },
                )
                .unwrap(),
            )
            .unwrap();
    }
    let reopened = LedgerStore::open(&ledger, &workspace, binding()).unwrap();
    assert_eq!(
        reopened.restart_view().query_acquire(
            intent.intent,
            intent.binding,
            intent.manifest_digest
        ),
        Ok(AcquireView::Lease {
            lease,
            state: LeaseView::Held {
                intent: intent.intent
            }
        })
    );
    assert_eq!(reopened.records().len(), 2);
    assert_eq!(
        intents.prepare(intent.clone()).unwrap().request(),
        prepared.request()
    );
    let path = temp.path().join("intent").join(NAME);
    let evidence = fs::read(&path).unwrap();
    let changed = IntentRecord {
        workspace_marker: WorkspaceMarker::from_bytes([9; 16]),
        ..intent
    };
    assert!(matches!(
        intents.prepare(changed),
        Err(IntentError::Conflict)
    ));
    assert_eq!(fs::read(path).unwrap(), evidence);
    assert_eq!(reopened.records().len(), 2);
}
