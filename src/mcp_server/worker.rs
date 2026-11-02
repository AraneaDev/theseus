//! Owned shared-engine and cleanup-finalizer threads.
use super::{
    owned_error::{EngineCompletion, OwnedError},
    types::{ErrorCode, FinalSummary, Handle, QueryError, SavedBinding, UINT_MAX},
};
use crate::{
    engine::{self, CancellationToken, EngineEvent, EventKind, RunOptions, RunResult},
    recipe::ValidatedRecipe,
    report::ReportStore,
    runner::{
        Docker, Environment,
        process::{CleanupFailures, worker_panic},
    },
};
use std::{
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
};
use theseus_host::fs::RelativePath;

#[derive(Debug)]
pub(crate) enum WorkerCompletion {
    Engine(EngineCompletion),
    Finalizer { owners: Vec<OwnedError> },
}
#[derive(Debug)]
pub(crate) struct FinalizerStartError {
    pub source: QueryError,
    pub owners: Vec<OwnedError>,
}
#[derive(Default)]
struct Bindings {
    first: Option<SavedBinding>,
    latest: Option<SavedBinding>,
}
struct WorkerOwner {
    thread: Option<JoinHandle<WorkerCompletion>>,
    bindings: Arc<Mutex<Bindings>>,
    handle: Option<Handle>,
    finalizer_handoff: Option<Arc<Mutex<Option<Vec<OwnedError>>>>>,
}
pub(crate) struct EngineWorker {
    owner: Option<Box<WorkerOwner>>,
}
impl EngineWorker {
    pub(crate) fn spawn(
        handle: Handle,
        recipe: ValidatedRecipe,
        options: RunOptions,
        token: CancellationToken,
        output: Arc<ReportStore>,
    ) -> Result<Self, QueryError> {
        Self::start(handle, recipe, options, token, output, Docker::default)
    }

    fn start<E: Environment + 'static>(
        handle: Handle,
        recipe: ValidatedRecipe,
        mut options: RunOptions,
        token: CancellationToken,
        output: Arc<ReportStore>,
        factory: impl FnOnce() -> E + Send + 'static,
    ) -> Result<Self, QueryError> {
        // The pinned parent decides both creation authority and displayed paths.
        options.output_root = output.root().to_path_buf();
        let bindings = Arc::new(Mutex::new(Bindings::default()));
        let worker_bindings = bindings.clone();
        let worker_handle = handle.clone();
        let thread = thread::Builder::new()
            .name("theseus-engine".into())
            .spawn(move || {
                let mut observer = Observer {
                    handle: worker_handle.clone(),
                    output,
                    bindings: worker_bindings,
                    binding: None,
                    sequence: 0,
                };
                let result = execute(recipe, options, token, factory, &mut observer);
                WorkerCompletion::Engine(EngineCompletion {
                    handle: worker_handle,
                    result,
                })
            })
            .map_err(|e| failure(e.into()))?;
        Ok(Self {
            owner: Some(Box::new(WorkerOwner {
                thread: Some(thread),
                bindings,
                handle: Some(handle),
                finalizer_handoff: None,
            })),
        })
    }

    pub(crate) fn try_binding(&mut self) -> Option<SavedBinding> {
        let owner = self.owner.as_ref()?;
        let mut bindings = match owner.bindings.try_lock() {
            Ok(bindings) => bindings,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        bindings.first.take().or_else(|| bindings.latest.take())
    }

    pub(crate) fn try_join(&mut self) -> Option<WorkerCompletion> {
        let owner = self.owner.as_mut()?;
        if !owner.thread.as_ref()?.is_finished() {
            return None;
        }
        let joined = owner
            .thread
            .take()
            .expect("finished thread retained")
            .join();
        Some(match joined {
            Ok(completion) => completion,
            Err(payload) => {
                // Only an actual join authorizes capture on this caller.
                let error = OwnedError::capture(failure(worker_panic(payload)));
                match &owner.handle {
                    Some(handle) => WorkerCompletion::Engine(EngineCompletion {
                        handle: handle.clone(),
                        result: Err(error),
                    }),
                    None => {
                        let mut owners = owner
                            .finalizer_handoff
                            .as_ref()
                            .and_then(|cell| cell.lock().unwrap_or_else(|e| e.into_inner()).take())
                            .unwrap_or_default();
                        owners.push(error);
                        WorkerCompletion::Finalizer { owners }
                    }
                }
            }
        })
    }

    pub(crate) fn spawn_finalizer(owners: Vec<OwnedError>) -> Result<Self, FinalizerStartError> {
        Self::start_finalizer(owners, |task| {
            thread::Builder::new()
                .name("theseus-finalizer".into())
                .spawn(task)
        })
    }

    fn start_finalizer(
        owners: Vec<OwnedError>,
        spawn: impl FnOnce(
            Box<dyn FnOnce() -> WorkerCompletion + Send>,
        ) -> io::Result<JoinHandle<WorkerCompletion>>,
    ) -> Result<Self, FinalizerStartError> {
        // Builder::spawn drops its closure on failure. Keep the actual owners in
        // a shared handoff until the spawned worker takes them, outside the lock.
        let handoff = Arc::new(Mutex::new(Some(owners)));
        let worker_handoff = handoff.clone();
        let thread = spawn(Box::new(move || {
            let owners = worker_handoff
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .expect("finalizer handoff taken once");
            let owners = owners.into_iter().map(OwnedError::finalize).collect();
            WorkerCompletion::Finalizer { owners }
        }))
        .map_err(|error| FinalizerStartError {
            source: failure(error.into()),
            owners: handoff
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .expect("failed spawn retains all owners"),
        })?;
        Ok(Self {
            owner: Some(Box::new(WorkerOwner {
                thread: Some(thread),
                bindings: Arc::new(Mutex::new(Bindings::default())),
                handle: None,
                finalizer_handoff: Some(handoff),
            })),
        })
    }

    #[cfg(test)]
    fn spawn_with_environment<E: Environment + 'static>(
        handle: Handle,
        recipe: ValidatedRecipe,
        options: RunOptions,
        token: CancellationToken,
        output: Arc<ReportStore>,
        factory: impl FnOnce() -> E + Send + 'static,
    ) -> Result<Self, QueryError> {
        Self::start(handle, recipe, options, token, output, factory)
    }
}
impl Drop for EngineWorker {
    fn drop(&mut self) {
        if self
            .owner
            .as_ref()
            .is_some_and(|owner| owner.thread.is_some())
            && let Some(owner) = self.owner.take()
        {
            // Caller misuse safety backstop. Normal facade shutdown retains this
            // owner through actual join or the binary-only process-exit boundary.
            let _ = Box::leak(owner);
        }
    }
}
fn failure(source: anyhow::Error) -> QueryError {
    QueryError::new(ErrorCode::WorkerError, source)
}

struct Observer {
    handle: Handle,
    output: Arc<ReportStore>,
    bindings: Arc<Mutex<Bindings>>,
    binding: Option<SavedBinding>,
    sequence: u64,
}
impl Observer {
    fn emit(&mut self, event: EngineEvent) {
        let next = self
            .sequence
            .checked_add(1)
            .filter(|n| *n <= UINT_MAX)
            .expect("saved hint sequence exhausted");
        let first = self.binding.is_none();
        let mut binding = if first {
            assert_eq!(
                event.kind,
                EventKind::Started,
                "first saved event must be Started"
            );
            let relative = event
                .report_path
                .strip_prefix(self.output.root())
                .expect("engine report is inside pinned output");
            let mut components = relative.components();
            let report_id = components
                .next()
                .and_then(|c| c.as_os_str().to_str())
                .expect("engine child name is UTF-8");
            assert_eq!(
                components.next().map(|c| c.as_os_str()),
                Some(std::ffi::OsStr::new("report.json"))
            );
            assert!(components.next().is_none(), "exact saved child report path");
            SavedBinding {
                handle: self.handle.clone(),
                report_id: report_id.into(),
                child: RelativePath::parse(report_id)
                    .expect("engine child is a relative component"),
                hint_sequence: next,
            }
        } else {
            self.binding
                .as_ref()
                .expect("Started binding retained")
                .clone()
        };
        binding.hint_sequence = next;
        self.sequence = next;
        self.binding = Some(binding.clone());
        // No filesystem, serialization, native cleanup or fallible projection
        // under this short mailbox lock. The first binding cannot be overwritten.
        let mut mailbox = self.bindings.lock().unwrap_or_else(|e| e.into_inner());
        if first {
            mailbox.first = Some(binding);
        } else {
            mailbox.latest = Some(binding);
        }
    }
}

fn execute<E: Environment>(
    recipe: ValidatedRecipe,
    options: RunOptions,
    token: CancellationToken,
    factory: impl FnOnce() -> E,
    observer: &mut Observer,
) -> Result<FinalSummary, OwnedError> {
    // Keep the actual engine result outside the Environment's scope. Both a run
    // panic and a later destructor panic can carry independent native authority.
    let mut result: Option<anyhow::Result<RunResult>> = None;
    let scope = catch_unwind(AssertUnwindSafe(|| {
        let mut environment = factory();
        let parent = observer.output.clone();
        result = Some(
            match catch_unwind(AssertUnwindSafe(|| {
                engine::run_with_parent(
                    &recipe,
                    options,
                    token,
                    &mut environment,
                    Some(&parent),
                    |event| observer.emit(event),
                )
            })) {
                Ok(result) => result,
                Err(payload) => Err(worker_panic(payload)),
            },
        );
        // environment drops here, while result remains owned by the outer scope.
    }));
    if let Err(payload) = scope {
        let secondary = worker_panic(payload);
        result = Some(Err(match result.take() {
            Some(Err(primary)) => retain_secondary(primary, secondary),
            _ => secondary,
        }));
    }
    let result = result.expect("factory either returned or its panic was retained");
    match result {
        Err(error) => Err(OwnedError::capture(failure(error))),
        Ok(run) => {
            // Borrow the full report during projection so unwind cannot consume
            // the source. Drop it only after the bounded final summary exists.
            let projection = catch_unwind(AssertUnwindSafe(|| FinalSummary {
                outcome: run.report.outcome,
                counts: run.report.counts(),
                report_path: run.report_path.clone(),
                journal_path: run.report_path.with_file_name("journal.json"),
            }));
            match projection {
                Ok(summary) => Ok(summary),
                Err(payload) => Err(OwnedError::capture(failure(worker_panic(payload)))),
            }
        }
    }
}
fn retain_secondary(primary: anyhow::Error, secondary: anyhow::Error) -> anyhow::Error {
    let mut retained = CleanupFailures::default();
    retained.retain(&secondary);
    primary.context(retained)
}

#[cfg(test)]
#[path = "worker_tests/mod.rs"]
mod tests;
