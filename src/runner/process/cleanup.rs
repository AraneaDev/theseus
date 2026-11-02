//! Retain native cleanup authority across diagnostic-only application records.
use std::{
    fmt, io,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use theseus_host::process::{HostCleanup, LaunchPhase, SpawnFailure};

#[derive(Debug)]
struct FailureState {
    launch: LaunchPhase,
    error: io::Error,
    diagnostics: Vec<io::Error>,
    cleanup: Mutex<HostCleanup>,
    deadline: Instant,
}

#[derive(Clone, Debug)]
pub(crate) struct AcquisitionFailure(Arc<FailureState>);
impl AcquisitionFailure {
    pub(super) fn from_spawn(failure: SpawnFailure, deadline: Instant) -> Self {
        Self(Arc::new(FailureState {
            launch: failure.launch,
            error: failure.error,
            diagnostics: failure.cleanup_errors,
            cleanup: Mutex::new(failure.cleanup),
            deadline,
        }))
    }
    pub(super) fn after_launch(message: String) -> Self {
        Self::from_spawn(
            SpawnFailure {
                launch: LaunchPhase::MayRun,
                error: io::Error::other(message),
                cleanup: HostCleanup::Complete,
                cleanup_errors: vec![],
            },
            Instant::now(),
        )
    }
    pub(super) fn into_io_error(self) -> io::Error {
        // io::Error delegates source() to its payload's source(), skipping the
        // payload itself. This layer keeps the actionable owner in that chain.
        io::Error::other(EndpointFailure(self))
    }
    pub(crate) fn launch(&self) -> LaunchPhase {
        self.0.launch
    }
    pub(crate) fn pending(&self) -> bool {
        self.0
            .cleanup
            .lock()
            .map_or(true, |cleanup| matches!(*cleanup, HostCleanup::Pending(_)))
    }
    /// A failed/unfinished attempt retains the original guard. The clock belongs
    /// to the first abort, so lowering and retrying errors cannot extend it.
    pub(crate) fn retry_cleanup(&self) -> io::Result<bool> {
        let mut cleanup = self
            .0
            .cleanup
            .lock()
            .map_err(|_| io::Error::other("native cleanup ownership lock poisoned"))?;
        match &mut *cleanup {
            HostCleanup::Complete => Ok(true),
            HostCleanup::Pending(owner) => {
                if owner.try_complete()? {
                    *cleanup = HostCleanup::Complete;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
        }
    }
    fn settle(&self) {
        loop {
            match self.retry_cleanup() {
                Ok(true) | Err(_) => break,
                Ok(false) if Instant::now() < self.0.deadline => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Ok(false) => break,
            }
        }
    }
}
impl fmt::Display for AcquisitionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "native launch {:?}: {}", self.0.launch, self.0.error)?;
        for diagnostic in &self.0.diagnostics {
            write!(f, "; cleanup: {diagnostic}")?;
        }
        Ok(())
    }
}
impl std::error::Error for AcquisitionFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0.error)
    }
}

#[derive(Debug)]
struct EndpointFailure(AcquisitionFailure);
impl fmt::Display for EndpointFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for EndpointFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

#[derive(Default, Debug)]
pub(crate) struct CleanupFailures {
    owners: Vec<AcquisitionFailure>,
    panics: Vec<PanicFailure>,
}
/// Facts sampled only while the caller owns the error on its worker.
#[derive(Debug, Default)]
pub(crate) struct WorkerCleanupSnapshot {
    pub(crate) launch: Option<LaunchPhase>,
    pub(crate) pending: bool,
    pub(crate) publication_unknown: bool,
}
impl CleanupFailures {
    /// Capture before formatting or transfer; unlike `retain`, history includes
    /// completed acquisitions. Never call this from a coordinator polling views.
    pub(crate) fn capture_on_worker(error: &anyhow::Error) -> (Self, WorkerCleanupSnapshot) {
        let mut capture = WorkerCapture::default();
        // Typed anyhow contexts are not individual Error::source entries.
        if let Some(failures) = error.downcast_ref::<Self>() {
            capture.aggregate(failures);
        }
        if let Some(failure) = error.downcast_ref::<AcquisitionFailure>() {
            capture.acquisition(failure);
        }
        if let Some(panic) = error.downcast_ref::<PanicFailure>() {
            capture.panic(panic);
        }
        if let Some(publication) = error.downcast_ref::<theseus_host::fs::PublicationError>() {
            capture.publication(publication);
        }
        if let Some(io) = error.downcast_ref::<io::Error>() {
            capture.source(io);
        }
        capture.source(error.as_ref());
        capture.facts.pending = capture.retained.pending();
        (capture.retained, capture.facts)
    }
    pub(crate) fn retain(&mut self, error: &anyhow::Error) {
        // anyhow exposes typed context through downcast_ref, not as a separate
        // Error::source entry. Keep that aggregate as well as source-chain owners.
        if let Some(failures) = error.downcast_ref::<Self>() {
            self.extend(failures);
        }
        for source in error.chain() {
            if let Some(failure) = source.downcast_ref::<AcquisitionFailure>() {
                self.push(failure);
            }
            if let Some(panic) = source.downcast_ref::<PanicFailure>() {
                self.push_panic(panic);
            }
            if let Some(failures) = source.downcast_ref::<Self>() {
                self.extend(failures);
            }
        }
    }
    fn extend(&mut self, failures: &Self) {
        for failure in &failures.owners {
            self.push(failure);
        }
        for panic in &failures.panics {
            self.push_panic(panic);
        }
    }
    pub(crate) fn retain_panic(&mut self, panic: &(dyn std::any::Any + Send)) {
        if let Some(panic) = panic.downcast_ref::<PanicFailure>() {
            self.push_panic(panic);
        }
    }
    fn push_panic(&mut self, panic: &PanicFailure) {
        for failure in &panic.0.cleanup.owners {
            self.push(failure);
        }
        if !self
            .panics
            .iter()
            .any(|prior| Arc::ptr_eq(&prior.0, &panic.0))
        {
            self.panics.push(panic.clone());
        }
    }
    fn push(&mut self, failure: &AcquisitionFailure) {
        if failure.pending()
            && !self
                .owners
                .iter()
                .any(|prior| Arc::ptr_eq(&prior.0, &failure.0))
        {
            self.owners.push(failure.clone());
        }
    }
    pub(crate) fn pending(&self) -> bool {
        self.owners.iter().any(AcquisitionFailure::pending)
    }
    pub(crate) fn settled(self) -> Self {
        for failure in &self.owners {
            failure.settle();
        }
        Self {
            owners: self
                .owners
                .into_iter()
                .filter(AcquisitionFailure::pending)
                .collect(),
            panics: self.panics,
        }
    }
    pub(crate) fn finish(self) -> anyhow::Result<()> {
        let pending = self.settled();
        if pending.owners.is_empty() {
            Ok(())
        } else {
            Err(pending.into())
        }
    }
    pub(crate) fn resume_unwind(self, original: Box<dyn std::any::Any + Send>) -> ! {
        let cleanup = self.settled();
        if cleanup.pending() {
            std::panic::resume_unwind(Box::new(PanicFailure::new(original, cleanup)));
        }
        std::panic::resume_unwind(original)
    }
}
/// Worker-local traversal, separate from diagnostic rendering and pending filtering.
#[derive(Default)]
struct WorkerCapture {
    retained: CleanupFailures,
    facts: WorkerCleanupSnapshot,
}
impl WorkerCapture {
    fn acquisition(&mut self, failure: &AcquisitionFailure) {
        let rank = |phase| match phase {
            LaunchPhase::NeverCreated => 0,
            LaunchPhase::CreatedSuspended => 1,
            LaunchPhase::MayRun => 2,
        };
        let launch = failure.launch();
        if self
            .facts
            .launch
            .is_none_or(|prior| rank(launch) > rank(prior))
        {
            self.facts.launch = Some(launch);
        }
        // Record history first: push intentionally filters settled acquisitions.
        self.retained.push(failure);
    }
    fn aggregate(&mut self, failures: &CleanupFailures) {
        for failure in &failures.owners {
            self.acquisition(failure);
        }
        for panic in &failures.panics {
            self.panic(panic);
        }
    }
    fn panic(&mut self, panic: &PanicFailure) {
        self.aggregate(&panic.0.cleanup);
        self.retained.push_panic(panic);
    }
    fn publication(&mut self, publication: &theseus_host::fs::PublicationError) {
        self.facts.publication_unknown |=
            publication.state() == theseus_host::fs::PublicationState::Unknown;
        if let Some(cleanup) = publication.cleanup_error() {
            self.source(cleanup);
        }
    }
    fn source(&mut self, mut source: &(dyn std::error::Error + 'static)) {
        loop {
            if let Some(failure) = source.downcast_ref::<AcquisitionFailure>() {
                self.acquisition(failure);
            }
            if let Some(failures) = source.downcast_ref::<CleanupFailures>() {
                self.aggregate(failures);
            }
            if let Some(panic) = source.downcast_ref::<PanicFailure>() {
                self.panic(panic);
            }
            if let Some(publication) = source.downcast_ref::<theseus_host::fs::PublicationError>() {
                self.publication(publication);
            }
            // io::Error::source skips its payload. Visit get_ref instead, then
            // follow that payload's own source; this also avoids duplicate walks.
            if let Some(io) = source.downcast_ref::<io::Error>()
                && let Some(payload) = io.get_ref()
            {
                source = payload;
            } else if let Some(next) = source.source() {
                source = next;
            } else {
                break;
            }
        }
    }
}

impl fmt::Display for CleanupFailures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "native host cleanup remains unresolved")?;
        for failure in &self.owners {
            write!(f, "; {failure}")?;
        }
        for panic in &self.panics {
            write!(f, "; original panic: {}", panic.0.cause)?;
        }
        Ok(())
    }
}
impl std::error::Error for CleanupFailures {}

#[cfg(all(test, windows))]
pub(crate) fn release_test_fault(failure: &AcquisitionFailure) {
    if let HostCleanup::Pending(owner) = &mut *failure.0.cleanup.lock().unwrap() {
        theseus_host::process::test_support::fail_cleanup_attempts(owner, 0);
    }
}

#[derive(Debug)]
struct CleanupPanic {
    cause: String,
    _original: Mutex<Box<dyn std::any::Any + Send>>,
    cleanup: CleanupFailures,
}
// A shared, per-run payload owner survives error-to-diagnostic lowering alongside
// native cleanup. Nested panic owners form an acyclic chain, never a registry.
#[derive(Clone, Debug)]
struct PanicFailure(Arc<CleanupPanic>);
impl PanicFailure {
    fn new(original: Box<dyn std::any::Any + Send>, cleanup: CleanupFailures) -> Self {
        let cause = original
            .downcast_ref::<&str>()
            .map(|cause| (*cause).to_string())
            .or_else(|| original.downcast_ref::<String>().cloned())
            .or_else(|| {
                original
                    .downcast_ref::<Self>()
                    .map(|panic| panic.0.cause.clone())
            })
            .unwrap_or_else(|| "non-string panic payload retained".into());
        Self(Arc::new(CleanupPanic {
            cause,
            _original: Mutex::new(original),
            cleanup,
        }))
    }
}
impl fmt::Display for PanicFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Ordinary completed-cleanup behavior keeps its existing diagnostic.
        // The payload remains owned until all sibling/completion owners are known.
        if self.0.cleanup.owners.is_empty() {
            f.write_str("engine worker panicked; inspect recovery journal")
        } else {
            write!(
                f,
                "engine worker panicked: {}; {}",
                self.0.cause, self.0.cleanup
            )
        }
    }
}
impl std::error::Error for PanicFailure {}
pub(crate) fn worker_panic(payload: Box<dyn std::any::Any + Send>) -> anyhow::Error {
    match payload.downcast::<PanicFailure>() {
        Ok(owner) => anyhow::Error::new(*owner),
        Err(original) => {
            anyhow::Error::new(PanicFailure::new(original, CleanupFailures::default()))
        }
    }
}

#[cfg(all(test, windows))]
pub(crate) mod native_tests {
    use super::*;
    use crate::{
        engine::CancellationToken,
        model::*,
        recipe::{Limits, ValidatedRecipe},
        report::ReportStore,
        runner::{CommandRequest, Environment, ExecutedCommand, PreparedEnvironment},
        snapshot::Snapshot,
    };
    pub(crate) fn suspended_failure() -> (tempfile::TempDir, AcquisitionFailure) {
        use theseus_host::process::{
            InputMode,
            test_support::{self, LaunchFault},
        };
        let temp = tempfile::tempdir().unwrap();
        let mut command = super::super::test_support::fixture("launch-marker", &[]);
        command.arg(temp.path().join("must-not-run"));
        let mut failure =
            test_support::spawn(&command, InputMode::Pipe, LaunchFault::CleanupPending)
                .unwrap_err();
        let HostCleanup::Pending(owner) = &mut failure.cleanup else {
            panic!("missing real native owner")
        };
        test_support::fail_cleanup_attempts(owner, 32);
        let failure =
            AcquisitionFailure::from_spawn(failure, Instant::now() + Duration::from_secs(3));
        (temp, failure)
    }
    pub(crate) fn finish_native(temp: &tempfile::TempDir, failure: &AcquisitionFailure) {
        release_test_fault(failure);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !failure.retry_cleanup().unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!failure.pending());
        assert!(!temp.path().join("must-not-run").exists());
    }
    pub(crate) struct FailingEnvironment {
        pub failure: AcquisitionFailure,
        pub removed: usize,
        pub panic_setup: bool,
        pub fail_remove: bool,
    }
    impl Environment for FailingEnvironment {
        fn prepare(
            &mut self,
            _: &ValidatedRecipe,
            _: bool,
            _: &CancellationToken,
        ) -> anyhow::Result<PreparedEnvironment> {
            Ok(PreparedEnvironment {
                image_id: "sha256:fixture".into(),
                artifacts: vec![],
            })
        }
        fn create(
            &mut self,
            run: &str,
            scenario: &str,
            _: &ValidatedRecipe,
        ) -> anyhow::Result<OwnedResource> {
            Ok(OwnedResource {
                container_id: "a".repeat(64),
                labels: std::collections::BTreeMap::from([
                    ("io.theseus.owner".into(), "v1".into()),
                    ("io.theseus.run".into(), run.into()),
                    ("io.theseus.scenario".into(), scenario.into()),
                ]),
                removed: false,
            })
        }
        fn setup(
            &mut self,
            _: &OwnedResource,
            _: &ValidatedRecipe,
            _: &CancellationToken,
        ) -> anyhow::Result<()> {
            if self.panic_setup {
                let mut cleanup = CleanupFailures::default();
                cleanup.retain(&self.failure.clone().into());
                cleanup.resume_unwind(Box::new("original setup panic with native owner"));
            }
            Err(self.failure.clone().into())
        }
        fn execute(
            &mut self,
            _: &OwnedResource,
            _: CommandRequest,
            _: &ReportStore,
            _: &CancellationToken,
        ) -> anyhow::Result<ExecutedCommand> {
            panic!("no unrelated execution after native pending owner")
        }
        fn observe(
            &mut self,
            _: &OwnedResource,
            _: &[String],
            _: Limits,
            _: &CancellationToken,
        ) -> anyhow::Result<Vec<Snapshot>> {
            panic!("no observation after failed setup")
        }
        fn stop(&mut self, _: &OwnedResource) -> anyhow::Result<()> {
            Ok(())
        }
        fn remove(&mut self, _: &OwnedResource) -> anyhow::Result<()> {
            assert!(
                self.failure.pending(),
                "host authority must survive until owned guest teardown"
            );
            self.removed += 1;
            if self.fail_remove {
                return Err(self.failure.clone().into());
            }
            Ok(())
        }
    }
    #[test]
    fn engine_retains_actual_suspended_owner_through_report_lowering_and_guest_cleanup() {
        let (temp, failure) = suspended_failure();
        let recipe =
            crate::recipe::load(std::path::Path::new("tests/fixtures/lifecycle/recipe.toml"))
                .unwrap();
        let mut environment = FailingEnvironment {
            failure: failure.clone(),
            removed: 0,
            panic_setup: false,
            fail_remove: false,
        };
        let mut options = crate::engine::RunOptions::all(&recipe);
        options.output_root = temp.path().join("runs");
        options.profiles = vec![ProfileName::User];
        options.scenarios = vec![ScenarioKind::Clean, ScenarioKind::Upgrade];
        let error = crate::engine::run_with(
            &recipe,
            options,
            CancellationToken::default(),
            &mut environment,
            |_| {},
        )
        .err()
        .expect("pending native owner must propagate beyond report strings");
        assert_eq!(
            environment.removed, 1,
            "guest cleanup precedes return, later scenario must not run"
        );
        let mut retained = CleanupFailures::default();
        retained.retain(&error);
        assert!(retained.pending());
        finish_native(&temp, &failure);
        retained.finish().unwrap();
    }
    #[test]
    fn original_engine_panic_and_actual_owner_survive_guest_teardown() {
        let (temp, failure) = suspended_failure();
        let recipe =
            crate::recipe::load(std::path::Path::new("tests/fixtures/lifecycle/recipe.toml"))
                .unwrap();
        let mut environment = FailingEnvironment {
            failure: failure.clone(),
            removed: 0,
            panic_setup: true,
            fail_remove: false,
        };
        let mut options = crate::engine::RunOptions::all(&recipe);
        options.output_root = temp.path().join("runs");
        options.profiles = vec![ProfileName::User];
        options.scenarios = vec![ScenarioKind::Clean];
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::engine::run_with(
                &recipe,
                options,
                CancellationToken::default(),
                &mut environment,
                |_| {},
            )
        }))
        .err()
        .expect("original panic must propagate after owned guest cleanup");
        let error = worker_panic(panic);
        assert!(format!("{error:#}").contains("original setup panic with native owner"));
        let mut retained = CleanupFailures::default();
        retained.retain(&error);
        assert!(retained.pending());
        assert_eq!(environment.removed, 1);
        finish_native(&temp, &failure);
        retained.finish().unwrap();
    }

    #[test]
    fn actual_pending_owner_survives_native_endpoint_io_wrapper() {
        let (temp, failure) = suspended_failure();
        let error = anyhow::Error::new(failure.clone().into_io_error());
        let mut retained = CleanupFailures::default();
        retained.retain(&error);
        assert!(retained.pending());
        finish_native(&temp, &failure);
        retained.finish().unwrap();
    }

    #[test]
    fn comparison_retains_actual_suspended_owner_after_source_diagnostic_lowering() {
        let (temp, failure) = suspended_failure();
        std::fs::create_dir(temp.path().join("target")).unwrap();
        std::fs::write(
            temp.path().join("recipe.toml"),
            include_str!("../../../tests/fixtures/recipes/v2_clean.toml"),
        )
        .unwrap();
        std::fs::write(temp.path().join("comparison.toml"), "schema_version=1\nname='native cleanup'\nbaseline='copy'\nprofiles=['user']\nscenarios=['clean']\nstages=['configured']\n[[variants]]\nid='copy'\nrecipe='recipe.toml'\n[[variants]]\nid='archive'\nrecipe='recipe.toml'\n[[scopes]]\nid='home'\nprofile='user'\npath='/home/theseus'\n").unwrap();
        let suite = crate::comparison::load(&temp.path().join("comparison.toml")).unwrap();
        let mut environment = FailingEnvironment {
            failure: failure.clone(),
            removed: 0,
            panic_setup: false,
            fail_remove: true,
        };
        let mut suite_path = None;
        let error = crate::comparison::run_with(
            &suite,
            crate::comparison::SuiteRunOptions {
                output_root: temp.path().join("runs"),
                pull: false,
            },
            CancellationToken::default(),
            &mut environment,
            |event| {
                suite_path = Some(event.report_path);
            },
        )
        .err()
        .expect("pending owner cannot become only a source.error string");
        let mut retained = CleanupFailures::default();
        retained.retain(&error);
        assert!(retained.pending());
        assert_eq!(
            environment.removed, 1,
            "remove owned guest; do not continue another variant"
        );
        let cleanup_error =
            crate::comparison::cleanup_with(&suite_path.unwrap(), &mut environment).unwrap_err();
        retained.retain(&cleanup_error);
        assert!(retained.pending());
        assert_eq!(
            environment.removed, 2,
            "explicit recovery retries only journaled owned guest"
        );
        finish_native(&temp, &failure);
        retained.finish().unwrap();
    }
}

#[cfg(test)]
pub(crate) mod io_error_boundary_tests {
    use super::*;

    pub(crate) struct PendingFixture {
        pub(crate) failure: AcquisitionFailure,
        _temp: tempfile::TempDir,
    }
    impl PendingFixture {
        pub(crate) fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let ready = temp.path().join("ready");
            let heartbeat = temp.path().join("heartbeat");
            let mut command = super::super::test_support::fixture("ready-hold", &[]);
            command.arg(&ready).arg(&heartbeat).arg("30000");
            let mut child = super::super::HostProcess::spawn(&command).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !ready.exists() || !heartbeat.exists() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            let failure = AcquisitionFailure::from_spawn(
                SpawnFailure {
                    launch: LaunchPhase::MayRun,
                    error: io::Error::other("owned fixture cleanup witness"),
                    cleanup_errors: vec![],
                    cleanup: HostCleanup::Pending(child.child.take().unwrap().into_cleanup()),
                },
                deadline,
            );
            // A real poisoned ownership lock must retain authority, never claim Complete.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _owner = failure.0.cleanup.lock().unwrap();
                panic!("inject ownership-lock poison");
            }));
            Self {
                failure,
                _temp: temp,
            }
        }
        pub(crate) fn release_fault(&self) {
            self.failure.0.cleanup.clear_poison();
        }
    }
    impl Drop for PendingFixture {
        fn drop(&mut self) {
            // Rescue the actual child on RED/assertion paths as well as success.
            self.release_fault();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !self.failure.retry_cleanup().unwrap() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
    #[test]
    fn native_endpoint_failure_is_visible_through_io_error_source_chain() {
        let error = anyhow::Error::new(
            AcquisitionFailure::after_launch("native endpoint completion failed".into())
                .into_io_error(),
        );
        assert!(
            error
                .chain()
                .any(|source| source.downcast_ref::<AcquisitionFailure>().is_some()),
            "Read adapters must retain the typed native owner through io::Error"
        );
    }
    #[test]
    fn joined_worker_aggregate_retains_original_panic_payload_and_actual_poisoned_owner() {
        joined_worker_owner_case(0);
    }
    #[test]
    fn fix_round_one_unwrapped_parser_panic_retains_separate_completion_owner() {
        joined_worker_owner_case(1);
    }
    #[test]
    fn fix_round_one_unwrapped_parser_panic_retains_separate_sibling_owner() {
        joined_worker_owner_case(2);
    }
    fn joined_worker_owner_case(mode: u8) {
        use super::super::{Drain, archive_worker_precedence, finish_archive_workers};
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Payload(Arc<AtomicBool>);
        impl Drop for Payload {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let fixture = PendingFixture::new();
        let failure = fixture.failure.clone();
        let mut cleanup = CleanupFailures::default();
        cleanup.retain(&failure.clone().into());
        let dropped = Arc::new(AtomicBool::new(false));
        let payload = Payload(dropped.clone());
        let mut completion = archive_worker_precedence::complete();
        let parser_cleanup = if mode == 0 {
            Some(cleanup)
        } else {
            if mode == 1 {
                completion.cleanup = cleanup;
            }
            None
        };
        let sibling_owner = (mode == 2).then(|| failure.clone());
        let parser = std::thread::spawn(move || -> anyhow::Result<crate::snapshot::Snapshot> {
            if let Some(cleanup) = parser_cleanup {
                cleanup.resume_unwind(Box::new(payload));
            }
            std::panic::resume_unwind(Box::new(payload));
        });
        let stderr = std::thread::spawn(move || -> anyhow::Result<Drain> {
            Err(sibling_owner.map_or_else(|| anyhow::anyhow!("stderr write failed"), Into::into))
        });
        let error = finish_archive_workers(completion, parser.join(), stderr.join()).unwrap_err();
        let lost_at_aggregation = dropped.load(Ordering::SeqCst);
        let mut retained = CleanupFailures::default();
        retained.retain(&error);
        let pending = retained.pending();
        drop(error);
        let lost_at_lowering = dropped.load(Ordering::SeqCst);
        // Rescue the owned live fixture even when the pre-fix assertion fails.
        fixture.release_fault();
        while !failure.retry_cleanup().unwrap() {
            assert!(Instant::now() < failure.0.deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        retained.finish().unwrap();
        assert!(
            pending,
            "poison must not advertise completed native cleanup"
        );
        assert!(
            !lost_at_aggregation && !lost_at_lowering,
            "original panic payload was discarded while native cleanup authority was still pending"
        );
        assert!(
            dropped.load(Ordering::SeqCst),
            "payload must be released after its final owner is discharged"
        );
    }
}

#[cfg(test)]
mod worker_snapshot_tests {
    use super::*;

    fn completed(launch: LaunchPhase) -> AcquisitionFailure {
        AcquisitionFailure::from_spawn(
            SpawnFailure {
                launch,
                cleanup: HostCleanup::Complete,
                error: io::Error::other("historical acquisition failure"),
                cleanup_errors: vec![],
            },
            Instant::now(),
        )
    }

    #[test]
    fn strongest_historical_phase_survives_complete_records_and_io_payloads() {
        let original = anyhow::Error::new(io::Error::other(completed(LaunchPhase::MayRun)))
            .context(completed(LaunchPhase::CreatedSuspended));
        let (retained, facts) = CleanupFailures::capture_on_worker(&original);
        assert_eq!(facts.launch, Some(LaunchPhase::MayRun));
        assert!(!facts.pending);
        assert!(!retained.pending());
    }

    #[test]
    fn explicit_never_created_is_distinct_from_no_record() {
        let (_, facts) =
            CleanupFailures::capture_on_worker(&completed(LaunchPhase::NeverCreated).into());
        assert_eq!(facts.launch, Some(LaunchPhase::NeverCreated));
        let (_, absent) =
            CleanupFailures::capture_on_worker(&anyhow::anyhow!("ordinary recipe error"));
        assert_eq!(absent.launch, None);
    }
}
