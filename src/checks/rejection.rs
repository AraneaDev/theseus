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
    let (outcome, message) =
        if !(1..=125).contains(&expected_exit) || stdout_contains.trim().is_empty() {
            (
                CheckOutcome::Incomplete,
                "invalid declared refusal status or stdout marker",
            )
        } else if !observed.complete || observed.error.is_some() || observed.exit_status.is_none() {
            (CheckOutcome::Incomplete, "refusal evidence incomplete")
        } else if observed.exit_status != Some(expected_exit) {
            (
                CheckOutcome::Failed,
                "exit status differs from declared refusal",
            )
        } else if !observed.stdout.contains(stdout_contains) {
            (
                CheckOutcome::Failed,
                "literal stdout marker for declared refusal is absent",
            )
        } else {
            (
                CheckOutcome::Passed,
                "declared refusal status and literal stdout marker matched",
            )
        };
    // Static summaries are valid UTF-8, below 1024 bytes, and never copy output,
    // adapter errors or potentially forwarded values into report summaries.
    RejectionAssessment {
        outcome,
        message: message.into(),
    }
}
