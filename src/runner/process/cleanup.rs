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
impl CleanupFailures {
    pub(crate) fn retain(&mut self, error: &anyhow::Error) {
        for source in error.chain() {
            if let Some(failure) = source.downcast_ref::<AcquisitionFailure>() {
                self.push(failure);
            }
            if let Some(panic) = source.downcast_ref::<PanicFailure>() {
                self.push_panic(panic);
            }
            if let Some(failures) = source.downcast_ref::<Self>() {
                for failure in &failures.owners {
                    self.push(failure);
                }
                for panic in &failures.panics {
                    self.push_panic(panic);
                }
            }
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
        if panic.0.cleanup.pending()
            && !self
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
    fn settled(self) -> Self {
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
            let cause = original
                .downcast_ref::<&str>()
                .map(|cause| (*cause).to_string())
                .or_else(|| original.downcast_ref::<String>().cloned())
                .or_else(|| {
                    original
                        .downcast_ref::<PanicFailure>()
                        .map(|panic| panic.0.cause.clone())
                })
                .unwrap_or_else(|| "non-string panic payload retained".into());
            std::panic::resume_unwind(Box::new(PanicFailure(Arc::new(CleanupPanic {
                cause,
                _original: Mutex::new(original),
                cleanup,
            }))));
        }
        std::panic::resume_unwind(original)
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
impl fmt::Display for PanicFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "engine worker panicked: {}; {}",
            self.0.cause, self.0.cleanup
        )
    }
}
impl std::error::Error for PanicFailure {}
pub(crate) fn worker_panic(payload: Box<dyn std::any::Any + Send>) -> anyhow::Error {
    match payload.downcast::<PanicFailure>() {
        Ok(owner) => anyhow::Error::new(*owner),
        Err(_) => anyhow::anyhow!("engine worker panicked; inspect recovery journal"),
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
mod io_error_boundary_tests {
    use super::*;
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
        use super::super::{Drain, HostProcess, archive_worker_precedence, finish_archive_workers};
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Payload(Arc<AtomicBool>);
        impl Drop for Payload {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let ready = temp.path().join("ready");
        let heartbeat = temp.path().join("heartbeat");
        let mut command = super::super::test_support::fixture("ready-hold", &[]);
        command.arg(&ready).arg(&heartbeat).arg("30000");
        let mut child = HostProcess::spawn(&command).unwrap();
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
        let mut cleanup = CleanupFailures::default();
        cleanup.retain(&failure.clone().into());
        let dropped = Arc::new(AtomicBool::new(false));
        let payload = Payload(dropped.clone());
        let parser = std::thread::spawn(move || -> anyhow::Result<crate::snapshot::Snapshot> {
            cleanup.resume_unwind(Box::new(payload));
        });
        let stderr = std::thread::spawn(|| -> anyhow::Result<Drain> {
            Err(anyhow::anyhow!("stderr write failed"))
        });
        let error = finish_archive_workers(
            archive_worker_precedence::complete(),
            parser.join(),
            stderr.join(),
        )
        .unwrap_err();
        let lost_at_aggregation = dropped.load(Ordering::SeqCst);
        let mut retained = CleanupFailures::default();
        retained.retain(&error);
        let pending = retained.pending();
        drop(error);
        let lost_at_lowering = dropped.load(Ordering::SeqCst);
        // Rescue the owned live fixture even when the pre-fix assertion fails.
        failure.0.cleanup.clear_poison();
        while !failure.retry_cleanup().unwrap() {
            assert!(Instant::now() < deadline);
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
