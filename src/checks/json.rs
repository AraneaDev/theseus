//! Strict complete-document parsing and finite typed observations.
use super::result;
use crate::recipe::json::{array_index, lookup, pointer_tokens, scalar};
use crate::{
    model::{
        CheckOutcome, CheckResult, JsonAssertionObservation as Observation, JsonCommandEvidence,
        JsonParseStatus, JsonValueObservation,
    },
    recipe::{Check, Expectation, JsonAssertion, JsonPredicate, validate_json_assertions},
};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::{collections::BTreeSet, fmt};

/// Adapters attest that a complete stream was captured within its requested byte bound.
/// `utf8` describes original bytes, before any lossy conversion to `stdout`.
pub struct JsonCommandObservation<'a> {
    pub exit_status: Option<i32>,
    pub stdout: &'a str,
    pub complete: bool,
    pub utf8: bool,
    pub error: Option<&'a str>,
}
struct Strict {
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for Strict {
    type Value = Value;
    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Strict {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a bounded JSON document")
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("invalid JSON number"))
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.into()))
    }
    fn visit_string<E: de::Error>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        if self.depth >= 64 {
            return Err(de::Error::custom("JSON depth exceeded"));
        }
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(Strict {
            depth: self.depth + 1,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        if self.depth >= 64 {
            return Err(de::Error::custom("JSON depth exceeded"));
        }
        let mut keys = BTreeSet::new();
        let mut values = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(de::Error::custom("duplicate JSON key"));
            }
            let value = map.next_value_seed(Strict {
                depth: self.depth + 1,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}
fn parse(text: &str) -> Result<Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let value = Strict { depth: 0 }.deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
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
