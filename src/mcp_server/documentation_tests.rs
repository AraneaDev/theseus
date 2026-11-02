//! README transcripts go through the actual strict wire/domain parser.
use super::{types::*, wire::WireState};

#[test]
fn readme_mcp_examples_parse_through_production_wire() {
    let readme = include_str!("../../README.md").replace("\r\n", "\n");
    let mut modern_tools = Vec::new();
    for era in ["legacy", "modern"] {
        let marker = format!("<!-- mcp-example:{era} -->");
        let section = readme.split_once(&marker).expect("example marker").1;
        let block = section.split_once("```json\n").expect("JSON fence").1;
        let block = block.split_once("\n```").expect("closed JSON fence").0;
        let mut wire = WireState::default();
        for line in block.lines() {
            match wire.decode(line.as_bytes()) {
                WireEvent::Request(Request { method, .. }) => {
                    if era == "modern" {
                        let name = match method {
                            Method::ToolCall(ToolCall::RecipeInspect(_)) => "recipe_inspect",
                            Method::ToolCall(ToolCall::RunStart(_)) => "run_start",
                            Method::ToolCall(ToolCall::RunList(_)) => "run_list",
                            Method::ToolCall(ToolCall::RunStatus(_)) => "run_status",
                            Method::ToolCall(ToolCall::RunCancel(_)) => "run_cancel",
                            Method::ToolCall(ToolCall::RunEvidence(_)) => "run_evidence",
                            other => panic!("unexpected modern example: {other:?}"),
                        };
                        modern_tools.push(name);
                    }
                }
                WireEvent::Notification(Notification::Initialized) if era == "legacy" => (),
                event => panic!("README {era} example rejected: {event:?}"),
            }
        }
    }
    assert_eq!(
        modern_tools,
        [
            "recipe_inspect",
            "run_start",
            "run_list",
            "run_status",
            "run_cancel",
            "run_evidence"
        ]
    );
}
