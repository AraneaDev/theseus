use serde_json::json;
use theseus::{
    checks::{CommandObservation, assess_rejection},
    model::CheckOutcome,
    recipe::{DowngradeContract, validate_downgrade_contract},
};

fn refusal() -> CommandObservation {
    CommandObservation {
        exit_status: Some(9),
        stdout: "data format is newer than supported\n".into(),
        complete: true,
        error: None,
    }
}

fn safe_rejection(binary_checks: &[&str], data_checks: &[&str]) -> DowngradeContract {
    DowngradeContract::SafeRejection {
        expected_exit: 9,
        stdout_contains: "data format is newer".into(),
        binary_checks: binary_checks.iter().map(|id| (*id).into()).collect(),
        data_checks: data_checks.iter().map(|id| (*id).into()).collect(),
    }
}

#[test]
fn complete_exact_declared_refusal_passes() {
    assert_eq!(
        assess_rejection(9, "data format is newer", &refusal()).outcome,
        CheckOutcome::Passed
    );
}

#[test]
fn complete_wrong_status_fails_even_with_matching_stdout() {
    for status in [0, 1, 8, 10, 126, 127, 137, -1] {
        let observed = CommandObservation {
            exit_status: Some(status),
            ..refusal()
        };
        assert_eq!(
            assess_rejection(9, "data format is newer", &observed).outcome,
            CheckOutcome::Failed,
            "status {status}"
        );
    }
}

#[test]
fn complete_wrong_or_missing_literal_marker_fails() {
    for stdout in ["", "DATA FORMAT IS NEWER", "data format is older"] {
        let observed = CommandObservation {
            stdout: stdout.into(),
            ..refusal()
        };
        assert_eq!(
            assess_rejection(9, "data format is newer", &observed).outcome,
            CheckOutcome::Failed
        );
    }
    assert_eq!(
        assess_rejection(9, "data.*newer", &refusal()).outcome,
        CheckOutcome::Failed,
        "the marker is a literal substring, not a regular expression"
    );
}

#[test]
fn unavailable_or_truncated_evidence_is_incomplete_even_with_matching_stdout() {
    for observed in [
        CommandObservation {
            exit_status: None,
            ..refusal()
        },
        CommandObservation {
            complete: false,
            ..refusal()
        },
        CommandObservation {
            error: Some("transport failed".into()),
            ..refusal()
        },
        CommandObservation {
            error: Some(String::new()),
            ..refusal()
        },
        CommandObservation {
            exit_status: Some(0),
            stdout: "wrong marker".into(),
            complete: false,
            error: None,
        },
    ] {
        assert_eq!(
            assess_rejection(9, "data format is newer", &observed).outcome,
            CheckOutcome::Incomplete
        );
    }
}

#[test]
fn classifier_fails_closed_for_invalid_expected_inputs() {
    for status in [i32::MIN, -1, 0, 126, 127, 137, 255, i32::MAX] {
        let observed = CommandObservation {
            exit_status: Some(status),
            ..refusal()
        };
        assert_eq!(
            assess_rejection(status, "data format is newer", &observed).outcome,
            CheckOutcome::Incomplete,
            "invalid expected status {status}"
        );
    }
    for marker in ["", " \t\n", "\u{2003}"] {
        let observed = CommandObservation {
            stdout: marker.into(),
            ..refusal()
        };
        assert_eq!(
            assess_rejection(9, marker, &observed).outcome,
            CheckOutcome::Incomplete
        );
    }
}

#[test]
fn refusal_status_bounds_are_inclusive() {
    for status in [1, 125] {
        let contract = DowngradeContract::SafeRejection {
            expected_exit: status,
            stdout_contains: "refused".into(),
            binary_checks: vec!["binary".into()],
            data_checks: vec!["data".into()],
        };
        assert!(validate_downgrade_contract(&contract).is_ok());
        assert_eq!(
            assess_rejection(
                status,
                "refused",
                &CommandObservation {
                    exit_status: Some(status),
                    stdout: "refused\n".into(),
                    complete: true,
                    error: None,
                }
            )
            .outcome,
            CheckOutcome::Passed
        );
    }
}

#[test]
fn rejection_summaries_are_bounded_utf8_and_do_not_expose_observed_values() {
    let private = format!("PRIVATE_VALUE{}", "秘密🦀".repeat(2000));
    for (status, marker, observed) in [
        (9, private.as_str(), refusal()),
        (
            9,
            "data format is newer",
            CommandObservation {
                stdout: private.clone(),
                ..refusal()
            },
        ),
        (
            9,
            "data format is newer",
            CommandObservation {
                error: Some(private.clone()),
                ..refusal()
            },
        ),
        (
            9,
            private.as_str(),
            CommandObservation {
                stdout: private.clone(),
                ..refusal()
            },
        ),
        (
            0,
            private.as_str(),
            CommandObservation {
                exit_status: Some(0),
                stdout: private.clone(),
                ..refusal()
            },
        ),
    ] {
        let message = assess_rejection(status, marker, &observed).message;
        assert!(!message.is_empty());
        assert!(message.len() <= 1024, "summary exceeds 1024 bytes");
        assert!(!message.contains("PRIVATE_VALUE"));
        assert!(!message.contains("秘密"));
    }
}

#[test]
fn tagged_contracts_decode_validate_and_serialize_without_changing_the_policy() {
    for document in [
        json!({"expect": "compatible", "data_checks": ["older-reads-newer-data"]}),
        json!({
            "expect": "safe_rejection", "expected_exit": 9,
            "stdout_contains": "data format is newer",
            "binary_checks": ["newer-binary-stable"],
            "data_checks": ["newer-data-stable"]
        }),
    ] {
        let contract: DowngradeContract = serde_json::from_value(document.clone()).unwrap();
        validate_downgrade_contract(&contract).unwrap();
        assert_eq!(serde_json::to_value(&contract).unwrap(), document);
        let encoded = toml::to_string(&contract).unwrap();
        let decoded: DowngradeContract = toml::from_str(&encoded).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), document);
    }
}

#[test]
fn policy_decode_rejects_unknown_variants_fields_and_missing_required_fields() {
    for document in [
        json!({"expect": "automatic", "data_checks": ["data"]}),
        json!({"expect": "Compatible", "data_checks": ["data"]}),
        json!({"data_checks": ["data"]}),
        json!({"expect": "compatible"}),
        json!({"expect": "compatible", "data_checks": ["data"], "extra": true}),
        json!({"expect": "compatible", "data_checks": ["data"], "expected_exit": 9}),
        json!({"expect": "safe_rejection", "expected_exit": 9,
            "stdout_contains": "refused", "data_checks": ["data"]}),
        json!({"expect": "safe_rejection", "expected_exit": 9,
            "stdout_contains": "refused", "binary_checks": ["binary"]}),
        json!({"expect": "safe_rejection", "expected_exit": 9,
            "binary_checks": ["binary"], "data_checks": ["data"]}),
        json!({"expect": "safe_rejection", "stdout_contains": "refused",
            "binary_checks": ["binary"], "data_checks": ["data"]}),
        json!({"expect": "safe_rejection", "expected_exit": 9,
            "stdout_contains": "refused", "binary_checks": ["binary"],
            "data_checks": ["data"], "extra": true}),
    ] {
        assert!(
            serde_json::from_value::<DowngradeContract>(document.clone()).is_err(),
            "accepted malformed policy: {document}"
        );
    }
}

#[test]
fn compatible_requires_nonempty_unique_data_reference_ids() {
    for data_checks in [vec![], vec![""], vec![" \t\n"], vec!["data", "data"]] {
        let contract = DowngradeContract::Compatible {
            data_checks: data_checks.into_iter().map(str::to_owned).collect(),
        };
        assert!(validate_downgrade_contract(&contract).is_err());
    }
    assert!(
        validate_downgrade_contract(&DowngradeContract::Compatible {
            data_checks: vec!["data-a".into(), "data-b".into()],
        })
        .is_ok()
    );
}

#[test]
fn safe_rejection_requires_both_nonempty_unique_reference_lists() {
    for (binary, data) in [
        (vec![], vec!["data"]),
        (vec!["binary"], vec![]),
        (vec![""], vec!["data"]),
        (vec!["binary"], vec![""]),
        (vec![" \t"], vec!["data"]),
        (vec!["binary"], vec!["\n "]),
        (vec!["binary", "binary"], vec!["data"]),
        (vec!["binary"], vec!["data", "data"]),
    ] {
        assert!(validate_downgrade_contract(&safe_rejection(&binary, &data)).is_err());
    }
    assert!(
        validate_downgrade_contract(&safe_rejection(
            &["binary-a", "binary-b"],
            &["data-a", "data-b"]
        ))
        .is_ok()
    );
}

#[test]
fn binary_and_data_reference_roles_cannot_share_an_id() {
    assert!(
        validate_downgrade_contract(&safe_rejection(&["binary", "shared"], &["shared", "data"]))
            .is_err()
    );
}

#[test]
fn safe_rejection_rejects_invalid_refusal_status_and_empty_markers() {
    for status in [i32::MIN, -1, 0, 126, 127, 137, 255, i32::MAX] {
        let mut contract = safe_rejection(&["binary"], &["data"]);
        if let DowngradeContract::SafeRejection { expected_exit, .. } = &mut contract {
            *expected_exit = status;
        }
        assert!(
            validate_downgrade_contract(&contract).is_err(),
            "status {status}"
        );
    }
    for marker in ["", " \n\t", "\u{2003}"] {
        let mut contract = safe_rejection(&["binary"], &["data"]);
        if let DowngradeContract::SafeRejection {
            stdout_contains, ..
        } = &mut contract
        {
            *stdout_contains = marker.into();
        }
        assert!(validate_downgrade_contract(&contract).is_err());
    }
}

#[test]
fn validation_preserves_literal_markers_and_reference_ids() {
    let contract = DowngradeContract::SafeRejection {
        expected_exit: 9,
        stdout_contains: " refused.*\n".into(),
        binary_checks: vec![" binary ".into()],
        data_checks: vec![" data ".into()],
    };
    validate_downgrade_contract(&contract).unwrap();
    assert_eq!(
        serde_json::to_value(contract).unwrap(),
        json!({
            "expect": "safe_rejection", "expected_exit": 9,
            "stdout_contains": " refused.*\n", "binary_checks": [" binary "],
            "data_checks": [" data "]
        })
    );
    assert_eq!(
        assess_rejection(
            9,
            " refused.*\n",
            &CommandObservation {
                stdout: " refused.*\n".into(),
                ..refusal()
            }
        )
        .outcome,
        CheckOutcome::Passed
    );
    assert_eq!(
        assess_rejection(
            9,
            " refused.*\n",
            &CommandObservation {
                stdout: "refused.*".into(),
                ..refusal()
            }
        )
        .outcome,
        CheckOutcome::Failed
    );
}
