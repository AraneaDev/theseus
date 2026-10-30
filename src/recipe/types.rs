use crate::model::{Checkpoint, FileKind, ProfileName, ScenarioKind, StepName};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const DEFAULT_TIMEOUT_SECONDS: u64 = 120;
pub const MAX_TIMEOUT_SECONDS: u64 = 3600;
pub const DEFAULT_LOG_LIMIT: u64 = 16_777_216;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Network {
    #[default]
    None,
    Bridge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default = "default_entries")]
    pub max_entries: u64,
    #[serde(default = "default_content")]
    pub max_content_bytes: u64,
}
const fn default_entries() -> u64 {
    100_000
}
const fn default_content() -> u64 {
    1_073_741_824
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: default_entries(),
            max_content_bytes: default_content(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenarios: Option<Vec<ScenarioKind>>,
    pub name: String,
    pub image: String,
    #[serde(default)]
    pub network: Network,
    pub artifacts: Artifacts,
    pub profiles: BTreeMap<ProfileName, Profile>,
    pub observe: Vec<String>,
    #[serde(default)]
    pub limits: Limits,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifacts {
    pub base: Option<PathBuf>,
    pub target: PathBuf,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<super::PluginContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback: Option<super::RollbackContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_rollback: Option<super::InterruptedRollbackContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downgrade: Option<super::DowngradeContract>,
    pub steps: BTreeMap<StepName, CommandSpec>,
    #[serde(default)]
    pub checks: Vec<Check>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub script: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub pass_env: Vec<String>,
}
const fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECONDS
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub id: String,
    pub checkpoint: Checkpoint,
    #[serde(flatten)]
    pub expectation: Expectation,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Expectation {
    JsonCommand {
        script: String,
        #[serde(default = "default_timeout")]
        timeout_seconds: u64,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        pass_env: Vec<String>,
        expected_exit: i32,
        #[serde(default = "default_json_limit")]
        max_stdout_bytes: u64,
        assertions: Vec<super::JsonAssertion>,
    },
    Command {
        script: String,
        #[serde(default = "default_timeout")]
        timeout_seconds: u64,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        pass_env: Vec<String>,
        expected_exit: i32,
        stdout_contains: Option<String>,
    },
    Executable {
        name: String,
        resolved_path: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default = "default_timeout")]
        timeout_seconds: u64,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        pass_env: Vec<String>,
        expected_exit: i32,
        stdout_contains: Option<String>,
    },
    Absent {
        path: String,
    },
    Present {
        path: String,
        expected_kind: Option<FileKind>,
    },
    Preserved {
        path: String,
    },
    Unchanged {
        path: String,
    },
    Changed {
        path: String,
    },
}
impl Expectation {
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Absent { path }
            | Self::Present { path, .. }
            | Self::Preserved { path }
            | Self::Unchanged { path }
            | Self::Changed { path } => Some(path),
            _ => None,
        }
    }
}

const fn default_json_limit() -> u64 {
    262144
}
