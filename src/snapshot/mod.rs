//! Bounded, no-extraction observations. Paths are absolute container paths.
mod archive;
mod diff;
use crate::model::FileKind;
pub use crate::recipe::Limits;
pub use archive::read_archive;
pub use diff::diff;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ObservationStatus {
    Complete,
    Missing,
    Incomplete { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub root: String,
    pub completeness: ObservationStatus,
    pub entries: BTreeMap<String, FileEntry>,
}
impl Snapshot {
    pub fn missing(root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            completeness: ObservationStatus::Missing,
            entries: BTreeMap::new(),
        }
    }
    pub fn incomplete(root: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            completeness: ObservationStatus::Incomplete {
                reason: reason.into(),
            },
            entries: BTreeMap::new(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub kind: FileKind,
    pub mode: u32,
    pub uid: u64,
    pub gid: u64,
    pub size: u64,
    pub sha256: Option<String>,
    pub link_target: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub kind: ChangeKind,
    pub before: Option<FileEntry>,
    pub after: Option<FileEntry>,
}
