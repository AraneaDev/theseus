//! Validated text and integer primitives, protocol constants and reply context.
use super::*;

pub(crate) const UINT_MAX: u64 = 9_007_199_254_740_991;
pub(crate) const LEGACY_VERSION: &str = "2025-11-25";
pub(crate) const MODERN_VERSION: &str = "2026-07-28";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ArgumentError;
impl fmt::Display for ArgumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid arguments")
    }
}
impl std::error::Error for ArgumentError {}

text_type!(Instance, Handle, Digest, Revision, Key, RecordId, Cursor);
fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn decimal(value: &str) -> bool {
    !value.is_empty()
        && (value == "0" || !value.starts_with('0'))
        && value.bytes().all(|b| b.is_ascii_digit())
}
fn valid_record(value: &str) -> bool {
    let c: Vec<_> = value.split('/').collect();
    match c.as_slice() {
        ["run" | "native_recipe"] => true,
        ["native", "leaves" | "limits"] => true,
        ["native", "artifact", "base" | "target"] => true,
        ["native", "check", i] => decimal(i),
        ["native", "step", step] => {
            serde_json::from_value::<crate::model::StepName>(Value::String((*step).into())).is_ok()
        }
        [
            "native_identity"
            | "native_disposition"
            | "service"
            | "readiness"
            | "integration_check"
            | "integration_disposition"
            | "desktop",
            i,
        ] => decimal(i),
        ["integration", "requirements"] => true,
        ["integration", "check", i] => decimal(i),
        ["artifact", "base" | "target"] => true,
        ["env" | "observe" | "error" | "artifact", i] | ["s", i] => decimal(i),
        ["artifact", a, "file", b] => decimal(a) && decimal(b),
        ["p", "user" | "system", "check", i] => decimal(i),
        ["p", "user" | "system", "step", step] => {
            serde_json::from_value::<crate::model::StepName>(Value::String((*step).into())).is_ok()
        }
        [
            "p",
            "user" | "system",
            "policy",
            "mcp" | "plugin" | "downgrade" | "rollback" | "interrupted_rollback",
        ] => true,
        ["s", s, "stage" | "finding" | "error", i] => decimal(s) && decimal(i),
        [
            "s",
            s,
            "stage",
            t,
            "command" | "check" | "snapshot" | "baseline" | "change" | "error" | "native_file",
            i,
        ] => decimal(s) && decimal(t) && decimal(i),
        ["s", s, "stage", t, "snapshot", i, "entry", j] => {
            decimal(s) && decimal(t) && decimal(i) && decimal(j)
        }
        _ => false,
    }
}
fn valid_text(kind: &str, value: &str) -> bool {
    match kind {
        "Instance" => hex(value, 32),
        "Digest" | "Revision" => hex(value, 64),
        "Handle" => value.split_once(':').is_some_and(|(i, s)| {
            hex(i, 32) && decimal(s) && s.parse::<u8>().is_ok_and(|n| (1..=128).contains(&n))
        }),
        "Key" => !value.is_empty() && value.len() <= 128 && !value.contains('\0'),
        "RecordId" => value.len() <= 128 && value.is_ascii() && valid_record(value),
        "Cursor" => value.len() <= 256 && value.is_ascii(),
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub(crate) struct UInt(u64);
impl UInt {
    pub fn new(value: u64) -> Result<Self, ArgumentError> {
        if value <= UINT_MAX {
            Ok(Self(value))
        } else {
            Err(ArgumentError)
        }
    }
    pub fn get(self) -> u64 {
        self.0
    }
}
impl TryFrom<u64> for UInt {
    type Error = ArgumentError;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<UInt> for u64 {
    fn from(value: UInt) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum RpcId {
    String(String),
    Integer(i64),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Era {
    Legacy,
    Modern,
}
#[derive(Debug, Clone)]
pub(crate) struct ReplyContext {
    pub era: Era,
    pub id: RpcId,
    pub instance: Instance,
}

pub(super) fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

pub(super) fn optional_non_null<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}
pub(super) fn default_log_limit() -> UInt {
    UInt(16_777_216)
}
