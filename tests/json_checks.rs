use theseus::{
    checks::{JsonCommandObservation, evaluate_json_command},
    model::{CheckOutcome, CheckResult},
    recipe::Check,
};
fn check() -> Check {
    serde_json::from_value(serde_json::json!({"id":"inventory", "checkpoint":"target_installed", "kind":"json_command", "script":"printf '[]'", "expected_exit":0, "max_stdout_bytes":262144, "assertions":[{"kind":"array_match", "pointer":"", "identity":[{"pointer":"/id","equals_json":"\"target@market\""}], "count":1, "values":[{"pointer":"/scope","equals_json":"\"user\""},{"pointer":"/enabled","equals_json":"true"}]}]})).unwrap()
}
fn evaluate(stdout: &str, complete: bool, utf8: bool) -> CheckResult {
    evaluate_json_command(
        &check(),
        &JsonCommandObservation {
            exit_status: Some(0),
            stdout,
            complete,
            utf8,
            error: None,
        },
    )
}
#[test]
fn inventory_selection_is_exact_complete_and_strict() {
    assert_eq!(
        evaluate(
            r#"[{"id":"companion@market"},{"id":"target@market","scope":"user","enabled":true}]"#,
            true,
            true
        )
        .outcome,
        CheckOutcome::Passed
    );
    assert_eq!(evaluate(r#"[{"id":"target@market","scope":"user","enabled":true},{"id":"target@market","scope":"project","enabled":true}]"#,true,true).outcome, CheckOutcome::Failed);
    assert_eq!(
        evaluate(
            r#"[{"id":"target@market","scope":"user","enabled":true}]"#,
            false,
            true
        )
        .outcome,
        CheckOutcome::Incomplete
    );
    assert_eq!(
        evaluate(
            r#"[{"id":"target@market","scope":"user","enabled":true}]"#,
            true,
            false
        )
        .outcome,
        CheckOutcome::Failed
    );
    assert_eq!(
        evaluate(
            r#"[{"id":"target@market","scope":"user","enabled":true,"enabled":false}]"#,
            true,
            true
        )
        .outcome,
        CheckOutcome::Failed
    );
}
#[test]
fn rejects_duplicates_anywhere_and_wrong_scope() {
    for json in [
        "",
        "[] true",
        "null",
        r#"[{"id":"target@market","scope":"project","enabled":true}]"#,
        r#"[{"id":"other","x":0,"x":1},{"id":"target@market","scope":"user","enabled":true}]"#,
    ] {
        assert_eq!(evaluate(json, true, true).outcome, CheckOutcome::Failed);
    }
    assert_eq!(
        evaluate(
            r#"[{"id":"target@market","scope":"user","enabled":true},{"id":"other"}]"#,
            true,
            true
        )
        .outcome,
        CheckOutcome::Passed
    );
}
#[test]
fn proof_is_replayed_against_retained_assertions() {
    let proof = evaluate(
        r#"[{"id":"target@market","scope":"user","enabled":true}]"#,
        true,
        true,
    )
    .json_expectation
    .unwrap();
    assert!(proof.verified_pass());
    let mut altered = proof.clone();
    altered.observations.clear();
    assert!(!altered.verified_pass());
    let mut altered = proof.clone();
    altered.observations.push(altered.observations[0].clone());
    assert!(!altered.verified_pass());
    let mut altered = proof;
    if let theseus::recipe::JsonAssertion::ArrayMatch { values, .. } = &mut altered.assertions[0] {
        values[0].equals_json = "\"project\"".into();
    }
    assert!(!altered.verified_pass());
}

fn with_assertions(
    assertions: Vec<theseus::recipe::JsonAssertion>,
    stdout: &str,
    cap: u64,
) -> CheckResult {
    let mut check = check();
    if let theseus::recipe::Expectation::JsonCommand {
        assertions: definitions,
        max_stdout_bytes,
        ..
    } = &mut check.expectation
    {
        *definitions = assertions;
        *max_stdout_bytes = cap;
    }
    evaluate_json_command(
        &check,
        &JsonCommandObservation {
            exit_status: Some(0),
            stdout,
            complete: true,
            utf8: true,
            error: None,
        },
    )
}
fn equals(pointer: &str, literal: &str) -> theseus::recipe::JsonAssertion {
    theseus::recipe::JsonAssertion::Equals {
        pointer: pointer.into(),
        equals_json: literal.into(),
    }
}
#[test]
fn absence_requires_existing_container_parent_and_canonical_array_index() {
    use theseus::recipe::JsonAssertion::Absent;
    for (json, pointer, want) in [
        (r#"{"parent":{}}"#, "/parent/leaf", CheckOutcome::Passed),
        (
            r#"{"parent":{"leaf":null}}"#,
            "/parent/leaf",
            CheckOutcome::Failed,
        ),
        ("{}", "/parent/leaf", CheckOutcome::Failed),
        (r#"{"parent":null}"#, "/parent/leaf", CheckOutcome::Failed),
        ("[]", "/0", CheckOutcome::Passed),
        ("[]", "/00", CheckOutcome::Failed),
        ("[]", "/-", CheckOutcome::Failed),
        ("[]", "/-1", CheckOutcome::Failed),
        (r#"{"00":true}"#, "/00", CheckOutcome::Failed),
    ] {
        assert_eq!(
            with_assertions(
                vec![Absent {
                    pointer: pointer.into()
                }],
                json,
                1024
            )
            .outcome,
            want,
            "{json} {pointer}"
        );
    }
    assert_eq!(
        with_assertions(vec![equals("/00", "true")], r#"{"00":true}"#, 1024).outcome,
        CheckOutcome::Passed
    );
    assert_eq!(
        with_assertions(vec![equals("/00", "true")], "[true]", 1024).outcome,
        CheckOutcome::Failed
    );
}
#[test]
fn scalar_types_pointer_escaping_and_legitimate_replacement_character() {
    for (json, pointer, literal, want) in [
        (
            r#"{"a/b":{"~key":"�"}}"#,
            "/a~1b/~0key",
            "\"�\"",
            CheckOutcome::Passed,
        ),
        ("null", "", "null", CheckOutcome::Passed),
        ("true", "", "true", CheckOutcome::Passed),
        ("1", "", "1", CheckOutcome::Passed),
        ("1.0", "", "1", CheckOutcome::Failed),
        ("9223372036854775808", "", "1", CheckOutcome::Failed),
        ("\"1\"", "", "1", CheckOutcome::Failed),
        ("{}", "", "null", CheckOutcome::Failed),
    ] {
        assert_eq!(
            with_assertions(vec![equals(pointer, literal)], json, 1024).outcome,
            want
        );
    }
}
#[test]
fn depth_and_stdout_caps_have_exact_boundaries() {
    for (depth, want) in [(64, CheckOutcome::Passed), (65, CheckOutcome::Failed)] {
        let document = format!("{}null{}", "[".repeat(depth), "]".repeat(depth));
        let pointer = "/0".repeat(depth);
        let result = with_assertions(vec![equals(&pointer, "null")], &document, 1024);
        assert_eq!(result.outcome, want);
        if depth == 65 {
            assert_eq!(
                result.json_expectation.unwrap().parse_status,
                theseus::model::JsonParseStatus::DepthExceeded
            );
        }
    }
    assert_eq!(
        with_assertions(vec![equals("", "true")], "true", 4).outcome,
        CheckOutcome::Passed
    );
    assert_eq!(
        with_assertions(vec![equals("", "true")], "true ", 4).outcome,
        CheckOutcome::Incomplete
    );
}
#[test]
fn count_zero_removal_and_wrong_version() {
    use theseus::recipe::{JsonAssertion, JsonPredicate};
    let absent = JsonAssertion::ArrayMatch {
        pointer: "".into(),
        identity: vec![JsonPredicate {
            pointer: "/id".into(),
            equals_json: "\"target@market\"".into(),
        }],
        count: 0,
        values: vec![],
    };
    assert_eq!(
        with_assertions(vec![absent.clone()], r#"[{"id":"companion@market"}]"#, 1024).outcome,
        CheckOutcome::Passed
    );
    assert_eq!(
        with_assertions(vec![absent], r#"[{"id":"target@market"}]"#, 1024).outcome,
        CheckOutcome::Failed
    );
    assert_eq!(
        with_assertions(
            vec![equals("/version", "\"2\"")],
            r#"{"version":"1"}"#,
            1024
        )
        .outcome,
        CheckOutcome::Failed
    );
}
#[test]
fn large_actual_scalars_never_enter_retained_proof() {
    let document = serde_json::to_string(&"x".repeat(4000)).unwrap();
    let result = with_assertions(vec![equals("", "\"x\"")], &document, 8192);
    assert_eq!(result.outcome, CheckOutcome::Failed);
    let proof = result.json_expectation.unwrap();
    assert!(serde_json::to_vec(&proof).unwrap().len() < 1000);
    assert!(matches!(
        &proof.observations[0],
        theseus::model::JsonAssertionObservation::Equals {
            actual: theseus::model::JsonValueObservation::OversizedScalar { bytes: 4002, .. }
        }
    ));
}
#[test]
fn missing_exit_adapter_error_and_wrong_exit_have_distinct_outcomes() {
    for (exit, error, complete, want) in [
        (None, None, true, CheckOutcome::Incomplete),
        (Some(0), Some("transport"), true, CheckOutcome::Incomplete),
        (Some(1), None, true, CheckOutcome::Failed),
        (Some(1), None, false, CheckOutcome::Incomplete),
    ] {
        let actual = evaluate_json_command(
            &check(),
            &JsonCommandObservation {
                exit_status: exit,
                stdout: r#"[{"id":"target@market","scope":"user","enabled":true}]"#,
                complete,
                utf8: true,
                error,
            },
        );
        assert_eq!(actual.outcome, want);
    }
}
#[test]
fn proof_rejects_unbounded_metadata_containers_and_contradictory_flags() {
    let good = with_assertions(vec![equals("", "true")], "true", 1024)
        .json_expectation
        .unwrap();
    for edit in 0..7 {
        let mut proof = good.clone();
        match edit {
            0 => proof.complete = false,
            1 => proof.utf8 = false,
            2 => proof.diagnostic = Some("x".repeat(131072)),
            3 => proof.observed_exit = None,
            4 => proof.parse_status = theseus::model::JsonParseStatus::InvalidJson,
            5 => {
                proof.observations = vec![theseus::model::JsonAssertionObservation::Equals {
                    actual: theseus::model::JsonValueObservation::Scalar {
                        value: serde_json::json!({"hidden":"container"}),
                    },
                }]
            }
            _ => {
                proof.observations = vec![theseus::model::JsonAssertionObservation::Equals {
                    actual: theseus::model::JsonValueObservation::NonScalar {
                        kind: "invented".into(),
                    },
                }]
            }
        }
        assert!(!proof.verified_pass(), "edit {edit}");
    }
}
#[test]
fn invalid_utf8_at_original_capture_cap_fails_despite_lossy_expansion() {
    let mut check = check();
    if let theseus::recipe::Expectation::JsonCommand {
        max_stdout_bytes, ..
    } = &mut check.expectation
    {
        *max_stdout_bytes = 1;
    }
    for (complete, want) in [
        (true, CheckOutcome::Failed),
        (false, CheckOutcome::Incomplete),
    ] {
        assert_eq!(
            evaluate_json_command(
                &check,
                &JsonCommandObservation {
                    exit_status: Some(0),
                    stdout: "�",
                    complete,
                    utf8: false,
                    error: None
                }
            )
            .outcome,
            want
        );
    }
}

#[test]
fn diagnostics_are_bounded_at_utf8_boundaries_and_keys_compare_after_decoding() {
    let message = "🧪".repeat(200);
    let result = evaluate_json_command(
        &check(),
        &JsonCommandObservation {
            exit_status: None,
            stdout: "",
            complete: false,
            utf8: false,
            error: Some(&message),
        },
    );
    let proof = result.json_expectation.unwrap();
    assert_eq!(proof.diagnostic.unwrap().len(), 512);
    let result = with_assertions(
        vec![equals("/a", "true")],
        r#"{"a":true,"\u0061":false}"#,
        1024,
    );
    assert_eq!(result.outcome, CheckOutcome::Failed);
    assert_eq!(
        result.json_expectation.unwrap().parse_status,
        theseus::model::JsonParseStatus::DuplicateKey
    );
}
