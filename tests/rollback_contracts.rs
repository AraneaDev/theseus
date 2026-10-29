use serde_json::json;
use theseus::{
    checks::{CommandObservation, assess_expected_failure},
    model::CheckOutcome,
    recipe::{
        BackupScope, ExpectedFailureContract, RestorationPolicy, RollbackContract,
        RollbackRecoveryContract, validate_rollback_contract,
    },
};

fn contract() -> RollbackContract {
    RollbackContract {
        failure: ExpectedFailureContract {
            expected_exit: 9,
            stdout_contains: "upgrade failed after migration".into(),
        },
        recovery: RollbackRecoveryContract {
            change_checks: vec!["changed".into()],
            binary_checks: vec!["binary".into()],
            runtime_checks: vec!["runtime".into()],
            data_checks: vec!["reader".into()],
            restoration: RestorationPolicy::RestoreData {
                backup_root: "/workspace/backup".into(),
                scopes: vec![BackupScope {
                    id: "data".into(),
                    source: "/home/theseus/data".into(),
                    stored: "data".into(),
                }],
            },
        },
    }
}
fn observed() -> CommandObservation {
    CommandObservation {
        exit_status: Some(9),
        stdout: "upgrade failed after migration\n".into(),
        complete: true,
        error: None,
    }
}

#[test]
fn policy_roundtrips_and_accepts_inclusive_bounds() {
    for status in [1, 9, 125] {
        let mut c = contract();
        c.failure.expected_exit = status;
        validate_rollback_contract(&c).unwrap();
        assert_eq!(
            serde_json::from_value::<RollbackContract>(serde_json::to_value(&c).unwrap()).unwrap(),
            c
        );
        assert_eq!(
            toml::from_str::<RollbackContract>(&toml::to_string(&c).unwrap()).unwrap(),
            c
        );
    }
    let mut c = contract();
    c.recovery.restoration = RestorationPolicy::BinaryOnly;
    validate_rollback_contract(&c).unwrap();
    assert_eq!(
        serde_json::to_value(c.recovery.restoration).unwrap(),
        json!({"mode":"binary_only"})
    );
}

#[test]
fn policy_requires_valid_failure_and_distinct_nonempty_reference_roles() {
    for status in [i32::MIN, -1, 0, 126, 137, i32::MAX] {
        let mut c = contract();
        c.failure.expected_exit = status;
        assert!(validate_rollback_contract(&c).is_err(), "{status}");
    }
    for marker in ["", " \t\n", "\u{2003}"] {
        let mut c = contract();
        c.failure.stdout_contains = marker.into();
        assert!(validate_rollback_contract(&c).is_err());
    }
    for role in [
        "change_checks",
        "binary_checks",
        "runtime_checks",
        "data_checks",
    ] {
        for ids in [
            json!([]),
            json!([""]),
            json!([" \t"]),
            json!(["duplicate", "duplicate"]),
        ] {
            let mut value = serde_json::to_value(contract()).unwrap();
            value[role] = ids;
            assert!(
                validate_rollback_contract(&serde_json::from_value(value).unwrap()).is_err(),
                "{role}"
            );
        }
    }
    for roles in [
        ("change_checks", "binary_checks"),
        ("change_checks", "data_checks"),
        ("binary_checks", "data_checks"),
        ("runtime_checks", "change_checks"),
        ("runtime_checks", "binary_checks"),
        ("runtime_checks", "data_checks"),
    ] {
        let mut value = serde_json::to_value(contract()).unwrap();
        value[roles.0] = json!(["shared"]);
        value[roles.1] = json!(["shared"]);
        assert!(validate_rollback_contract(&serde_json::from_value(value).unwrap()).is_err());
    }
}

#[test]
fn every_nested_policy_type_rejects_unknown_fields_including_binary_only() {
    for location in ["contract", "failure", "restoration", "scope"] {
        let mut value = serde_json::to_value(contract()).unwrap();
        match location {
            "contract" => value["extra"] = true.into(),
            "failure" => value["failure"]["extra"] = true.into(),
            "restoration" => value["restoration"]["extra"] = true.into(),
            _ => value["restoration"]["scopes"][0]["extra"] = true.into(),
        }
        assert!(
            serde_json::from_value::<RollbackContract>(value).is_err(),
            "{location}"
        );
    }
    for extra in [
        json!({"backup_root":"/workspace/backup"}),
        json!({"scopes":[]}),
        json!({"extra":true}),
    ] {
        let mut value = json!({"mode":"binary_only"});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(serde_json::from_value::<RestorationPolicy>(value).is_err());
    }
}

#[test]
fn restore_data_requires_bounded_valid_unique_scope_ids() {
    for id in ["", "bad id", "../x", "é", &"x".repeat(65)] {
        let mut c = contract();
        if let RestorationPolicy::RestoreData { scopes, .. } = &mut c.recovery.restoration {
            scopes[0].id = id.into();
        }
        assert!(validate_rollback_contract(&c).is_err(), "{id}");
    }
    for count in [0, 1, 64, 65] {
        let mut c = contract();
        if let RestorationPolicy::RestoreData { scopes, .. } = &mut c.recovery.restoration {
            *scopes = (0..count)
                .map(|i| BackupScope {
                    id: format!("scope_{i}-A"),
                    source: format!("/data/{i}"),
                    stored: format!("copies/{i}"),
                })
                .collect();
        }
        assert_eq!(
            validate_rollback_contract(&c).is_ok(),
            (1..=64).contains(&count),
            "count {count}"
        );
    }
    let mut c = contract();
    if let RestorationPolicy::RestoreData { scopes, .. } = &mut c.recovery.restoration {
        scopes.push(BackupScope {
            id: "data".into(),
            source: "/other".into(),
            stored: "other".into(),
        });
    }
    assert!(validate_rollback_contract(&c).is_err());
}

#[test]
fn restore_paths_are_normalized_and_storage_is_separate_and_safe() {
    for path in [
        "",
        "relative",
        "/",
        "/home//data",
        "/home/./data",
        "/home/data/",
        "/home/../data",
        "/home/\0data",
    ] {
        let mut c = contract();
        if let RestorationPolicy::RestoreData { scopes, .. } = &mut c.recovery.restoration {
            scopes[0].source = path.into();
        }
        assert!(validate_rollback_contract(&c).is_err(), "source {path:?}");
    }
    for root in [
        "",
        "relative",
        "/",
        "/workspace//backup",
        "/workspace/./backup",
        "/workspace/backup/",
        "/workspace/../backup",
        "/workspace/\0backup",
        "/inputs",
        "/inputs/copy",
        "/proc/copy",
        "/sys/copy",
        "/dev/copy",
        "/home/theseus",
        "/home/theseus/data/copy",
    ] {
        let mut c = contract();
        if let RestorationPolicy::RestoreData { backup_root, .. } = &mut c.recovery.restoration {
            *backup_root = root.into();
        }
        assert!(validate_rollback_contract(&c).is_err(), "root {root:?}");
    }
    for path in [
        "",
        "/absolute",
        ".",
        "..",
        "../data",
        "data/../copy",
        "data/./copy",
        "data//copy",
        "data/",
        "data/\0copy",
    ] {
        let mut c = contract();
        if let RestorationPolicy::RestoreData { scopes, .. } = &mut c.recovery.restoration {
            scopes[0].stored = path.into();
        }
        assert!(validate_rollback_contract(&c).is_err(), "stored {path:?}");
    }
    for (source, stored) in [
        ("/home/theseus/data", "other"),
        ("/home/theseus/data/child", "other"),
        ("/home/theseus", "other"),
        ("/other", "data"),
        ("/other", "data/child"),
    ] {
        let mut c = contract();
        if let RestorationPolicy::RestoreData { scopes, .. } = &mut c.recovery.restoration {
            scopes.push(BackupScope {
                id: "other".into(),
                source: source.into(),
                stored: stored.into(),
            });
        }
        assert!(validate_rollback_contract(&c).is_err(), "{source}/{stored}");
    }
    let mut c = contract();
    if let RestorationPolicy::RestoreData { scopes, .. } = &mut c.recovery.restoration {
        scopes.push(BackupScope {
            id: "other".into(),
            source: "/home/theseus/database".into(),
            stored: "database".into(),
        });
    }
    validate_rollback_contract(&c).unwrap();
}

#[test]
fn expected_failure_requires_complete_exact_nonzero_status_and_literal_marker() {
    let expected = contract().failure;
    assert_eq!(
        assess_expected_failure(&expected, &observed()).outcome,
        CheckOutcome::Passed
    );
    for status in [0, 1, 8, 10, 126, 127, 137, -1] {
        assert_eq!(
            assess_expected_failure(
                &expected,
                &CommandObservation {
                    exit_status: Some(status),
                    ..observed()
                }
            )
            .outcome,
            CheckOutcome::Failed
        );
    }
    for stdout in ["", "UPGRADE FAILED AFTER MIGRATION", "upgrade.*migration"] {
        assert_eq!(
            assess_expected_failure(
                &expected,
                &CommandObservation {
                    stdout: stdout.into(),
                    ..observed()
                }
            )
            .outcome,
            CheckOutcome::Failed
        );
    }
    for partial in [
        CommandObservation {
            complete: false,
            ..observed()
        },
        CommandObservation {
            exit_status: None,
            ..observed()
        },
        CommandObservation {
            error: Some("private transport".into()),
            ..observed()
        },
        CommandObservation {
            complete: false,
            exit_status: Some(0),
            stdout: "wrong".into(),
            error: None,
        },
    ] {
        assert_eq!(
            assess_expected_failure(&expected, &partial).outcome,
            CheckOutcome::Incomplete
        );
    }
    for status in [0, 126, -1] {
        let invalid = ExpectedFailureContract {
            expected_exit: status,
            ..expected.clone()
        };
        assert_eq!(
            assess_expected_failure(&invalid, &observed()).outcome,
            CheckOutcome::Incomplete
        );
    }
    for marker in ["", " \t", "\u{2003}"] {
        let invalid = ExpectedFailureContract {
            stdout_contains: marker.into(),
            ..expected.clone()
        };
        assert_eq!(
            assess_expected_failure(&invalid, &observed()).outcome,
            CheckOutcome::Incomplete
        );
    }
}

#[test]
fn failure_messages_are_static_bounded_and_do_not_echo_values() {
    let private = "PRIVATE_秘密".repeat(2000);
    let expected = ExpectedFailureContract {
        expected_exit: 9,
        stdout_contains: private.clone(),
    };
    for observation in [
        observed(),
        CommandObservation {
            stdout: private.clone(),
            ..observed()
        },
        CommandObservation {
            error: Some(private),
            ..observed()
        },
    ] {
        let assessment = assess_expected_failure(&expected, &observation);
        assert!(!assessment.message.is_empty());
        assert!(assessment.message.len() <= 1024);
        assert!(!assessment.message.contains("PRIVATE"));
        assert!(!assessment.message.contains("秘密"));
    }
}

#[test]
fn binary_only_rejects_backup_fields() {
    for extra in [
        json!({"backup_root":"/workspace/backup"}),
        json!({"scopes":[]}),
        json!({"extra":true}),
    ] {
        let mut value = json!({"mode":"binary_only"});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(serde_json::from_value::<RestorationPolicy>(value).is_err());
    }
}

#[test]
fn policy_decode_rejects_missing_fields_unknown_modes_and_wrong_types() {
    let valid = serde_json::to_value(contract()).unwrap();
    for field in [
        "failure",
        "change_checks",
        "binary_checks",
        "runtime_checks",
        "data_checks",
        "restoration",
    ] {
        let mut value = valid.clone();
        value.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<RollbackContract>(value).is_err(),
            "{field}"
        );
    }
    for field in ["expected_exit", "stdout_contains"] {
        let mut value = valid.clone();
        value["failure"].as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<RollbackContract>(value).is_err(),
            "failure/{field}"
        );
    }
    for field in ["backup_root", "scopes"] {
        let mut value = valid.clone();
        value["restoration"].as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<RollbackContract>(value).is_err(),
            "restoration/{field}"
        );
    }
    for field in ["id", "source", "stored"] {
        let mut value = valid.clone();
        value["restoration"]["scopes"][0]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            serde_json::from_value::<RollbackContract>(value).is_err(),
            "scope/{field}"
        );
    }
    for policy in [
        json!({"mode":"automatic"}),
        json!({"mode":"BinaryOnly"}),
        json!({}),
        json!({"mode":"binary_only","backup_root":null}),
    ] {
        assert!(serde_json::from_value::<RestorationPolicy>(policy).is_err());
    }
    let mut value = valid;
    value["failure"]["expected_exit"] = "9".into();
    assert!(serde_json::from_value::<RollbackContract>(value).is_err());
}

#[test]
fn validators_preserve_literal_markers_ids_and_component_boundaries() {
    let mut c = contract();
    c.failure.stdout_contains = " failed.*\n".into();
    c.recovery.change_checks = vec![" change ".into()];
    if let RestorationPolicy::RestoreData {
        backup_root,
        scopes,
    } = &mut c.recovery.restoration
    {
        *backup_root = "/inputsevil/backup".into();
        scopes[0].id = "x".repeat(64);
    }
    let original = c.clone();
    validate_rollback_contract(&c).unwrap();
    assert_eq!(c, original);
    let observed = CommandObservation {
        stdout: " failed.*\n".into(),
        ..observed()
    };
    assert_eq!(
        assess_expected_failure(&c.failure, &observed).outcome,
        CheckOutcome::Passed
    );
    assert_eq!(
        assess_expected_failure(
            &c.failure,
            &CommandObservation {
                stdout: "failed.*".into(),
                ..observed
            }
        )
        .outcome,
        CheckOutcome::Failed
    );
}
