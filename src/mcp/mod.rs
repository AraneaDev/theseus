//! Bounded pure stdio discovery and retained response consistency checks.
mod evidence;
mod protocol;
use anyhow::{Result, ensure};
pub use protocol::McpSession;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const FRAME_LIMIT: usize = 65536;
pub(crate) const STDOUT_LIMIT: usize = 262144;
pub(crate) const FRAME_COUNT: u32 = 256;
pub(crate) const PAGE_LIMIT: usize = 8;
pub(crate) const TOOL_LIMIT: usize = 128;
pub(crate) const OUTGOING_LIMIT: usize = 16384;
pub(crate) const EVIDENCE_LIMIT: usize = 1048576;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum McpProtocol {
    #[serde(rename = "2026-07-28")]
    Modern20260728,
    #[serde(rename = "2025-11-25")]
    Legacy20251125,
}
impl McpProtocol {
    pub fn version(self) -> &'static str {
        match self {
            Self::Modern20260728 => "2026-07-28",
            Self::Legacy20251125 => "2025-11-25",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpProbe {
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    pub protocol: McpProtocol,
    pub server_name: String,
    pub server_version: String,
    pub tools: Vec<String>,
}
fn default_timeout() -> u64 {
    15
}
impl McpProbe {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=64).contains(&self.argv.len()),
            "MCP argv must have 1..=64 entries"
        );
        ensure!(
            (3..=120).contains(&self.timeout_seconds),
            "MCP timeout must be 3..=120 seconds"
        );
        ensure!(
            self.argv.iter().all(|s| !s.contains('\0')),
            "MCP argv contains NUL"
        );
        let executable = &self.argv[0];
        ensure!(
            executable != "/"
                && crate::recipe::normalize_container_path(executable)? == *executable,
            "MCP executable must be a normalized absolute container path"
        );
        ensure!(
            self.env
                .iter()
                .all(|(k, v)| crate::recipe::validate::valid_env_name(k) && !v.contains('\0')),
            "invalid MCP environment"
        );
        let bytes = self
            .argv
            .iter()
            .map(String::len)
            .chain(self.env.iter().flat_map(|(k, v)| [k.len(), v.len()]))
            .try_fold(0usize, usize::checked_add);
        ensure!(
            bytes.is_some_and(|n| n <= OUTGOING_LIMIT),
            "MCP argv/environment budget exceeded"
        );
        ensure!(
            bounded_text(&self.server_name, 256) && bounded_text(&self.server_version, 256),
            "invalid MCP server identity"
        );
        ensure!(
            self.tools.len() <= TOOL_LIMIT && self.tools.iter().all(|s| bounded_text(s, 128)),
            "invalid MCP expected tools"
        );
        ensure!(
            self.tools.iter().collect::<BTreeSet<_>>().len() == self.tools.len(),
            "duplicate MCP expected tool"
        );
        Ok(())
    }
}
pub(crate) fn bounded_text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.contains('\0')
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    String(String),
    Integer(i64),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpIssueKind {
    Protocol,
    Inventory,
    Limit,
    Transport,
    Shutdown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpIssue {
    pub kind: McpIssueKind,
    pub message: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpShutdown {
    NotStarted,
    Graceful,
    ForcedOwnedRemoval,
    Unverified,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpEvidence {
    pub probe: McpProbe,
    pub sent_ids: Vec<RpcId>,
    pub response_lines: Vec<String>,
    pub notification_count: u32,
    pub stdout_bytes: u64,
    pub stdout_complete: bool,
    pub protocol_complete: bool,
    pub issue: Option<McpIssue>,
    pub shutdown: McpShutdown,
}
