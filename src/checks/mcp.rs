use super::*;
use crate::{mcp::McpShutdown, runner::ExecutedMcp};

/// Protocol findings and acquisition gaps share the engine's ordinary outcomes.
pub fn evaluate_mcp(check: &Check, observed: &ExecutedMcp) -> CheckResult {
    let command = &observed.command;
    let gap = command.record.cancelled
        || command.record.timed_out
        || command.record.error.is_some()
        || !command.stdout_complete
        || !observed.container_available
        || observed.evidence.shutdown != McpShutdown::Graceful;
    let outcome = if gap {
        CheckOutcome::Incomplete
    } else if command.record.exit_status != Some(0) {
        CheckOutcome::Failed
    } else {
        observed.evidence.protocol_outcome()
    };
    let mut result = result(
        check,
        outcome,
        observed
            .evidence
            .issue
            .as_ref()
            .map(|i| i.message.as_str())
            .unwrap_or(match outcome {
                CheckOutcome::Passed => "MCP identity and complete tool inventory verified",
                CheckOutcome::Failed => "MCP server exited unsuccessfully",
                _ => "MCP acquisition or shutdown incomplete",
            }),
    );
    result.mcp_required = true;
    result.mcp = Some(observed.evidence.clone());
    result
}
