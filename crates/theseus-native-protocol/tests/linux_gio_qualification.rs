//! Pure graph/context contracts. Synthetic paths/data are not native SDK qualification.
use theseus_native_protocol::v2;
#[path = "support/linux_gio_qualification.rs"]
mod support;
use support::fixture;
#[test]
fn attestation_binds_original_prepare_actual_graph_and_base_context_without_digest_cycle() {
    let (m, c, a) = fixture();
    a.validate_context(&m, &c).unwrap();
    a.validate_broker_header(c.desktop_broker_header.as_ref().unwrap())
        .unwrap();
    assert_ne!(
        c.digest().unwrap(),
        c.qualification_binding_digest().unwrap()
    );
    assert_ne!(
        c.desktop_broker_header.as_ref().unwrap().digest().unwrap(),
        c.desktop_broker_header
            .as_ref()
            .unwrap()
            .qualification_binding_digest()
            .unwrap()
    );
    let mut changed = c.clone();
    changed
        .desktop_broker_header
        .as_mut()
        .unwrap()
        .session_epoch = "aa".repeat(32);
    assert!(a.validate_context(&m, &changed).is_err());
    for field in ["prepare", "graph", "context", "full"] {
        let mut bad = a.clone();
        match field {
            "prepare" => bad.prepare_invocation_digest = "bb".repeat(32),
            "graph" => bad.applications[0].artifact.sha256 = "bb".repeat(32),
            "context" => bad.base_context_digest = "bb".repeat(32),
            _ => bad.manifest_digest = "bb".repeat(32),
        }
        assert!(bad.validate_context(&m, &c).is_err(), "{field}");
    }
}
#[test]
fn qualification_bootstrap_has_exact_prelease_placement_and_real_codec_roundtrip() {
    let (m, c, a) = fixture();
    use v2::ledger::{Entry, LedgerView, Record};
    let core = v2::Digest::parse(&a.core_manifest_digest).unwrap();
    let integration = v2::Digest::parse(&a.integration_manifest_digest).unwrap();
    let mut records = vec![
        Record::new(
            0,
            Entry::IntegrationHeader {
                artifact_commitment: v2::Digest::from_bytes([9; 32]),
                binding: v2::BindingId::parse(&a.binding).unwrap(),
                manifest_digest: v2::Digest::parse(&a.manifest_digest).unwrap(),
                workspace_marker: v2::WorkspaceMarker::parse(&a.workspace_marker).unwrap(),
                os: v2::Os::Linux,
                provider_epoch: v2::Digest::parse(&a.provider_epoch).unwrap(),
                core_manifest_digest: core,
                integration_manifest_digest: integration,
                context_digest: c.digest().unwrap(),
                ownership_id: a.ownership_id.clone(),
            },
        )
        .unwrap(),
        Record::new(
            1,
            Entry::DesktopAuthorityDeclared {
                manifest: Box::new(m),
                context: Box::new(c),
            },
        )
        .unwrap(),
    ];
    let lease = Entry::LeaseReserved {
        intent: v2::IntentId::from_bytes([7; 16]),
        lease: v2::LeaseId::from_bytes([8; 16]),
    };
    let mut missing = records.clone();
    missing.push(Record::new(2, lease.clone()).unwrap());
    assert!(LedgerView::replay(&missing).is_err());
    records.push(
        Record::new(
            2,
            Entry::LinuxGioQualificationDeclared {
                attestation: Box::new(a),
            },
        )
        .unwrap(),
    );
    records.push(Record::new(3, lease).unwrap());
    LedgerView::replay(&records).unwrap();
    for record in records {
        let bytes = v2::ledger::encode(&record).unwrap();
        assert_eq!(
            v2::ledger::Decoder::new().feed(&bytes).unwrap().record,
            Some(record)
        );
    }
}
#[test]
fn omitted_qualification_references_preserve_old_context_and_header_bytes() {
    let (_, mut context, _) = fixture();
    context.linux_gio_qualification = None;
    context
        .desktop_broker_header
        .as_mut()
        .unwrap()
        .linux_gio_qualification = None;
    let encoded = serde_json::to_value(&context).unwrap();
    assert!(encoded.get("linux_gio_qualification").is_none());
    assert!(
        encoded["desktop_broker_header"]
            .get("linux_gio_qualification")
            .is_none()
    );
}

#[test]
fn sdk_image_closure_accepts_32_support_images_and_preserves_strict_bounds() {
    let (_, _, mut attestation) = fixture();
    let support_artifact = attestation.sdk.images[0].artifact.clone();
    for index in 0..32 {
        let mut artifact = support_artifact.clone();
        artifact.relative_path = format!("libsupport{index}.so");
        attestation.sdk.images.push(v2::LinuxSdkImage {
            role: v2::LinuxSdkImageRole::Support,
            artifact,
        });
    }
    assert_eq!(attestation.sdk.images.len(), 35);
    attestation.sdk.validate().unwrap();
    attestation.sdk.digest().unwrap();
    let record = v2::ledger::Record::new(
        2,
        v2::ledger::Entry::LinuxGioQualificationDeclared {
            attestation: Box::new(attestation.clone()),
        },
    )
    .unwrap();
    let encoded = v2::ledger::encode(&record).unwrap();
    assert!(encoded.len() <= v2::ledger::MAX_RECORD_BYTES);
    assert_eq!(
        v2::ledger::Decoder::new().feed(&encoded).unwrap().record,
        Some(record)
    );

    let mut excess = attestation.sdk.clone();
    let mut extra = excess.images[3].clone();
    extra.artifact.relative_path = "libsupport32.so".into();
    excess.images.push(extra);
    assert_eq!(excess.images.len(), 36);
    assert_eq!(
        excess
            .images
            .iter()
            .filter(|i| i.role == v2::LinuxSdkImageRole::Support)
            .count(),
        33
    );
    assert!(excess.validate().is_err());
    for mutation in [
        "duplicate_core",
        "artifact_alias",
        "different_abi",
        "missing_core",
    ] {
        let mut invalid = attestation.sdk.clone();
        match mutation {
            "duplicate_core" => invalid.images[3].role = v2::LinuxSdkImageRole::Gio,
            "artifact_alias" => invalid.images[3].artifact = invalid.images[0].artifact.clone(),
            "different_abi" => {
                invalid.images[3].artifact.native_abi = v2::NativeLinuxAbi::LinuxAarch64
            }
            _ => invalid.images[0].role = v2::LinuxSdkImageRole::Support,
        }
        assert!(invalid.validate().is_err(), "{mutation}");
    }
    // More images do not enlarge the existing encoded record allowance.
    for image in &mut attestation.sdk.images {
        image.artifact.root = format!("/{}", "a".repeat(500));
    }
    attestation.sdk.validate().unwrap();
    attestation.digest().unwrap();
    let oversized = v2::ledger::Record::new(
        2,
        v2::ledger::Entry::LinuxGioQualificationDeclared {
            attestation: Box::new(attestation),
        },
    )
    .unwrap();
    assert!(matches!(
        v2::ledger::encode(&oversized),
        Err(v2::ledger::LedgerError::InvalidLength)
    ));
}
