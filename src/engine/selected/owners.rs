//! Native owner bundle, retained-failure wrapper and cleanup-facts inspection.
use super::super::{EngineEvent, EventKind, domain::*};
use crate::{
    native::{
        launch::LaunchPreparation,
        provider::CommandProvider,
        transport::{NativeSession, NativeStartFailure},
    },
    report::ReportStore,
};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(in crate::engine) struct Owners {
    pub(in crate::engine) provider: Option<CommandProvider>,
    pub(in crate::engine) session: Option<NativeSession>,
    pub(in crate::engine) start: Option<NativeStartFailure>,
    pub(in crate::engine) preparation: Option<LaunchPreparation>,
    pub(in crate::engine) errors: Vec<anyhow::Error>,
    pub(in crate::engine) panics: Vec<Box<dyn std::any::Any + Send>>,
    pub(in crate::engine) pending_cleanup: bool,
    pub(in crate::engine) remote_unknown: bool,
    pub(in crate::engine) evidence: Option<(ReportStore, StoredRun, StoredJournal)>,
    pub(in crate::engine) intents: Option<theseus_native_helper_store::IntentStore>,
    pub(super) nested_cleanup: Vec<crate::runner::process::CleanupFailures>,
    pub(super) publication_unknown: bool,
    pub(super) captured_errors: usize,
}
impl Owners {
    pub(super) fn capture_errors(&mut self) {
        let first = self.captured_errors;
        self.captured_errors = self.errors.len();
        let captures = self.errors[first..]
            .iter()
            .map(|error| {
                catch_unwind(AssertUnwindSafe(|| {
                    crate::runner::process::CleanupFailures::capture_on_worker(error)
                }))
            })
            .collect::<Vec<_>>();
        for capture in captures {
            match capture {
                Ok((cleanup, facts)) => {
                    self.publication_unknown |= facts.publication_unknown;
                    self.nested_cleanup.push(cleanup);
                }
                Err(payload) => {
                    self.pending_cleanup = true;
                    self.publication_unknown = true;
                    self.panics.push(payload);
                }
            }
        }
    }
    fn settle(&mut self) -> bool {
        self.settle_for(Duration::from_secs(30))
    }
    fn settle_for(&mut self, budget: Duration) -> bool {
        if let Some(s) = &mut self.session {
            s.request_stop();
        }
        if let Some(s) = &mut self.start {
            s.request_stop();
        }
        if let Some(s) = &mut self.preparation {
            s.request_stop();
        }
        let end = Instant::now() + budget;
        loop {
            let mut done = true;
            if let Some(s) = &mut self.session {
                match s.poll_settle() {
                    Ok(v) => done &= v && s.is_settled(),
                    Err(e) => {
                        self.errors.push(e.into());
                        return false;
                    }
                }
            }
            if let Some(s) = &mut self.start {
                match s.poll_settle() {
                    Ok(v) => done &= v && s.is_settled(),
                    Err(e) => {
                        self.errors.push(e);
                        return false;
                    }
                }
            }
            if let Some(s) = &mut self.preparation {
                match s.poll_settle() {
                    Ok(v) => done &= v && s.is_settled(),
                    Err(e) => {
                        self.errors.push(e);
                        return false;
                    }
                }
            }
            if let Some(s) = &mut self.provider {
                match s.poll_settle() {
                    Ok(v) => done &= v && s.settled(),
                    Err(e) => {
                        self.errors.push(e);
                        return false;
                    }
                }
            }
            if done {
                self.nested_cleanup = std::mem::take(&mut self.nested_cleanup)
                    .into_iter()
                    .map(|cleanup| cleanup.settled())
                    .collect();
                return !self.nested_cleanup.iter().any(|cleanup| cleanup.pending());
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::yield_now();
        }
    }
}
/// Original typed errors, panic payloads and partial native owners survive an unresolved join.
struct RetainedFailure {
    message: String,
    owners: Mutex<Owners>,
}
impl std::fmt::Debug for RetainedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetainedNativeFailure")
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for RetainedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for RetainedFailure {}
pub(in crate::engine) fn retained_failure(message: String, mut owners: Owners) -> anyhow::Error {
    owners.capture_errors();
    anyhow::Error::new(RetainedFailure {
        message,
        owners: Mutex::new(owners),
    })
}
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NativeCleanupFacts {
    pub local_pending: bool,
    pub remote_unknown: bool,
    pub publication_unknown: bool,
}
impl NativeCleanupFacts {
    pub fn pending(self) -> bool {
        self.local_pending || self.remote_unknown || self.publication_unknown
    }
    fn unavailable() -> Self {
        Self {
            local_pending: true,
            remote_unknown: true,
            publication_unknown: true,
        }
    }
}
fn cleanup_facts(owners: &Owners) -> NativeCleanupFacts {
    NativeCleanupFacts {
        local_pending: owners
            .nested_cleanup
            .iter()
            .any(|cleanup| cleanup.pending())
            || owners
                .session
                .as_ref()
                .is_some_and(|owner| !owner.is_settled())
            || owners
                .start
                .as_ref()
                .is_some_and(|owner| !owner.is_settled())
            || owners
                .preparation
                .as_ref()
                .is_some_and(|owner| !owner.is_settled())
            || owners
                .provider
                .as_ref()
                .is_some_and(|owner| !owner.settled()),
        remote_unknown: owners.remote_unknown,
        publication_unknown: owners.publication_unknown,
    }
}
/// Read-only inspection: never traverses error sources or polls native owners.
pub(crate) fn native_cleanup_facts(error: &anyhow::Error) -> Option<NativeCleanupFacts> {
    let failure = error.downcast_ref::<RetainedFailure>()?;
    Some(match failure.owners.try_lock() {
        Ok(owners) => cleanup_facts(&owners),
        Err(_) => NativeCleanupFacts::unavailable(),
    })
}
pub(crate) fn native_cleanup_pending(error: &anyhow::Error) -> bool {
    native_cleanup_facts(error).is_some_and(NativeCleanupFacts::pending)
}
/// Owning finalizer workers only: advance the exact existing local owner bundle.
/// No launch, provider invocation/query, report publication or new identity occurs.
pub(crate) fn finalize_native_cleanup(error: &anyhow::Error) -> Option<NativeCleanupFacts> {
    let failure = error.downcast_ref::<RetainedFailure>()?;
    let Ok(mut owners) = failure.owners.lock() else {
        return Some(NativeCleanupFacts::unavailable());
    };
    owners.capture_errors();
    let joined = match catch_unwind(AssertUnwindSafe(|| {
        owners.settle_for(Duration::from_millis(250))
    })) {
        Ok(joined) => joined,
        Err(payload) => {
            owners.panics.push(payload);
            false
        }
    };
    owners.pending_cleanup = owners.remote_unknown || !joined;
    owners.capture_errors();
    Some(cleanup_facts(&owners))
}
#[cfg(feature = "native-catalog-test-support")]
#[doc(hidden)]
pub fn fixture_retained_native_session(
    session: NativeSession,
    remote_unknown: bool,
) -> anyhow::Error {
    retained_failure(
        "explicit inert native cleanup fixture".into(),
        Owners {
            session: Some(session),
            remote_unknown,
            pending_cleanup: true,
            ..Owners::default()
        },
    )
}

#[cfg(feature = "native-catalog-test-support")]
#[doc(hidden)]
pub fn fixture_native_cleanup_facts(error: &anyhow::Error) -> (bool, bool, bool) {
    let facts = native_cleanup_facts(error).expect("explicit retained native fixture");
    (
        facts.local_pending,
        facts.remote_unknown,
        facts.publication_unknown,
    )
}
#[cfg(feature = "native-catalog-test-support")]
#[doc(hidden)]
pub fn fixture_finalize_native_cleanup(error: &anyhow::Error) -> (bool, bool, bool) {
    let facts = finalize_native_cleanup(error).expect("explicit retained native fixture");
    (
        facts.local_pending,
        facts.remote_unknown,
        facts.publication_unknown,
    )
}

pub(in crate::engine) fn event(
    owners: &mut Owners,
    emit: &mut impl FnMut(EngineEvent),
    kind: EventKind,
    path: &Path,
    id: Option<&str>,
) {
    let emitted = catch_unwind(AssertUnwindSafe(|| {
        emit(EngineEvent {
            kind,
            report_path: path.to_path_buf(),
            scenario_id: id.map(str::to_owned),
            stage: None,
        })
    }));
    if let Err(payload) = emitted {
        owners.panics.push(payload);
    }
}
pub(in crate::engine) fn settle_owned(owners: &mut Owners) -> bool {
    owners.capture_errors();
    match catch_unwind(AssertUnwindSafe(|| owners.settle())) {
        Ok(settled) => settled,
        Err(payload) => {
            owners.panics.push(payload);
            false
        }
    }
}
