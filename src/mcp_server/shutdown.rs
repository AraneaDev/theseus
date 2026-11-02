//! Monotonic, once-only shutdown policy; ownership remains in ServerRuntime.
use std::time::{Duration, Instant};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeAction {
    Continue,
    OrderlyExit(i32),
    ForceProcessExit(i32),
}
#[derive(Default)]
pub(crate) struct Shutdown {
    started: Option<Instant>,
    fatal: bool,
}
impl Shutdown {
    pub(crate) fn begin(&mut self, now: Instant, fatal: bool) -> bool {
        self.fatal |= fatal;
        if self.started.is_some() {
            return false;
        }
        self.started = Some(now);
        true
    }
    pub(crate) fn active(&self) -> bool {
        self.started.is_some()
    }
    pub(crate) fn action(&self, now: Instant, discharged: bool) -> RuntimeAction {
        let Some(started) = self.started else {
            return RuntimeAction::Continue;
        };
        if discharged {
            RuntimeAction::OrderlyExit(if self.fatal { 2 } else { 0 })
        } else if now.saturating_duration_since(started) >= Duration::from_secs(5) {
            RuntimeAction::ForceProcessExit(2)
        } else {
            RuntimeAction::Continue
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grace_is_once_only_and_force_does_not_claim_discharge() {
        let start = Instant::now();
        let mut s = Shutdown::default();
        assert!(s.begin(start, false));
        assert!(!s.begin(start + Duration::from_secs(4), true));
        assert_eq!(
            s.action(start + Duration::from_millis(4999), false),
            RuntimeAction::Continue
        );
        assert_eq!(
            s.action(start + Duration::from_secs(5), false),
            RuntimeAction::ForceProcessExit(2)
        );
        assert_eq!(
            s.action(start + Duration::from_secs(5), true),
            RuntimeAction::OrderlyExit(2)
        );
    }
    #[test]
    fn orderly_requires_explicit_discharge() {
        let now = Instant::now();
        let mut s = Shutdown::default();
        assert_eq!(s.action(now, true), RuntimeAction::Continue);
        s.begin(now, false);
        assert_eq!(s.action(now, false), RuntimeAction::Continue);
        assert_eq!(s.action(now, true), RuntimeAction::OrderlyExit(0));
    }
}

use super::{
    owned_error::OwnedError,
    types::{ErrorCode, QueryError},
};
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Origin {
    Coordinator,
    Worker,
    Restoration,
    Query,
}
struct Retained {
    origin: Origin,
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "Retained precedence metadata survives finalization even though serve omits diagnostics"
        )
    )]
    ordinal: u64,
    owner: Option<OwnedError>,
}
/// Original errors remain separately typed. Empty cells mean the corresponding
/// owner is in the sole finalizer, whose returned vector preserves input order.
/// No flattening/stringification or cleanup traversal runs on the coordinator.
pub(crate) struct ErrorBundle {
    entries: Vec<Retained>,
    next_ordinal: Option<u64>,
}
impl Default for ErrorBundle {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            next_ordinal: Some(0),
        }
    }
}
impl ErrorBundle {
    pub(crate) fn push(&mut self, owner: OwnedError) {
        self.insert(Origin::Query, owner);
    }
    pub(crate) fn coordinator_owner(&mut self, owner: OwnedError) {
        self.insert(Origin::Coordinator, owner);
    }
    pub(crate) fn unresolved(&self) -> bool {
        self.entries.iter().any(|e| {
            e.owner
                .as_ref()
                .is_none_or(|owner| owner.view().cleanup_blocked)
        })
    }
    pub(crate) fn coordinator(&mut self, source: anyhow::Error) {
        self.insert(
            Origin::Coordinator,
            OwnedError::deferred(QueryError::new(ErrorCode::InternalError, source)),
        );
    }
    pub(crate) fn restoration(&mut self, source: std::io::Error) {
        self.insert(
            Origin::Restoration,
            OwnedError::deferred(QueryError::new(ErrorCode::IoError, source.into())),
        );
    }
    fn insert(&mut self, origin: Origin, owner: OwnedError) {
        let ordinal = self.next_ordinal.unwrap_or(u64::MAX);
        // Exhaustion closes the runtime at its next control check. Keep every
        // actual owner even on that path; never wrap a retention ordinal.
        self.next_ordinal = ordinal.checked_add(1);
        self.entries.push(Retained {
            origin,
            ordinal,
            owner: Some(owner),
        });
    }
    pub(crate) fn sequence_exhausted(&self) -> bool {
        self.next_ordinal.is_none()
    }
    pub(crate) fn extend_registry(&mut self, owners: Vec<OwnedError>) {
        // There is no execution queue: a subsequent admission is possible only
        // after the preceding engine joins. Registry order therefore matches
        // actual worker-error observation order, rather than start-time age.
        for owner in owners {
            self.insert(
                if owner.view().code == ErrorCode::WorkerError {
                    Origin::Worker
                } else {
                    Origin::Query
                },
                owner,
            );
        }
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &OwnedError> {
        self.entries.iter().filter_map(|e| e.owner.as_ref())
    }
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
    pub(crate) fn has_coordinator(&self) -> bool {
        self.entries.iter().any(|e| e.origin == Origin::Coordinator)
    }
    #[cfg(test)]
    pub(crate) fn primary(&self) -> Option<&anyhow::Error> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.origin != Origin::Query)
            .min_by_key(|(index, e)| (e.origin, e.ordinal, *index))
            .and_then(|(_, e)| e.owner.as_ref())
            .map(OwnedError::primary)
    }
    pub(crate) fn take_for_finalizer(&mut self) -> Vec<OwnedError> {
        self.entries
            .iter_mut()
            .filter_map(|e| e.owner.take())
            .collect()
    }
    pub(crate) fn restore(&mut self, owners: Vec<OwnedError>) {
        let mut owners = owners.into_iter();
        for entry in &mut self.entries {
            if entry.owner.is_none() {
                entry.owner = owners.next();
            }
        }
        // An uncaught finalizer join panic may append one additional typed owner.
        for owner in owners {
            self.insert(Origin::Coordinator, owner);
        }
    }
}
#[cfg(test)]
mod ownership_tests {
    use super::*;
    use std::{
        fmt,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };
    #[derive(Debug)]
    struct Primary(u8, Arc<AtomicUsize>);
    impl fmt::Display for Primary {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "error {}", self.0)
        }
    }
    impl std::error::Error for Primary {}
    impl Drop for Primary {
        fn drop(&mut self) {
            self.1.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn owner(id: u8, drops: &Arc<AtomicUsize>) -> OwnedError {
        OwnedError::capture(QueryError::new(
            ErrorCode::WorkerError,
            Primary(id, drops.clone()).into(),
        ))
    }
    #[test]
    fn primary_precedence_and_typed_siblings_survive_finalizer_transfer() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut errors = ErrorBundle::default();
        errors.restoration(std::io::Error::other(Primary(3, drops.clone())));
        errors.extend_registry(vec![owner(1, &drops), owner(2, &drops)]);
        assert_eq!(
            errors
                .primary()
                .unwrap()
                .downcast_ref::<Primary>()
                .unwrap()
                .0,
            1
        );
        errors.coordinator_owner(owner(4, &drops));
        errors.coordinator_owner(owner(5, &drops));
        assert_eq!(
            errors
                .primary()
                .unwrap()
                .downcast_ref::<Primary>()
                .unwrap()
                .0,
            4
        );
        let owners = errors.take_for_finalizer();
        assert!(errors.primary().is_none());
        assert!(errors.unresolved());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        errors.restore(
            std::thread::spawn(move || owners.into_iter().map(OwnedError::finalize).collect())
                .join()
                .unwrap(),
        );
        assert_eq!(
            errors
                .primary()
                .unwrap()
                .downcast_ref::<Primary>()
                .unwrap()
                .0,
            4
        );
        assert!(!errors.unresolved());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(errors);
        assert_eq!(drops.load(Ordering::SeqCst), 5);
    }
    #[test]
    fn missing_finalizer_owner_is_never_discharge_and_ordinals_do_not_wrap() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut errors = ErrorBundle {
            next_ordinal: Some(u64::MAX),
            ..ErrorBundle::default()
        };
        errors.push(owner(1, &drops));
        assert!(errors.sequence_exhausted());
        let held = errors.take_for_finalizer();
        errors.restore(Vec::new());
        assert!(errors.unresolved());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        errors.restore(held);
        assert!(!errors.unresolved());
    }
}
