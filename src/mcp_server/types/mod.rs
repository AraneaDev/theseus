//! Bounded public projections and private ownership-preserving worker handoffs.
use crate::model::{ProfileName, ResultCounts, RunOutcome, ScenarioKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, path::PathBuf};
use theseus_host::fs::RelativePath;

macro_rules! text_type {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub(crate) struct $name(String);
        impl $name {
            pub fn parse(value: &str) -> Result<Self, ArgumentError> {
                if valid_text(stringify!($name), value) { Ok(Self(value.into())) } else { Err(ArgumentError) }
            }
            #[allow(dead_code, reason = "Validated text tokens share a uniform borrowing accessor.")]
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl TryFrom<String> for $name {
            type Error = ArgumentError;
            fn try_from(value: String) -> Result<Self, Self::Error> { Self::parse(&value) }
        }
        impl From<$name> for String { fn from(value: $name) -> Self { value.0 } }
    )+};
}

macro_rules! named_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub(crate) enum $name { $($variant),+ }
    };
}

mod args;
mod data;
mod errors;
mod primitives;
mod validate;
mod wire;

pub(crate) use args::*;
pub(crate) use data::*;
pub(crate) use errors::*;
pub(crate) use primitives::*;
pub(crate) use wire::*;
