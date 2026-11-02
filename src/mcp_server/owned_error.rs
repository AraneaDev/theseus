//! Original typed failures and worker-captured, immutable diagnostic projections.
use super::types::{
    ErrorCode, FinalSummary, Handle, Launch, NativeCleanup, QueryError, TextPreview,
};
use crate::runner::process::CleanupFailures;
use serde::Serialize;
use std::fmt::{self, Write};
use theseus_host::process::LaunchPhase;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ErrorView {
    pub code: ErrorCode,
    pub message: TextPreview,
    pub native: NativeCleanup,
    pub launch: Option<Launch>,
    pub cleanup_blocked: bool,
}

#[derive(Debug)]
pub(crate) struct OwnedError {
    error: QueryError,
    cleanup: CleanupFailures,
    view: ErrorView,
}
impl OwnedError {
    /// Only the owning worker (or the caller after its actual join) may capture.
    pub(crate) fn capture(error: QueryError) -> Self {
        let (cleanup, facts) = CleanupFailures::capture_on_worker(&error.source);
        // Retain authority before invoking even bounded Display implementations.
        let view = ErrorView {
            code: error.code,
            message: bounded_message(&error.source),
            native: if facts.pending {
                NativeCleanup::Pending
            } else if facts.publication_unknown {
                NativeCleanup::Unknown
            } else {
                NativeCleanup::Complete
            },
            launch: facts.launch.map(|phase| match phase {
                LaunchPhase::NeverCreated => Launch::NeverCreated,
                LaunchPhase::CreatedSuspended => Launch::CreatedSuspended,
                LaunchPhase::MayRun => Launch::MayRun,
            }),
            cleanup_blocked: facts.pending || facts.publication_unknown,
        };
        Self {
            error,
            cleanup,
            view,
        }
    }
    /// Immutable projection only: no cleanup mutex or native query is touched.
    pub(crate) fn view(&self) -> &ErrorView {
        &self.view
    }
    pub(crate) fn primary(&self) -> &anyhow::Error {
        &self.error.source
    }
    /// Run only on the finalizer occupying the now-free engine slot.
    pub(crate) fn finalize(self) -> Self {
        let Self {
            mut error,
            cleanup,
            view,
        } = self;
        let remaining = cleanup.settled();
        if remaining.pending() {
            // Context retains every unresolved owner without replacing the
            // original primary's type or dropping the original panic payload.
            error.source = error.source.context(remaining);
        }
        let mut finalized = Self::capture(error);
        let rank = |phase| match phase {
            Launch::NeverCreated => 0,
            Launch::CreatedSuspended => 1,
            Launch::MayRun => 2,
        };
        if let Some(historical) = view.launch
            && finalized
                .view
                .launch
                .is_none_or(|current| rank(historical) > rank(current))
        {
            finalized.view.launch = Some(historical);
        }
        finalized
    }
}

/// Stop formatting at the first overflowing fragment; never allocate a complete
/// anyhow chain merely to truncate it afterwards.
fn bounded_message(error: &anyhow::Error) -> TextPreview {
    struct Bounded {
        text: String,
        truncated: bool,
        lossy: bool,
    }
    impl fmt::Write for Bounded {
        fn write_str(&mut self, value: &str) -> fmt::Result {
            let mut end = value.len().min(512 - self.text.len());
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            let part = &value[..end];
            // Native Display conversion uses the replacement character. This
            // conservative flag also covers a literal replacement character.
            self.lossy |= part.contains('\u{fffd}');
            self.text.push_str(part);
            if end < value.len() {
                self.truncated = true;
                Err(fmt::Error)
            } else {
                Ok(())
            }
        }
    }
    let mut bounded = Bounded {
        text: String::with_capacity(512),
        truncated: false,
        lossy: false,
    };
    if write!(&mut bounded, "{error:#}").is_err() {
        bounded.truncated = true;
    }
    TextPreview {
        text: bounded.text,
        truncated: bounded.truncated,
        lossy: bounded.lossy,
    }
}

#[derive(Debug)]
pub(crate) struct EngineCompletion {
    pub handle: Handle,
    pub result: Result<FinalSummary, OwnedError>,
}

#[cfg(test)]
#[path = "owned_error_tests.rs"]
mod tests;
