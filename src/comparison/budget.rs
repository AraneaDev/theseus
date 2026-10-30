//! Conservative accounting before copying variable evidence into derived reports.
use crate::{model::ArtifactDigest, snapshot::FileEntry};

pub const MAX_DERIVED_BYTES: usize = 32 * 1024 * 1024;
// Cover pretty-JSON punctuation, bounded matrix identities, fallback diagnostics,
// validated run IDs/digests and contained recovery paths, even after exhaustion.
pub(super) const CELL_RESERVE: usize = 2048;
pub(super) const SOURCE_RESERVE: usize = 4096;
pub(super) const REPORT_RESERVE: usize = 4096;
pub(super) const BYTE_LIMIT: &str = "derived byte limit exhausted; evidence omitted";

pub(super) struct Budget {
    remaining: usize,
    pub exhausted: bool,
}
impl Budget {
    pub fn new(limit: usize, reserved: usize) -> Option<Self> {
        limit.checked_sub(reserved).map(|remaining| Self {
            remaining,
            exhausted: false,
        })
    }
    pub fn admit(&mut self, cost: Option<usize>) -> bool {
        if !self.exhausted
            && let Some(left) = cost.and_then(|n| self.remaining.checked_sub(n))
        {
            self.remaining = left;
            return true;
        }
        self.exhausted = true;
        false
    }
}
// A UTF-8 byte may expand to six JSON escape bytes. All additions/multiplications
// are checked; overflow is exhaustion, never a wrapped admission.
pub(super) fn strings<'a>(
    fixed: usize,
    values: impl IntoIterator<Item = &'a str>,
) -> Option<usize> {
    values.into_iter().try_fold(fixed, |sum, value| {
        sum.checked_add(value.len().checked_mul(6)?)
    })
}
pub(super) fn entry(entry: Option<&FileEntry>) -> Option<usize> {
    let Some(entry) = entry else {
        return Some(0);
    };
    strings(
        512,
        entry
            .sha256
            .iter()
            .chain(&entry.link_target)
            .map(String::as_str),
    )
}
pub(super) fn artifacts(artifacts: &[ArtifactDigest]) -> Option<usize> {
    artifacts.iter().try_fold(0usize, |sum, artifact| {
        let sum = sum.checked_add(strings(
            512,
            [artifact.name.as_str(), artifact.sha256.as_str()],
        )?)?;
        artifact.files.iter().try_fold(sum, |sum, file| {
            sum.checked_add(strings(256, [file.path.as_str(), file.sha256.as_str()])?)
        })
    })
}
