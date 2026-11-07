use theseus_native_protocol::v2::{
    self,
    ledger::{self, Entry, LedgerView, Record},
};
#[test]
fn native_artifact_commitment_binds_both_lengths_and_hashes() {
    let base = v2::Digest::from_bytes([1; 32]);
    let target = v2::Digest::from_bytes([2; 32]);
    let original = v2::artifact_commitment(10, base, 20, target).unwrap();
    for changed in [
        v2::artifact_commitment(11, base, 20, target).unwrap(),
        v2::artifact_commitment(10, target, 20, target).unwrap(),
        v2::artifact_commitment(10, base, 21, target).unwrap(),
        v2::artifact_commitment(10, base, 20, base).unwrap(),
    ] {
        assert_ne!(original, changed);
    }
    assert!(v2::artifact_commitment(0, base, 20, target).is_err());
    assert!(v2::artifact_commitment(10, base, v2::MAX_NATIVE_FILE_BYTES + 1, target).is_err());
}
fn header(epoch: u8) -> Record {
    Record::new(
        0,
        Entry::NativeHeader {
            binding: v2::BindingId::from_bytes([1; 16]),
            manifest_digest: v2::Digest::from_bytes([2; 32]),
            workspace_marker: v2::WorkspaceMarker::from_bytes([3; 16]),
            os: v2::Os::Linux,
            capabilities: v2::NativeCapabilities,
            provider_epoch: v2::Digest::from_bytes([epoch; 32]),
        },
    )
    .unwrap()
}
#[test]
fn native_header_is_distinct_canonical_and_epoch_bound() {
    let h = header(4);
    let encoded = ledger::encode(&h).unwrap();
    let text = std::str::from_utf8(&encoded[4..encoded.len() - 32]).unwrap();
    assert!(text.contains("\"kind\":\"native_header\",\"profile\":\"native_catalog_v1\""));
    assert!(text.contains("\"capabilities\":[\"catalog_run\",\"helper_current_account\",\"settled_leaf_absence\",\"native_stream_inspect_64m\"]"));
    let mut d = ledger::Decoder::new();
    assert_eq!(d.feed(&encoded).unwrap().record, Some(h));
    d.finish().unwrap();
    assert_ne!(encoded, ledger::encode(&header(5)).unwrap());
    // The legacy header's strict schema rejects the new capability/epoch shape.
    use sha2::{Digest as _, Sha256};
    let wrong = text
        .replace("native_header", "header")
        .replace("native_catalog_v1", "trusted_fixture_v1");
    let mut raw = (wrong.len() as u32).to_be_bytes().to_vec();
    raw.extend(wrong.as_bytes());
    raw.extend(Sha256::digest(wrong.as_bytes()));
    assert!(ledger::Decoder::new().feed(&raw).is_err());
}
#[test]
fn native_revocation_survives_replay_and_blocks_later_effect_records() {
    let lease = v2::LeaseId::from_bytes([6; 16]);
    let reason = v2::PrefixAuthorityFailure::Replaced;
    let mut records = vec![
        header(4),
        Record::new(
            1,
            Entry::LeaseReserved {
                lease,
                intent: v2::IntentId::from_bytes([7; 16]),
            },
        )
        .unwrap(),
        Record::new(2, Entry::NativeAuthorityRevoked { lease, reason }).unwrap(),
    ];
    assert_eq!(
        LedgerView::replay(&records)
            .unwrap()
            .native_authority_failure(),
        Some(reason)
    );
    records.push(
        Record::new(
            3,
            Entry::OperationAccepted {
                lease,
                operation: v2::OperationId::from_bytes([8; 16]),
                request_digest: v2::Digest::from_bytes([9; 32]),
                operation_kind: v2::OperationKind::Run,
            },
        )
        .unwrap(),
    );
    assert!(LedgerView::replay(&records).is_err());
    records.pop();
    records.push(
        Record::new(
            3,
            Entry::LeaseClosed {
                lease,
                disposition: ledger::Disposition::AwaitingExternalReset,
            },
        )
        .unwrap(),
    );
    assert_eq!(
        LedgerView::replay(&records)
            .unwrap()
            .native_authority_failure(),
        Some(reason)
    );
}
fn description() -> v2::Reply {
    v2::Reply::NativeDescription {
        description: Box::new(v2::NativeDescription {
            binding: v2::BindingId::from_bytes([1; 16]),
            manifest_digest: v2::Digest::from_bytes([2; 32]),
            workspace_marker: v2::WorkspaceMarker::from_bytes([3; 16]),
            os: v2::Os::Linux,
            helper_principal: v2::Principal::Posix {
                effective_uid: 1000,
                effective_gid: 1000,
            },
            programs: v2::ProgramList::new(
                [
                    "check_base",
                    "check_data",
                    "check_target",
                    "configure",
                    "exercise",
                    "install_base",
                    "install_target",
                    "uninstall",
                ]
                .map(|id| v2::ProgramId::new(id).unwrap())
                .to_vec(),
            )
            .unwrap(),
            capabilities: v2::NativeCapabilities,
            provider_epoch: v2::Digest::from_bytes([4; 32]),
            artifact_commitment: v2::Digest::from_bytes([6; 32]),
            build: v2::Provenance::Unavailable {
                reason: "not supplied".into(),
            },
            os_build: v2::Provenance::Observed {
                value: "fixture".into(),
                source: "test".into(),
            },
        }),
    }
}
#[test]
fn native_wire_is_closed_and_does_not_widen_fixture_lengths() {
    let lease = v2::LeaseId::from_bytes([5; 16]);
    let path = v2::LeafName::new("reader").unwrap();
    for reply in [
        description(),
        v2::Reply::NativeFileInfo {
            lease,
            path: path.clone(),
            length: 64 * 1024 * 1024,
            sha256: v2::Digest::from_bytes([6; 32]),
        },
        v2::Reply::FileAbsent {
            lease,
            path: path.clone(),
        },
        v2::Reply::PrefixAuthorityRevoked {
            lease,
            reason: v2::PrefixAuthorityFailure::Replaced,
        },
    ] {
        let envelope = v2::Envelope::new(
            v2::SessionId::from_bytes([0; 16]),
            1,
            v2::Message::Response {
                reply_to: 1,
                result: reply,
            },
        )
        .unwrap();
        let raw = v2::encode(&envelope).unwrap();
        assert_eq!(v2::Decoder::new().feed(&raw).unwrap().frame, Some(envelope));
    }
    let envelope = v2::Envelope::new(
        v2::SessionId::from_bytes([0; 16]),
        1,
        v2::Message::Response {
            reply_to: 1,
            result: description(),
        },
    )
    .unwrap();
    let raw = v2::encode(&envelope).unwrap();
    let text = String::from_utf8(raw[4..].to_vec()).unwrap();
    for bad in [
        text.replace(
            &format!(",\"artifact_commitment\":\"{}\"", "06".repeat(32)),
            "",
        ),
        text.replace("native_catalog_v1", "trusted_fixture_v1"),
        text.replace("native_stream_inspect_64m", "unknown_capability"),
        text.replace("native_description", "description"),
        text.replace(
            "\"reason\":\"not supplied\"",
            "\"reason\":\"not supplied\",\"extra\":true",
        ),
    ] {
        let mut raw = (bad.len() as u32).to_be_bytes().to_vec();
        raw.extend(bad.as_bytes());
        assert!(v2::Decoder::new().feed(&raw).is_err());
    }
    assert!(v2::FileBytes::new(&vec![0; 4097]).is_err());
    assert!(
        v2::Envelope::new(
            v2::SessionId::from_bytes([0; 16]),
            1,
            v2::Message::Response {
                reply_to: 1,
                result: v2::Reply::FileInfo {
                    lease,
                    path: path.clone(),
                    length: 4097,
                    digest: v2::Digest::from_bytes([6; 32])
                }
            }
        )
        .is_err()
    );
    assert!(
        v2::Envelope::new(
            v2::SessionId::from_bytes([0; 16]),
            1,
            v2::Message::Response {
                reply_to: 1,
                result: v2::Reply::NativeFileInfo {
                    lease,
                    path,
                    length: v2::MAX_NATIVE_FILE_BYTES + 1,
                    sha256: v2::Digest::from_bytes([6; 32])
                }
            }
        )
        .is_err()
    );
}
