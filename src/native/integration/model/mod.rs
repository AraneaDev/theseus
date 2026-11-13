//! Strict schema2 evidence; serialized identities never reconstruct native owners.
mod broker_progress;
mod desktop_generation;
mod journal;
mod report;
mod requirements;
mod serde_util;
mod validate;
mod vocab;

pub use broker_progress::*;
pub use desktop_generation::*;
pub use journal::*;
pub use report::*;
pub use requirements::*;
pub use validate::*;
pub use vocab::*;

// The test modules historically saw the parent's imports through `use super::*`.
#[cfg(test)]
use crate::model::{RunOutcome, ScenarioKind};
#[cfg(test)]
use crate::native::model::*;

#[cfg(test)]
mod catalog_review_tests;
#[cfg(test)]
mod desktop_generation_presence_tests;
#[cfg(test)]
mod desktop_generation_sut_tests;
#[cfg(test)]
mod desktop_ordinary_contract_tests;
#[cfg(test)]
mod native_desktop_mode_tests;
#[cfg(test)]
mod owned_desktop_mode_tests;
#[cfg(test)]
mod tests;
