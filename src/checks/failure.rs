use super::{CommandObservation, completion};
use crate::{model::CheckOutcome, recipe::ExpectedFailureContract};

/// The attempt's failure classification alone does not establish safe rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureAssessment {
    pub outcome: CheckOutcome,
    pub message: String,
}
pub fn assess_expected_failure(
    expected: &ExpectedFailureContract,
    observed: &CommandObservation,
) -> FailureAssessment {
    let (outcome, message) = completion::assess(
        expected.expected_exit,
        &expected.stdout_contains,
        observed,
        [
            "invalid declared failure status or stdout marker",
            "failure evidence incomplete",
            "exit status differs from declared failure",
            "literal stdout marker for declared failure is absent",
            "declared failure status and literal stdout marker matched",
        ],
    );
    FailureAssessment { outcome, message }
}
