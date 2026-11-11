//! Explicit schema2 native integration contracts.
pub mod model;
pub mod recipe;
pub use model::*;
pub use recipe::{NativeIntegrationRecipe, ValidatedIntegrationRecipe};
pub const PROFILE: &str = "native_integration_v1";

pub mod view;

pub mod driver;

pub mod desktop;

mod owned_lifetime;
