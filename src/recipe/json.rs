//! Finite, bounded JSON predicates shared by recipes and retained proof.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JsonAssertion {
    Equals {
        pointer: String,
        equals_json: String,
    },
    Absent {
        pointer: String,
    },
    ArrayMatch {
        pointer: String,
        identity: Vec<JsonPredicate>,
        count: u32,
        values: Vec<JsonPredicate>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonPredicate {
    pub pointer: String,
    pub equals_json: String,
}

pub(crate) fn scalar(literal: &str) -> Result<Value> {
    ensure!(
        literal.len() <= 16384,
        "JSON scalar literal exceeds definition budget"
    );
    let value: Value = serde_json::from_str(literal)?;
    ensure!(
        matches!(value, Value::Null | Value::Bool(_) | Value::String(_))
            || value.as_i64().is_some(),
        "expected null, boolean, signed integer or string"
    );
    ensure!(
        serde_json::to_vec(&value)?.len() <= 256,
        "serialized JSON scalar exceeds 256 bytes"
    );
    Ok(value)
}
pub(crate) fn pointer_tokens(pointer: &str) -> Result<Vec<String>> {
    ensure!(pointer.len() <= 256, "JSON pointer exceeds 256 bytes");
    if pointer.is_empty() {
        return Ok(vec![]);
    }
    ensure!(pointer.starts_with('/'), "JSON pointer must start with /");
    pointer[1..]
        .split('/')
        .map(|part| {
            ensure!(
                !part.contains('*'),
                "JSON pointer wildcards are unsupported"
            );
            let mut decoded = String::new();
            let mut chars = part.chars();
            while let Some(c) = chars.next() {
                decoded.push(if c == '~' {
                    match chars.next() {
                        Some('0') => '~',
                        Some('1') => '/',
                        _ => anyhow::bail!("invalid JSON pointer escape"),
                    }
                } else {
                    c
                });
            }
            Ok(decoded)
        })
        .collect()
}
pub(crate) fn array_index(token: &str) -> Option<usize> {
    if token.is_empty()
        || (token.len() > 1 && token.starts_with('0'))
        || !token.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    token.parse().ok()
}
pub(crate) fn lookup<'a>(value: &'a Value, tokens: &[String]) -> Option<&'a Value> {
    let mut current = value;
    for token in tokens {
        current = match current {
            Value::Object(map) => map.get(token)?,
            Value::Array(array) => array.get(array_index(token)?)?,
            _ => return None,
        };
    }
    Some(current)
}
fn predicates(predicates: &[JsonPredicate]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for p in predicates {
        let tokens = pointer_tokens(&p.pointer)?;
        ensure!(seen.insert(tokens), "duplicate JSON predicate");
        scalar(&p.equals_json)?;
    }
    Ok(())
}
pub fn validate_json_assertions(assertions: &[JsonAssertion], cap: u64) -> Result<()> {
    ensure!(
        (1..=1_048_576).contains(&cap),
        "JSON stdout bound must be 1..=1048576"
    );
    ensure!(
        (1..=32).contains(&assertions.len()),
        "JSON assertions must contain 1..=32 entries"
    );
    let mut leaves = 0;
    for assertion in assertions {
        match assertion {
            JsonAssertion::Equals {
                pointer,
                equals_json,
            } => {
                pointer_tokens(pointer)?;
                scalar(equals_json)?;
                leaves += 1;
            }
            JsonAssertion::Absent { pointer } => {
                ensure!(!pointer.is_empty(), "absent requires a parent");
                pointer_tokens(pointer)?;
            }
            JsonAssertion::ArrayMatch {
                pointer,
                identity,
                count,
                values,
            } => {
                pointer_tokens(pointer)?;
                ensure!(
                    (1..=4).contains(&identity.len()),
                    "identity requires 1..=4 predicates"
                );
                ensure!(
                    (*count == 0 && values.is_empty())
                        || (*count == 1 && (1..=16).contains(&values.len())),
                    "array count must be 0 without values or 1 with 1..=16 values"
                );
                predicates(identity)?;
                predicates(values)?;
                let mut seen = BTreeSet::new();
                for predicate in identity.iter().chain(values) {
                    ensure!(
                        seen.insert(pointer_tokens(&predicate.pointer)?),
                        "duplicate JSON predicate"
                    );
                }
                leaves += identity.len() + values.len();
            }
        }
    }
    ensure!(leaves <= 64, "JSON leaf predicate budget exceeded");
    ensure!(
        serde_json::to_vec(assertions)?.len() <= 16384,
        "JSON assertion definition budget exceeded"
    );
    Ok(())
}
