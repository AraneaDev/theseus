use super::CommandObservation;
use crate::model::CheckOutcome;

/// Common complete, nonzero-status and literal-marker classification. Callers
/// provide phase-appropriate static messages and never forward observed values.
pub(super) fn assess(
    expected_exit: i32,
    marker: &str,
    observed: &CommandObservation,
    messages: [&'static str; 5],
) -> (CheckOutcome, String) {
    let (outcome, index) = if !(1..=125).contains(&expected_exit) || marker.trim().is_empty() {
        (CheckOutcome::Incomplete, 0)
    } else if !observed.complete || observed.error.is_some() || observed.exit_status.is_none() {
        (CheckOutcome::Incomplete, 1)
    } else if observed.exit_status != Some(expected_exit) {
        (CheckOutcome::Failed, 2)
    } else if !observed.stdout.contains(marker) {
        (CheckOutcome::Failed, 3)
    } else {
        (CheckOutcome::Passed, 4)
    };
    (outcome, messages[index].into())
}
