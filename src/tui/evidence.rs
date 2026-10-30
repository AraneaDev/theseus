//! Borrowed typed records. Catalogs never execute actions or change captured evidence.
mod catalog;
mod filter;
mod format;
use super::paging::{self, TextPage};
use crate::{comparison::*, model::*, snapshot::*};
pub use catalog::Catalog;
pub use filter::FilterIndex;
use std::{fmt, path::Path};
#[derive(Debug, Clone, Copy)]
pub enum Record<'a> {
    TerminalError(&'a str),
    Text(&'a str),
    Skipped(&'a str),
    Check(&'a CheckResult),
    Stage(&'a StageReport),
    Restart(&'a RestartEvidence),
    ExpectedFailure(&'a ExpectedFailureEvidence),
    DeclaredRefusal(&'a CommandRecord, &'a RejectionEvidence),
    Interruption(&'a InterruptionEvidence),
    Barrier(&'a CommandRecord),
    Finding(&'a Finding),
    Teardown(&'a TeardownStatus),
    Observation(&'a Snapshot, bool),
    IncompleteObservation(&'a Snapshot),
    Entry(&'a str, &'a FileEntry),
    Change(&'static str, &'a FileChange),
    Command(&'a CommandRecord),
    Log(&'a CommandRecord, &'static str, &'a LogReference),
    Cell(&'a ComparisonCell, &'a SuiteManifest),
    Diagnostic(&'a str),
    Source(&'a ComparisonSource),
    Artifact(&'a ArtifactDigest),
    Plugin(&'a crate::recipe::PluginContract),
    SourcePath(&'a Path, &'a str),
    Cleanup(&'a Path),
    Json(&'a Path),
}
#[derive(Debug, Clone, Copy)]
pub struct SelectedRecord<'a> {
    pub key: usize,
    pub record: Record<'a>,
}
#[derive(Debug, Clone)]
pub struct Summary {
    pub key: usize,
    pub text: String,
}
impl<'a> Record<'a> {
    pub fn log_reference(self) -> Option<&'a LogReference> {
        if let Self::Log(_, _, r) = self {
            Some(r)
        } else {
            None
        }
    }
    pub fn page(self, offset: usize, limit: usize) -> anyhow::Result<TextPage> {
        paging::page(|out| self.write(out), offset, limit)
    }
    /// Compatibility API only: callers rendering a viewport should use page().
    pub fn text(self) -> String {
        let mut sink = paging::SafeString(String::new());
        let _ = self.write(&mut sink);
        sink.0
    }
    pub fn summary(self, limit: usize) -> String {
        let limit = limit.clamp(4, paging::DETAIL_BYTES);
        let mut page = self.page(0, limit).expect("valid summary window");
        if page.next_offset.is_some() {
            while page.text.len() + '…'.len_utf8() > limit {
                page.text.pop();
            }
            page.text.push('…');
        }
        page.text
    }
    pub(crate) fn write(self, out: &mut dyn fmt::Write) -> fmt::Result {
        format::record(self, out)
    }
}
