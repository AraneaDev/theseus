//! Replayable consistency proof; this does not authenticate raw command output.
use super::{CheckOutcome, CommandOutcome, StageReport};
use crate::recipe::json::scalar;
use crate::recipe::{JsonAssertion, validate_json_assertions};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonCommandEvidence {
    pub expected_exit: i32,
    pub observed_exit: Option<i32>,
    pub complete: bool,
    pub utf8: bool,
    pub max_stdout_bytes: u64,
    pub parse_status: JsonParseStatus,
    pub diagnostic: Option<String>,
    pub assertions: Vec<JsonAssertion>,
    pub observations: Vec<JsonAssertionObservation>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonParseStatus {
    Parsed,
    InvalidUtf8,
    InvalidJson,
    DuplicateKey,
    DepthExceeded,
    Unavailable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum JsonValueObservation {
    Missing,
    Scalar { value: Value },
    NonScalar { kind: String },
    OversizedScalar { kind: String, bytes: u64 },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JsonAssertionObservation {
    Equals {
        actual: JsonValueObservation,
    },
    Absent {
        parent_valid: bool,
        leaf_exists: bool,
    },
    ArrayMatch {
        array_valid: bool,
        matched_count: u32,
        values: Vec<JsonValueObservation>,
    },
}
impl JsonValueObservation {
    fn valid(&self) -> bool {
        match self {
            Self::Missing => true,
            Self::Scalar { value } => {
                !value.is_array()
                    && !value.is_object()
                    && serde_json::to_string(value).is_ok_and(|s| scalar(&s).is_ok())
            }
            Self::NonScalar { kind } => matches!(
                kind.as_str(),
                "null" | "boolean" | "integer" | "string" | "object" | "array" | "number"
            ),
            Self::OversizedScalar { kind, bytes } => {
                *bytes > 256
                    && matches!(
                        kind.as_str(),
                        "null" | "boolean" | "integer" | "string" | "number"
                    )
            }
        }
    }
    fn equals(&self, expected: &str) -> bool {
        matches!(self, Self::Scalar{value} if scalar(expected).is_ok_and(|v| v == *value))
    }
}
impl JsonCommandEvidence {
    pub(crate) fn valid(&self) -> bool {
        if !(0..=255).contains(&self.expected_exit)
            || validate_json_assertions(&self.assertions, self.max_stdout_bytes).is_err()
            || self.diagnostic.as_ref().is_some_and(|s| s.len() > 512)
            || self.observations.len() > 32
        {
            return false;
        }
        for observation in &self.observations {
            match observation {
                JsonAssertionObservation::Equals { actual } if !actual.valid() => return false,
                JsonAssertionObservation::ArrayMatch {
                    array_valid,
                    matched_count,
                    values,
                } => {
                    if values.len() > 16
                        || (!*array_valid && *matched_count != 0)
                        || (*matched_count != 1 && !values.is_empty())
                        || values.iter().any(|v| !v.valid())
                    {
                        return false;
                    }
                }
                JsonAssertionObservation::Absent {
                    parent_valid: false,
                    leaf_exists: true,
                } => return false,
                _ => {}
            }
        }
        serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= 131072)
    }
    pub fn verified_pass(&self) -> bool {
        self.valid()
            && self.complete
            && self.utf8
            && self.parse_status == JsonParseStatus::Parsed
            && self.diagnostic.is_none()
            && self.observed_exit == Some(self.expected_exit)
            && self.assertions.len() == self.observations.len()
            && self
                .assertions
                .iter()
                .zip(&self.observations)
                .all(|(assertion, observed)| match (assertion, observed) {
                    (
                        JsonAssertion::Equals { equals_json, .. },
                        JsonAssertionObservation::Equals { actual },
                    ) => actual.equals(equals_json),
                    (
                        JsonAssertion::Absent { .. },
                        JsonAssertionObservation::Absent {
                            parent_valid,
                            leaf_exists,
                        },
                    ) => *parent_valid && !leaf_exists,
                    (
                        JsonAssertion::ArrayMatch { count, values, .. },
                        JsonAssertionObservation::ArrayMatch {
                            array_valid,
                            matched_count,
                            values: actual,
                        },
                    ) => {
                        *array_valid
                            && count == matched_count
                            && values.len() == actual.len()
                            && values
                                .iter()
                                .zip(actual)
                                .all(|(p, a)| a.equals(&p.equals_json))
                    }
                    _ => false,
                })
    }
}
impl StageReport {
    pub(crate) fn valid_json_checks(&self) -> bool {
        self.checks.iter().all(|check| {
            if check.json_expectation.is_some() && !check.json_required {
                return false;
            }
            if check
                .json_expectation
                .as_ref()
                .is_some_and(|proof| !proof.valid())
            {
                return false;
            }
            if !check.json_required || check.outcome != CheckOutcome::Passed {
                return true;
            }
            let Some(proof) = &check.json_expectation else {
                return false;
            };
            proof.verified_pass()
                && check.command_ids.len() == 1
                && self
                    .commands
                    .iter()
                    .filter(|command| command.id == check.command_ids[0])
                    .count()
                    == 1
                && self.commands.iter().any(|command| {
                    command.id == check.command_ids[0]
                        && command.phase == format!("check:{}", check.id)
                        && command.outcome == CommandOutcome::Succeeded
                        && command.exit_status == proof.observed_exit
                        && !command.timed_out
                        && !command.cancelled
                        && command.error.is_none()
                        && command.interruption.is_none()
                        && command.expected_failure.is_none()
                        && command.rejection.is_none()
                        && command.stdout.as_ref().is_some_and(|log| {
                            !log.truncated && log.bytes <= proof.max_stdout_bytes
                        })
                })
        })
    }
}
