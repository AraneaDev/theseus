//! Strict complete-document parsing and finite typed observations.
use super::result;
use crate::recipe::json::{array_index, lookup, pointer_tokens, scalar};
use crate::strict_json::parse;
use crate::{
    model::{
        CheckOutcome, CheckResult, JsonAssertionObservation as Observation, JsonCommandEvidence,
        JsonParseStatus, JsonValueObservation,
    },
    recipe::{Check, Expectation, JsonAssertion, JsonPredicate, validate_json_assertions},
};
use serde_json::Value;

/// Adapters attest that a complete stream was captured within its requested byte bound.
/// `utf8` describes original bytes, before any lossy conversion to `stdout`.
pub struct JsonCommandObservation<'a> {
    pub exit_status: Option<i32>,
    pub stdout: &'a str,
    pub complete: bool,
    pub utf8: bool,
    pub error: Option<&'a str>,
}
fn bounded_diagnostic(text: &str) -> String {
    let mut end = text.len().min(512);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].into()
}
fn select<'a>(value: &'a Value, pointer: &str) -> Option<&'a Value> {
    lookup(value, &pointer_tokens(pointer).ok()?)
}
fn value_observation(value: Option<&Value>) -> JsonValueObservation {
    let Some(value) = value else {
        return JsonValueObservation::Missing;
    };
    let kind = match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Number(n) if n.as_i64().is_some() => "integer",
        Value::Number(_) => "number",
    };
    if matches!(kind, "array" | "object" | "number") {
        return JsonValueObservation::NonScalar { kind: kind.into() };
    }
    let bytes = serde_json::to_vec(value)
        .expect("JSON value serialization")
        .len() as u64;
    if bytes > 256 {
        JsonValueObservation::OversizedScalar {
            kind: kind.into(),
            bytes,
        }
    } else {
        JsonValueObservation::Scalar {
            value: value.clone(),
        }
    }
}
fn matches(value: &Value, predicate: &JsonPredicate) -> bool {
    select(value, &predicate.pointer)
        .zip(scalar(&predicate.equals_json).ok())
        .is_some_and(|(actual, expected)| actual == &expected)
}
fn observe(value: &Value, assertion: &JsonAssertion) -> Observation {
    match assertion {
        JsonAssertion::Equals { pointer, .. } => Observation::Equals {
            actual: value_observation(select(value, pointer)),
        },
        JsonAssertion::Absent { pointer } => {
            let tokens = pointer_tokens(pointer).expect("validated pointer");
            let (leaf, parent) = tokens.split_last().expect("nonempty absence pointer");
            let (parent_valid, leaf_exists) = match lookup(value, parent) {
                Some(Value::Object(map)) => (true, map.contains_key(leaf)),
                Some(Value::Array(array)) => {
                    array_index(leaf).map_or((false, false), |index| (true, index < array.len()))
                }
                _ => (false, false),
            };
            Observation::Absent {
                parent_valid,
                leaf_exists,
            }
        }
        JsonAssertion::ArrayMatch {
            pointer,
            identity,
            values,
            ..
        } => {
            let Some(Value::Array(array)) = select(value, pointer) else {
                return Observation::ArrayMatch {
                    array_valid: false,
                    matched_count: 0,
                    values: vec![],
                };
            };
            let mut selected = None;
            let mut matched_count = 0;
            for element in array {
                if identity.iter().all(|p| matches(element, p)) {
                    matched_count += 1;
                    selected = Some(element);
                }
            }
            let values = if matched_count == 1 {
                values
                    .iter()
                    .map(|p| value_observation(select(selected.unwrap(), &p.pointer)))
                    .collect()
            } else {
                vec![]
            };
            Observation::ArrayMatch {
                array_valid: true,
                matched_count,
                values,
            }
        }
    }
}
pub fn evaluate_json_command(check: &Check, observed: &JsonCommandObservation<'_>) -> CheckResult {
    let Expectation::JsonCommand {
        expected_exit,
        max_stdout_bytes,
        assertions,
        ..
    } = &check.expectation
    else {
        return result(
            check,
            CheckOutcome::Incomplete,
            "JSON command expectation required",
        );
    };
    let mut proof = JsonCommandEvidence {
        expected_exit: *expected_exit,
        observed_exit: observed.exit_status,
        complete: observed.complete,
        utf8: observed.utf8,
        max_stdout_bytes: *max_stdout_bytes,
        parse_status: JsonParseStatus::Unavailable,
        diagnostic: None,
        assertions: assertions.clone(),
        observations: vec![],
    };
    let outcome = if !observed.complete
        || observed.error.is_some()
        || observed.exit_status.is_none()
        // Lossy replacement can expand invalid bytes; completeness attests the raw bound.
        || (observed.utf8 && observed.stdout.len() as u64 > *max_stdout_bytes)
        || validate_json_assertions(assertions, *max_stdout_bytes).is_err()
    {
        proof.complete = false;
        proof.diagnostic =
            Some(bounded_diagnostic(observed.error.unwrap_or(
                "JSON command evidence incomplete or invalid definition",
            )));
        CheckOutcome::Incomplete
    } else if !observed.utf8 {
        proof.parse_status = JsonParseStatus::InvalidUtf8;
        proof.diagnostic = Some("stdout is not valid UTF-8".into());
        CheckOutcome::Failed
    } else {
        match parse(observed.stdout) {
            Ok(value) => {
                proof.parse_status = JsonParseStatus::Parsed;
                proof.observations = assertions.iter().map(|a| observe(&value, a)).collect();
                if proof.verified_pass() {
                    CheckOutcome::Passed
                } else {
                    CheckOutcome::Failed
                }
            }
            Err(error) => {
                let diagnostic = error.to_string();
                proof.parse_status = if diagnostic.starts_with("duplicate JSON key") {
                    JsonParseStatus::DuplicateKey
                } else if diagnostic.starts_with("JSON depth exceeded") {
                    JsonParseStatus::DepthExceeded
                } else {
                    JsonParseStatus::InvalidJson
                };
                proof.diagnostic = Some(bounded_diagnostic(&diagnostic));
                CheckOutcome::Failed
            }
        }
    };
    let mut result = result(check, outcome, "bounded JSON command expectation");
    if serde_json::to_vec(&proof).is_ok_and(|bytes| bytes.len() <= 131072) {
        result.json_expectation = Some(proof);
    } else {
        result.outcome = CheckOutcome::Incomplete;
        result.message = "JSON evidence budget exceeded".into();
    }
    result
}
