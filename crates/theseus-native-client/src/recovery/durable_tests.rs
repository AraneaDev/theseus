use super::*;
use theseus_native_helper_store::IntentStore;
fn mode(tamper: bool) -> (tempfile::TempDir, ClientMode, crate::ExpectedHelper) {
    let temp = tempfile::tempdir().unwrap();
    let mut store = IntentStore::new(
        theseus_host::fs::HostDir::create_private(&temp.path().join("intent")).unwrap(),
    );
    let expected = crate::pump::tests::expected();
    let b = &expected.binding;
    let record = IntentRecord {
        intent: v2::IntentId::from_bytes([8; 16]),
        binding: b.binding,
        manifest_digest: b.manifest_digest,
        workspace_marker: b.workspace_marker,
    };
    store.prepare(record.clone()).unwrap();
    let lease = v2::LeaseId::from_bytes([5; 16]);
    let request = v2::Request::Run {
        lease,
        operation: v2::OperationId::from_bytes([6; 16]),
        program: expected.programs.as_slice()[0].clone(),
    };
    let digest = if tamper {
        v2::Digest::from_bytes([0; 32])
    } else {
        v2::request_digest(&request).unwrap()
    };
    let image = RecoveryImage {
        binding: record.clone(),
        profile: expected.profile,
        operations: vec![RecoveredOperation { request, digest }],
        lease: Some(lease),
    };
    let original = store.verify_original(&record).unwrap();
    (
        temp,
        ClientMode::Recover {
            image: Box::new(image),
            original,
        },
        expected,
    )
}
#[test]
fn canonical_durable_image_roundtrips_and_rejects_truncation_trailing_and_duplicate_identity() {
    let (_temp, mode, expected) = mode(false);
    let ClientMode::Recover { image, original } = mode else {
        unreachable!()
    };
    let bytes = image.encode().unwrap();
    let decoded = RecoveryImage::decode(&bytes).unwrap();
    assert_eq!(decoded.encode().unwrap(), bytes);
    assert!(RecoveryImage::decode(&bytes[..bytes.len() - 1]).is_err());
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(RecoveryImage::decode(&trailing).is_err());
    let mut duplicate = decoded.clone();
    duplicate.operations.push(duplicate.operations[0].clone());
    assert!(duplicate.encode().is_err());
    let (mut recovery, query_only) = Recovery::from_mode(
        ClientMode::Recover {
            image: Box::new(decoded),
            original,
        },
        &expected,
    )
    .unwrap();
    assert!(query_only);
    let (lease, operation) = recovery.identities[0].ids();
    assert!(
        recovery
            .request(Action::Query { lease, operation }, &expected)
            .is_ok()
    );
    assert_eq!(
        recovery.request(
            Action::Run {
                lease,
                operation,
                program: expected.programs.as_slice()[0].clone()
            },
            &expected
        ),
        Err(ClientError::RecoveryRequired)
    );
}
#[test]
fn recovered_image_rejects_digest_changes_and_all_new_mutations() {
    let (_t, m, e) = mode(true);
    assert!(matches!(
        Recovery::from_mode(m, &e),
        Err(ClientError::Protocol)
    ));
    let (_t, m, e) = mode(false);
    let (mut recovery, query_only) = Recovery::from_mode(m, &e).unwrap();
    assert!(query_only);
    let lease = v2::LeaseId::from_bytes([5; 16]);
    let operation = v2::OperationId::from_bytes([6; 16]);
    assert!(
        recovery
            .request(Action::Query { lease, operation }, &e)
            .is_ok()
    );
    assert_eq!(
        recovery.request(
            Action::Run {
                lease,
                operation,
                program: e.programs.as_slice()[0].clone()
            },
            &e
        ),
        Err(ClientError::RecoveryRequired)
    );
    assert_eq!(
        recovery.request(
            Action::Query {
                lease,
                operation: v2::OperationId::from_bytes([7; 16])
            },
            &e
        ),
        Err(ClientError::RecoveryRequired)
    );
}
