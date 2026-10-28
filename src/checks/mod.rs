//! Pure expectation evaluation. Runtime adapters execute commands and provide evidence.
mod command;
mod files;
use crate::{
    model::{CheckOutcome, CheckResult},
    recipe::Check,
};
pub use command::{CommandObservation, evaluate_command, evaluate_executable};
pub use files::{evaluate_filesystem, review_findings};
fn result(check: &Check, outcome: CheckOutcome, message: impl Into<String>) -> CheckResult {
    CheckResult {
        id: check.id.clone(),
        checkpoint: check.checkpoint,
        outcome,
        message: message.into(),
        path: check.expectation.path().map(str::to_owned),
        command_ids: Vec::new(),
    }
}
