//! Pure expectation evaluation. Runtime adapters execute commands and provide evidence.
mod backup;
mod command;
mod files;
mod lookup;
mod rejection;
mod stability;
use crate::{
    model::{CheckOutcome, CheckResult},
    recipe::Check,
};
pub use backup::{compare_changed, compare_mapped};
pub use command::{CommandObservation, evaluate_command, evaluate_executable};
pub use files::{evaluate_filesystem, review_findings};
pub use rejection::{RejectionAssessment, assess_rejection};
pub use stability::{StabilityResult, compare_unchanged};
fn result(check: &Check, outcome: CheckOutcome, message: impl Into<String>) -> CheckResult {
    CheckResult {
        id: check.id.clone(),
        checkpoint: check.checkpoint,
        outcome,
        message: message.into(),
        path: check.expectation.path().map(str::to_owned),
        command_ids: Vec::new(),
        comparison: None,
    }
}
