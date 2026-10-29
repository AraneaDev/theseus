use super::CommandObservation;
use crate::model::CheckOutcome;

/// Attempt classification only; passing does not establish binary/data safety.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectionAssessment {
    pub outcome: CheckOutcome,
    pub message: String,
}

/// Require complete evidence before comparing the exact declared status and
/// literal stdout marker. Adapters mark truncated/timeout/cancelled evidence
/// incomplete; matching partial text never overrides that signal.
pub fn assess_rejection(
    expected_exit: i32,
    stdout_contains: &str,
    observed: &CommandObservation,
) -> RejectionAssessment {
    let (outcome, message) = super::completion::assess(
        expected_exit,
        stdout_contains,
        observed,
        [
            "invalid declared refusal status or stdout marker",
            "refusal evidence incomplete",
            "exit status differs from declared refusal",
            "literal stdout marker for declared refusal is absent",
            "declared refusal status and literal stdout marker matched",
        ],
    );
    RejectionAssessment { outcome, message }
}
