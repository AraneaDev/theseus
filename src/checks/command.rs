use super::result;
use crate::{
    model::{CheckOutcome, CheckResult},
    recipe::{Check, Expectation},
};
use serde::{Deserialize, Serialize};
/// Adapter-supplied output, with completeness false for timeout, cancellation,
/// truncated output that cannot establish a substring, or an execution error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandObservation {
    pub exit_status: Option<i32>,
    pub stdout: String,
    pub complete: bool,
    pub error: Option<String>,
}
pub fn evaluate_command(check: &Check, observed: &CommandObservation) -> CheckResult {
    match &check.expectation {
        Expectation::Command {
            expected_exit,
            stdout_contains,
            ..
        } => evaluate(check, observed, *expected_exit, stdout_contains.as_deref()),
        _ => result(
            check,
            CheckOutcome::Incomplete,
            "command expectation required",
        ),
    }
}
pub fn evaluate_executable(
    check: &Check,
    resolved: Option<&str>,
    observed: &CommandObservation,
) -> CheckResult {
    let Expectation::Executable {
        resolved_path,
        expected_exit,
        stdout_contains,
        ..
    } = &check.expectation
    else {
        return result(
            check,
            CheckOutcome::Incomplete,
            "executable expectation required",
        );
    };
    if !observed.complete || observed.error.is_some() {
        return result(
            check,
            CheckOutcome::Incomplete,
            observed
                .error
                .as_deref()
                .unwrap_or("command evidence incomplete"),
        );
    }
    if resolved != Some(resolved_path.as_str()) {
        return result(
            check,
            CheckOutcome::Failed,
            format!("resolved path {resolved:?} differs from {resolved_path}"),
        );
    }
    evaluate(check, observed, *expected_exit, stdout_contains.as_deref())
}
fn evaluate(
    check: &Check,
    observed: &CommandObservation,
    expected_exit: i32,
    contains: Option<&str>,
) -> CheckResult {
    if !observed.complete || observed.error.is_some() || observed.exit_status.is_none() {
        return result(
            check,
            CheckOutcome::Incomplete,
            observed
                .error
                .as_deref()
                .unwrap_or("command evidence incomplete"),
        );
    }
    let passed = observed.exit_status == Some(expected_exit)
        && contains.is_none_or(|literal| observed.stdout.contains(literal));
    result(
        check,
        if passed {
            CheckOutcome::Passed
        } else {
            CheckOutcome::Failed
        },
        if passed {
            "command expectation satisfied"
        } else {
            "exit status or literal stdout expectation failed"
        },
    )
}
