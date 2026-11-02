//! Original typed failures and worker-captured, immutable diagnostic projections.
use super::types::{
    ErrorCode, FinalSummary, Handle, Launch, NativeCleanup, QueryError, TextPreview,
};
use crate::runner::process::{CleanupFailures, worker_panic};
use serde::Serialize;
use std::{
    fmt::{self, Write},
    panic::{AssertUnwindSafe, catch_unwind},
};
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
    // Every retained cleanup reference is also owned by the original primary or
    // one of these typed siblings, outside all potentially unwinding callbacks.
    panics: Vec<anyhow::Error>,
    has_worker_panic: bool,
}
impl OwnedError {
    /// Retain a coordinator-origin error without formatting, source traversal,
    /// cleanup locks or native queries. Only the finalizer may refresh it.
    pub(crate) fn deferred(error: QueryError) -> Self {
        Self {
            view: ErrorView {
                code: error.code,
                message: TextPreview::new("error diagnostic unavailable", false),
                native: NativeCleanup::Unknown,
                launch: None,
                cleanup_blocked: true,
            },
            error,
            cleanup: CleanupFailures::default(),
            panics: Vec::new(),
            has_worker_panic: false,
        }
    }
    /// Only the owning worker (or the caller after its actual join) may capture.
    pub(crate) fn capture(error: QueryError) -> Self {
        let mut owner = Self::deferred(error);
        owner.refresh();
        owner
    }
    /// Immutable projection only: no cleanup mutex or native query is touched.
    pub(crate) fn view(&self) -> &ErrorView {
        &self.view
    }
    #[cfg(test)]
    pub(crate) fn primary(&self) -> &anyhow::Error {
        &self.error.source
    }
    /// A panic caught during capture/finalization is fatal even when the original
    /// query classification was an ordinary domain error. No native calls here.
    pub(crate) fn has_worker_panic(&self) -> bool {
        self.has_worker_panic
    }
    /// Run only on the finalizer occupying the now-free engine slot.
    pub(crate) fn finalize(mut self) -> Self {
        let settled = catch_unwind(AssertUnwindSafe(|| {
            // Settle only freshly retained shared references. Their authoritative
            // primary and panic siblings, and the previous retention bundle,
            // all stay in self outside this potentially unwinding closure.
            let (mut cleanup, _) = CleanupFailures::capture_on_worker(&self.error.source);
            for panic in &self.panics {
                cleanup.retain(panic);
            }
            let remaining = cleanup.settled();
            let pending = remaining.pending();
            (remaining, pending)
        }));
        match settled {
            Ok((remaining, pending)) => {
                if pending {
                    self.error.source = self.error.source.context(remaining);
                }
                self.refresh();
            }
            Err(payload) => self.record_panic(payload, true),
        }
        self
    }
    fn refresh(&mut self) {
        // Only borrow the original: source() and Display are user implementations
        // and can unwind. The original and every panic sibling remain outside.
        let sampled = catch_unwind(AssertUnwindSafe(|| {
            CleanupFailures::capture_on_worker(&self.error.source)
        }));
        let (cleanup, facts) = match sampled {
            Ok(sampled) => sampled,
            Err(payload) => {
                self.record_panic(payload, true);
                return;
            }
        };
        self.cleanup = cleanup;
        self.view.native = if facts.pending {
            NativeCleanup::Pending
        } else if facts.publication_unknown {
            NativeCleanup::Unknown
        } else {
            NativeCleanup::Complete
        };
        self.view.cleanup_blocked = facts.pending || facts.publication_unknown;
        self.record_launch(facts.launch);
        // Sibling panic authority need not appear in the primary's source chain.
        for index in 0..self.panics.len() {
            let (_, facts) = CleanupFailures::capture_on_worker(&self.panics[index]);
            self.cleanup.retain(&self.panics[index]);
            self.record_launch(facts.launch);
            if facts.pending {
                self.view.native = NativeCleanup::Pending;
                self.view.cleanup_blocked = true;
            }
        }
        match catch_unwind(AssertUnwindSafe(|| bounded_message(&self.error.source))) {
            Ok(message) => self.view.message = message,
            Err(payload) => self.record_panic(payload, false),
        }
    }
    fn record_panic(&mut self, payload: Box<dyn std::any::Any + Send>, unknown: bool) {
        // worker_panic uses its concrete ownership wrapper, not the panicking
        // original's Display/source callbacks. Retain that typed sibling first.
        self.panics.push(worker_panic(payload));
        self.has_worker_panic = true;
        let panic = self.panics.last().expect("just retained panic");
        let (_, facts) = CleanupFailures::capture_on_worker(panic);
        self.cleanup.retain(panic);
        self.record_launch(facts.launch);
        if facts.pending {
            self.view.native = NativeCleanup::Pending;
            self.view.cleanup_blocked = true;
        } else if unknown {
            if self.view.native != NativeCleanup::Pending {
                self.view.native = NativeCleanup::Unknown;
            }
            self.view.cleanup_blocked = true;
        }
        self.view.message = TextPreview::new("error diagnostic or cleanup worker panicked", false);
    }
    fn record_launch(&mut self, phase: Option<LaunchPhase>) {
        let phase = phase.map(|phase| match phase {
            LaunchPhase::NeverCreated => Launch::NeverCreated,
            LaunchPhase::CreatedSuspended => Launch::CreatedSuspended,
            LaunchPhase::MayRun => Launch::MayRun,
        });
        let rank = |phase| match phase {
            Launch::NeverCreated => 0,
            Launch::CreatedSuspended => 1,
            Launch::MayRun => 2,
        };
        if let Some(phase) = phase
            && self
                .view
                .launch
                .is_none_or(|prior| rank(phase) > rank(prior))
        {
            self.view.launch = Some(phase);
        }
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
