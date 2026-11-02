//! Single owned query worker; completion collection is distinct from thread join.
use super::{
    framing::SlotId, owned_error::OwnedError, roots::Roots, status_projection::SavedProjection,
    types::*,
};
use crate::{engine::CancellationToken, recipe::ValidatedRecipe};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QueryDestination {
    Request(SlotId),
    Admission(Handle),
}
#[derive(Debug)]
pub(crate) struct QueryJob {
    pub generation: u64,
    pub destination: QueryDestination,
    pub cancel: CancellationToken,
    pub work: QueryWork,
}
#[derive(Debug)]
pub(crate) enum QueryWork {
    Inspect {
        args: InspectArgs,
        reply: ReplyContext,
    },
    ValidateStart(StartArgs),
    RefreshStatus {
        binding: SavedBinding,
        sampled_hint: u64,
    },
    Evidence {
        binding: SavedBinding,
        args: EvidenceArgs,
        sampled_hint: u64,
        reply: ReplyContext,
    },
}
#[derive(Debug)]
pub(crate) enum QueryValue {
    Inspect(InspectData),
    Validated(ValidatedRecipe),
    Refreshed(SavedProjection),
    Evidence(EvidenceData),
}
#[derive(Debug)]
pub(crate) struct QueryCompletion {
    pub generation: u64,
    pub destination: QueryDestination,
    pub worker_panicked: bool,
    pub result: Result<QueryValue, OwnedError>,
}
enum Command {
    Run(Box<QueryJob>),
    Stop,
}
struct WorkerOwner {
    commands: SyncSender<Command>,
    completions: Receiver<QueryCompletion>,
    thread: Option<JoinHandle<()>>,
    stopping: Arc<AtomicBool>,
    active: Option<CancellationToken>,
    pending: Option<QueryCompletion>,
    last_generation: u64,
}
pub(crate) struct QueryWorker {
    owner: Option<Box<WorkerOwner>>,
}
impl QueryWorker {
    pub(crate) fn spawn(roots: Arc<Roots>, max_run_seconds: u64) -> Result<Self, QueryError> {
        Self::start(move |work, cancel| execute(&roots, max_run_seconds, work, cancel))
    }
    fn start(
        mut executor: impl FnMut(QueryWork, &CancellationToken) -> Result<QueryValue, QueryError>
        + Send
        + 'static,
    ) -> Result<Self, QueryError> {
        let (commands, incoming) = mpsc::sync_channel(1);
        let (outgoing, completions) = mpsc::sync_channel(1);
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stop = stopping.clone();
        let thread = thread::Builder::new()
            .name("theseus-query".into())
            .spawn(move || {
                while let Ok(command) = incoming.recv() {
                    let Command::Run(job) = command else {
                        break;
                    };
                    let QueryJob {
                        generation,
                        destination,
                        cancel,
                        work,
                    } = *job;
                    let result = catch_unwind(AssertUnwindSafe(|| executor(work, &cancel)));
                    // Capture the actual source before formatting, transferring or
                    // suppressing anything, even when cancellation raced the query.
                    let (worker_panicked, result) = match result {
                        Ok(result) => (false, result.map_err(OwnedError::capture)),
                        Err(payload) => (
                            true,
                            Err(OwnedError::capture(QueryError::new(
                                ErrorCode::WorkerError,
                                crate::runner::process::worker_panic(payload),
                            ))),
                        ),
                    };
                    // Capturing the original source can itself catch a Display
                    // or source-chain panic. Preserve that owner and make the
                    // completion fatal before either publishing or dispatching
                    // another query, just as for an executor panic.
                    let worker_panicked = worker_panicked
                        || result
                            .as_ref()
                            .err()
                            .is_some_and(OwnedError::has_worker_panic);
                    if worker_panicked {
                        worker_stop.store(true, Ordering::SeqCst);
                    }
                    let completion = QueryCompletion {
                        generation,
                        destination,
                        worker_panicked,
                        result,
                    };
                    if let Err(unsent) = outgoing.send(completion) {
                        // Supported callers retain the receiver through actual join.
                        // An erroneous disconnected consumer must not destroy native
                        // authority or panic payloads on this fallback path.
                        let _ = Box::leak(Box::new(unsent.0));
                        break;
                    }
                    if worker_stop.load(Ordering::SeqCst) {
                        break;
                    }
                }
            })
            .map_err(|e| QueryError::new(ErrorCode::WorkerError, e.into()))?;
        Ok(Self {
            owner: Some(Box::new(WorkerOwner {
                commands,
                completions,
                thread: Some(thread),
                stopping,
                active: None,
                pending: None,
                last_generation: 0,
            })),
        })
    }
    // The agreed handoff returns the exact owned job on every rejection.
    #[allow(clippy::result_large_err)]
    pub(crate) fn try_submit(&mut self, job: QueryJob) -> Result<(), QueryJob> {
        let owner = self.owner.as_mut().expect("query owner retained");
        if owner.stopping.load(Ordering::SeqCst)
            || owner.active.is_some()
            || job.generation <= owner.last_generation
            || owner.thread.as_ref().is_none_or(JoinHandle::is_finished)
        {
            return Err(job);
        }
        let generation = job.generation;
        let cancel = job.cancel.clone();
        match owner.commands.try_send(Command::Run(Box::new(job))) {
            Ok(()) => {
                owner.active = Some(cancel);
                owner.last_generation = generation;
                Ok(())
            }
            Err(TrySendError::Full(Command::Run(job))) => Err(*job),
            Err(TrySendError::Disconnected(Command::Run(job))) => {
                owner.stopping.store(true, Ordering::SeqCst);
                Err(*job)
            }
            Err(_) => unreachable!("only a job was submitted"),
        }
    }
    pub(crate) fn try_complete(&mut self) -> Option<QueryCompletion> {
        let owner = self.owner.as_mut().expect("query owner retained");
        let completion = owner
            .pending
            .take()
            .or_else(|| owner.completions.try_recv().ok())?;
        owner.active = None;
        Some(completion)
    }
    pub(crate) fn is_busy(&self) -> bool {
        self.owner
            .as_ref()
            .is_some_and(|owner| owner.active.is_some())
    }
    pub(crate) fn request_stop(&mut self) {
        let owner = self.owner.as_mut().expect("query owner retained");
        owner.stopping.store(true, Ordering::SeqCst);
        if let Some(cancel) = &owner.active {
            cancel.cancel();
        }
        // If the sole slot still contains a job, its token and shared stop flag
        // are sufficient. Otherwise wake an idle receiver without blocking.
        let _ = owner.commands.try_send(Command::Stop);
    }
    pub(crate) fn try_join(&mut self) -> Option<Result<(), OwnedError>> {
        let owner = self.owner.as_mut().expect("query owner retained");
        let thread = owner.thread.as_ref()?;
        if !thread.is_finished() || owner.pending.is_some() {
            return None;
        }
        if owner.active.is_some() {
            match owner.completions.try_recv() {
                Ok(completion) => {
                    owner.pending = Some(completion);
                    return None;
                }
                Err(TryRecvError::Empty) => return None,
                // No completion was produced by an uncaught outer panic. Only
                // actual join below authorizes capturing that panic payload.
                Err(TryRecvError::Disconnected) => (),
            }
        }
        let missing_completion = owner.active.is_some();
        let joined = owner
            .thread
            .take()
            .expect("finished thread retained")
            .join();
        owner.stopping.store(true, Ordering::SeqCst);
        owner.active = None;
        Some(match joined {
            Err(payload) => Err(OwnedError::capture(QueryError::new(
                ErrorCode::WorkerError,
                crate::runner::process::worker_panic(payload),
            ))),
            Ok(()) if missing_completion => Err(OwnedError::capture(QueryError::new(
                ErrorCode::WorkerError,
                anyhow::anyhow!("query worker exited without its active completion"),
            ))),
            Ok(()) => Ok(()),
        })
    }
    #[cfg(any(test, feature = "mcp-test-support"))]
    pub(super) fn spawn_executor(
        executor: impl FnMut(QueryWork, &CancellationToken) -> Result<QueryValue, QueryError>
        + Send
        + 'static,
    ) -> Result<Self, QueryError> {
        Self::start(executor)
    }
}
impl Drop for QueryWorker {
    fn drop(&mut self) {
        if self.owner.as_ref().is_some_and(|owner| {
            owner.thread.is_some() || owner.active.is_some() || owner.pending.is_some()
        }) && let Some(owner) = self.owner.take()
        {
            // Memory-safety backstop only. Supported runtime retains this
            // entire owner until collection/join or binary process exit.
            let _ = Box::leak(owner);
        }
    }
}
fn cancelled(cancel: &CancellationToken) -> Result<(), QueryError> {
    if cancel.is_cancelled() {
        Err(super::roots::error(ErrorCode::CancelledBeforeStart))
    } else {
        Ok(())
    }
}
fn bound_store(
    roots: &Roots,
    binding: &SavedBinding,
) -> Result<crate::report::ReportStore, QueryError> {
    use theseus_host::fs::RelativePath;
    if binding.report_id.is_empty()
        || binding.report_id.contains(['/', '\\'])
        || RelativePath::parse(&binding.report_id).ok().as_ref() != Some(&binding.child)
    {
        return Err(super::roots::error(ErrorCode::InternalError));
    }
    // report_id is usable only after proving it is the exact trusted child.
    roots
        .output
        .open_child(&binding.report_id)
        .map_err(|e| QueryError::new(ErrorCode::IoError, e))
}
fn execute(
    roots: &Roots,
    max_run_seconds: u64,
    work: QueryWork,
    cancel: &CancellationToken,
) -> Result<QueryValue, QueryError> {
    cancelled(cancel)?;
    match work {
        QueryWork::Inspect { args, reply } => {
            super::inspection::inspect(roots, &args, max_run_seconds, &reply)
                .map(QueryValue::Inspect)
        }
        QueryWork::ValidateStart(args) => {
            ToolCall::RunStart(args.clone())
                .validate()
                .map_err(|_| super::roots::error(ErrorCode::InvalidArguments))?;
            let validated = super::roots::read_recipe(roots, &args.recipe_path)?;
            if validated.digest != args.recipe_digest.as_str() {
                return Err(super::roots::error(ErrorCode::RecipeChanged));
            }
            cancelled(cancel)?;
            validated
                .validate_selection(&args.profiles, &args.scenarios)
                .map_err(|e| QueryError::new(ErrorCode::RecipeInvalid, e))?;
            Ok(QueryValue::Validated(validated))
        }
        QueryWork::RefreshStatus {
            binding,
            sampled_hint,
        } => {
            let store = bound_store(roots, &binding)?;
            cancelled(cancel)?;
            let snapshot = super::evidence::load_snapshot(&store, sampled_hint)?;
            super::status_projection::project_snapshot(&snapshot).map(QueryValue::Refreshed)
        }
        QueryWork::Evidence {
            binding,
            args,
            sampled_hint,
            reply,
        } => {
            if args.handle != binding.handle {
                return Err(super::roots::error(ErrorCode::UnknownRun));
            }
            if args.server_instance != reply.instance
                || binding
                    .handle
                    .as_str()
                    .split_once(':')
                    .map(|(instance, _)| instance)
                    != Some(reply.instance.as_str())
            {
                return Err(super::roots::error(ErrorCode::StaleInstance));
            }
            ToolCall::RunEvidence(args.clone())
                .validate()
                .map_err(|_| super::roots::error(ErrorCode::InvalidArguments))?;
            let store = bound_store(roots, &binding)?;
            cancelled(cancel)?;
            let snapshot = super::evidence::load_snapshot(&store, sampled_hint)?;
            cancelled(cancel)?;
            super::evidence::evidence(&store, &snapshot, &args, &reply).map(QueryValue::Evidence)
        }
    }
}
#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
