use super::*;
#[test]
fn sut_transition_keeps_original_files_and_never_restores_revoked_owner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("original");
    std::fs::write(&path, b"base").unwrap();
    let file = File::open(&path).unwrap();
    let fp = crate::fs::fingerprint(&file).unwrap();
    let definition = SystemdDefinition {
        executable: "/fixture/base".into(),
        arguments: vec![],
        environment: vec![],
        working_directory: "/fixture".into(),
    };
    let mut owner = RegistrationOwner {
        _file: file,
        prior_files: vec![],
        transition_receipts: vec![],
        fingerprint: fp.clone(),
        definition_bytes: b"base".to_vec(),
        definition: definition.clone(),
        manager_instance: [1; 32],
        install_receipt: "original-install".into(),
        revoked: false,
    };
    let next = dir.path().join("target");
    std::fs::write(&next, b"target").unwrap();
    let replacement = File::open(&next).unwrap();
    let replacement_fp = crate::fs::fingerprint(&replacement).unwrap();
    owner
        .record_transition(
            replacement,
            replacement_fp,
            b"target".to_vec(),
            &definition,
            "accepted-upgrade".into(),
        )
        .unwrap();
    assert_eq!(owner.install_receipt(), "original-install");
    assert_eq!(owner.prior_files.len(), 1);
    assert_eq!(crate::fs::fingerprint(&owner.prior_files[0]).unwrap(), fp);
    assert_eq!(owner.definition_bytes(), b"target");
    owner.revoked = true;
    let original = File::open(&path).unwrap();
    assert!(
        owner
            .record_transition(
                original,
                fp,
                b"base".to_vec(),
                &definition,
                "accepted-reinstall".into()
            )
            .is_err()
    );
    assert!(owner.is_revoked());
    assert_eq!(owner.definition_bytes(), b"target");
    assert_eq!(owner.prior_files.len(), 1);
}
