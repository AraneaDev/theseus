//! Typed recipes and offline validation. Environment forwarding is a run-preflight concern.
mod types;
pub(crate) mod validate;
pub use types::*;
pub use validate::{ValidatedRecipe, load, normalize_container_path};
