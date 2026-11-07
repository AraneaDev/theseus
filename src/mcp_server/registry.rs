//! Bounded process-local admission and immutable lifecycle projections.
use super::{
    budget::{PageCandidate, fit_page},
    cursor::CursorRecord,
    owned_error::{EngineCompletion, OwnedError},
    status_projection::{self, SavedProjection},
    types::*,
};
use crate::{
    engine::{CancellationToken, RunOptions},
    model::{ProfileName, ScenarioKind},
    recipe::ValidatedRecipe,
};
use std::{
    fmt::{self, Write},
    path::Path,
};

#[derive(Debug)]
pub(crate) enum AdmissionDecision {
    Replay(StartData),
    Reserved {
        handle: Handle,
        validation: StartArgs,
    },
}
#[derive(Debug)]
pub(crate) struct ReadyNativeRun {
    pub handle: Handle,
    pub recipe: crate::engine::domain::SelectedRecipe,
    pub options: crate::engine::domain::NativeRunOptions,
    pub token: CancellationToken,
}
#[derive(Debug)]
pub(crate) struct ReadyRun {
    pub handle: Handle,
    pub recipe: ValidatedRecipe,
    pub options: RunOptions,
    pub token: CancellationToken,
}
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "The bounded ownership-transfer API returns recipes and original error owners directly"
)]
pub(crate) enum RegistryUpdate {
    ValidationStarted {
        generation: u64,
    },
    Validation {
        generation: u64,
        result: Result<ValidatedRecipe, OwnedError>,
    },
    NativeValidation {
        generation: u64,
        result: Result<crate::native::recipe::ValidatedNativeRecipe, OwnedError>,
    },
    IntegrationValidation {
        generation: u64,
        result: Result<crate::native::integration::recipe::ValidatedIntegrationRecipe, OwnedError>,
    },
    ValidationExpired {
        generation: u64,
        code: ErrorCode,
    },
    Dispatched,
    SpawnFailed(OwnedError),
    Bound(SavedBinding),
    Refreshed(SavedProjection),
    Joined(EngineCompletion),
}

/// The canonical typed tuple, with defaults already applied by StartArgs.
/// Equality retains array order and avoids a lossy Debug or JSON-key-order hash.
#[derive(Debug, PartialEq, Eq)]
struct Fingerprint {
    instance: Instance,
    path: String,
    digest: Digest,
    profiles: Vec<ProfileName>,
    scenarios: Vec<ScenarioKind>,
    pull: bool,
    log_limit: UInt,
}
impl From<&StartArgs> for Fingerprint {
    fn from(args: &StartArgs) -> Self {
        Self {
            instance: args.server_instance.clone(),
            path: args.recipe_path.clone(),
            digest: args.recipe_digest.clone(),
            profiles: args.profiles.clone(),
            scenarios: args.scenarios.clone(),
            pull: args.pull,
            log_limit: args.log_limit,
        }
    }
}
#[derive(Debug)]
struct Entry {
    key: Key,
    fingerprint: Fingerprint,
    token: CancellationToken,
    generation: Option<u64>,
    recipe: Option<ValidatedRecipe>,
    native_recipe: Option<crate::engine::domain::SelectedRecipe>,
    dispatch_taken: bool,
    execution_started: bool,
    binding: Option<SavedBinding>,
    saved_guest: Option<GuestCleanup>,
    owner: Option<OwnedError>,
    // Cached independently from owner so transferring it cannot erase authority.
    cleanup_blocked: bool,
    view: RunView,
}
impl Entry {
    fn new(key: Key, fingerprint: Fingerprint, handle: Handle, sequence: u8) -> Self {
        Self {
            key,
            fingerprint,
            token: CancellationToken::default(),
            generation: None,
            recipe: None,
            native_recipe: None,
            dispatch_taken: false,
            execution_started: false,
            binding: None,
            saved_guest: None,
            owner: None,
            cleanup_blocked: false,
            view: RunView {
                handle,
                admission_sequence: sequence,
                state: RunState::Validating,
                cancellation_requested: false,
                cancellation_reason: None,
                report_id: None,
                report_path: None,
                journal_path: None,
                saved: None,
                engine_result: None,
                saved_hint_sequence: UInt::new(0).expect("zero"),
                snapshot_hint_sequence: None,
                cleanup: status_projection::cleanup_view(
                    NativeCleanup::NotStarted,
                    None,
                    GuestCleanup::NotStarted,
                    false,
                    false,
                    false,
                ),
                error: None,
            },
        }
    }
    fn terminal(&self) -> bool {
        matches!(
            self.view.state,
            RunState::Finished | RunState::StartError | RunState::WorkerError
        )
    }
    fn start_data(&self) -> StartData {
        StartData {
            handle: self.view.handle.clone(),
            state: self.view.state,
            replayed: true,
            cancellation_requested: self.view.cancellation_requested,
            lifetime: Lifetime::ServerProcess,
            poll_after_ms: 500,
        }
    }
    fn cleanup(&mut self) {
        let guest = if self.view.state == RunState::Running {
            GuestCleanup::InProgress
        } else {
            self.saved_guest.unwrap_or(if self.execution_started {
                GuestCleanup::Unknown
            } else {
                GuestCleanup::NotStarted
            })
        };
        self.view.cleanup = status_projection::cleanup_view(
            self.view.cleanup.native,
            self.view.cleanup.launch,
            guest,
            matches!(
                self.view.state,
                RunState::StartError | RunState::WorkerError
            ),
            self.view.state == RunState::WorkerError,
            self.binding.is_some(),
        );
    }
    fn fail(&mut self, state: RunState, error: FailureError) {
        self.recipe = None;
        self.generation = None;
        self.view.state = state;
        self.view.error = Some(error);
        self.cleanup();
    }
    fn own_error(&mut self, instance: &Instance, state: RunState, owner: OwnedError) {
        // Read only worker-captured immutable fields. Never inspect/settle native
        // owners, format the primary, or use native Complete as a join signal.
        let facts = owner.view();
        let error = Failure::new(
            instance.clone(),
            facts.code,
            facts.message.clone(),
            Some(self.view.handle.clone()),
        )
        .error;
        self.cleanup_blocked = facts.cleanup_blocked;
        self.view.cleanup.native = facts.native;
        self.view.cleanup.launch = facts.launch;
        self.owner = Some(owner);
        self.fail(state, error);
    }
}

#[derive(Debug)]
pub(crate) struct Registry {
    instance: Instance,
    entries: Vec<Entry>,
    global_cleanup_blocked: bool,
}
impl Registry {
    pub(crate) fn new(instance: Instance) -> Self {
        Self {
            instance,
            entries: Vec::with_capacity(128),
            global_cleanup_blocked: false,
        }
    }
    fn failure(&self, code: ErrorCode, handle: Option<&Handle>) -> Failure {
        Failure::new(
            self.instance.clone(),
            code,
            TextPreview::new("Registry request rejected", false),
            handle.cloned(),
        )
    }
    fn index(&self, handle: &Handle) -> Result<usize, Failure> {
        let (instance, sequence) = handle.as_str().split_once(':').expect("typed handle");
        if instance != self.instance.as_str() {
            return Err(self.failure(ErrorCode::InvalidArguments, Some(handle)));
        }
        let index = sequence.parse::<usize>().expect("typed sequence") - 1;
        if index >= self.entries.len() {
            return Err(self.failure(ErrorCode::UnknownRun, Some(handle)));
        }
        Ok(index)
    }
    pub(crate) fn admit(
        &mut self,
        args: StartArgs,
        query_available: bool,
    ) -> Result<AdmissionDecision, Failure> {
        ToolCall::RunStart(args.clone())
            .validate()
            .map_err(|_| self.failure(ErrorCode::InvalidArguments, None))?;
        if args.server_instance != self.instance {
            return Err(self.failure(ErrorCode::StaleInstance, None));
        }
        let fingerprint = Fingerprint::from(&args);
        if let Some(entry) = self.entries.iter().find(|e| e.key == args.request_key) {
            return if entry.fingerprint == fingerprint {
                Ok(AdmissionDecision::Replay(entry.start_data()))
            } else {
                Err(self.failure(ErrorCode::IdempotencyConflict, None))
            };
        }
        let rejected =
            if self.global_cleanup_blocked || self.entries.iter().any(|e| e.cleanup_blocked) {
                Some(ErrorCode::CleanupPending)
            } else if self.entries.len() == 128 {
                Some(ErrorCode::RegistryFull)
            } else if self.entries.iter().any(|e| !e.terminal()) {
                Some(ErrorCode::Busy)
            } else if !query_available {
                Some(ErrorCode::QueryBusy)
            } else {
                None
            };
        if let Some(code) = rejected {
            return Err(self.failure(code, None));
        }
        let sequence = u8::try_from(self.entries.len() + 1).expect("bounded admission");
        let handle = Handle::parse(&format!("{}:{sequence}", self.instance.as_str()))
            .expect("bounded handle");
        self.entries.push(Entry::new(
            args.request_key.clone(),
            fingerprint,
            handle.clone(),
            sequence,
        ));
        // Coordinator submits this exact validation job in the same exclusive
        // turn; this registry never owns a second worker or execution queue.
        Ok(AdmissionDecision::Reserved {
            handle,
            validation: args,
        })
    }
    pub(crate) fn cancel(
        &mut self,
        handle: &Handle,
        reason: CancellationReason,
    ) -> Result<CancelData, Failure> {
        let index = self.index(handle)?;
        let cancellation_error = self
            .failure(ErrorCode::CancelledBeforeStart, Some(handle))
            .error;
        let entry = &mut self.entries[index];
        let requested_now = !entry.terminal() && !entry.view.cancellation_requested;
        if requested_now {
            entry.token.cancel();
            entry.view.cancellation_requested = true;
            entry.view.cancellation_reason = Some(reason);
            if matches!(entry.view.state, RunState::Validating | RunState::Admitted) {
                entry.fail(RunState::StartError, cancellation_error);
            }
        }
        Ok(CancelData {
            run: entry.view.clone(),
            requested_now,
        })
    }
    pub(crate) fn cached(&self, handle: &Handle) -> Result<&RunView, Failure> {
        Ok(&self.entries[self.index(handle)?].view)
    }
    /// Actual saved-child authority, separate from display-only RunView paths.
    pub(crate) fn binding(&self, handle: &Handle) -> Result<Option<&SavedBinding>, Failure> {
        Ok(self.entries[self.index(handle)?].binding.as_ref())
    }
    pub(crate) fn list(&self, args: &ListArgs, reply: &ReplyContext) -> Result<ListData, Failure> {
        ToolCall::RunList(args.clone())
            .validate()
            .map_err(|_| self.failure(ErrorCode::InvalidArguments, None))?;
        if args.server_instance != self.instance || reply.instance != self.instance {
            return Err(self.failure(ErrorCode::StaleInstance, None));
        }
        let mut cursor = if let Some(encoded) = &args.cursor {
            let cursor =
                CursorRecord::decode(encoded, &self.instance).map_err(|c| self.failure(c, None))?;
            if usize::from(cursor.list_high_water) > self.entries.len() {
                return Err(self.failure(ErrorCode::InvalidCursor, None));
            }
            let expected = CursorRecord::list(args, cursor.list_high_water)
                .map_err(|c| self.failure(c, None))?;
            cursor
                .validate(&expected, u64::from(cursor.list_high_water))
                .map_err(|c| self.failure(c, None))?;
            cursor
        } else {
            CursorRecord::list(args, self.entries.len() as u8).map_err(|c| self.failure(c, None))?
        };
        let start = usize::try_from(cursor.next_position)
            .map_err(|_| self.failure(ErrorCode::InvalidCursor, None))?;
        let high_water = cursor.list_high_water;
        let end = (start + usize::from(cursor.requested_limit)).min(usize::from(high_water));
        let runs = self.entries[start..end]
            .iter()
            .map(|e| e.view.clone())
            .collect();
        cursor.next_position = start as u64;
        let data = ListData {
            runs,
            high_water,
            next_cursor: None,
        };
        match fit_page(
            reply,
            PageCandidate {
                data: ToolData::RunList(data),
                cursor,
                more_after_candidate: end < usize::from(high_water),
            },
        )? {
            ToolData::RunList(data) => Ok(data),
            _ => Err(self.failure(ErrorCode::InternalError, None)),
        }
    }
    /// Rejected updates are returned intact: the coordinator retains any original
    /// errors/panic/native owners even for cancelled or obsolete generations.
    #[allow(
        clippy::result_large_err,
        reason = "Rejected updates return the exact typed recipe/error ownership handoff"
    )]
    pub(crate) fn apply(
        &mut self,
        handle: &Handle,
        update: RegistryUpdate,
    ) -> Result<(), RegistryUpdate> {
        let Ok(index) = self.index(handle) else {
            return Err(update);
        };
        let entry = &mut self.entries[index];
        let valid = match &update {
            RegistryUpdate::ValidationStarted { .. } => {
                entry.view.state == RunState::Validating && entry.generation.is_none()
            }
            RegistryUpdate::Validation { generation, .. }
            | RegistryUpdate::NativeValidation { generation, .. }
            | RegistryUpdate::IntegrationValidation { generation, .. }
            | RegistryUpdate::ValidationExpired { generation, .. } => {
                entry.view.state == RunState::Validating && entry.generation == Some(*generation)
            }
            RegistryUpdate::Dispatched | RegistryUpdate::SpawnFailed(_) => {
                entry.view.state == RunState::Admitted
                    && entry.dispatch_taken
                    && !entry.token.is_cancelled()
            }
            RegistryUpdate::Bound(binding) => {
                entry.execution_started
                    && binding.handle == *handle
                    && UInt::new(binding.hint_sequence).is_ok()
                    && entry.binding.as_ref().is_none_or(|old| {
                        old.report_id == binding.report_id
                            && old.child == binding.child
                            && old.hint_sequence <= binding.hint_sequence
                    })
            }
            RegistryUpdate::Refreshed(projection) => {
                entry.binding.is_some()
                    && UInt::new(projection.sampled_hint).is_ok()
                    && projection.sampled_hint <= entry.view.saved_hint_sequence.get()
            }
            RegistryUpdate::Joined(completion) => {
                entry.view.state == RunState::Running
                    && completion.handle == *handle
                    && completion.result.as_ref().map_or(true, |summary| {
                        status_projection::counts(&summary.counts).is_ok()
                    })
            }
        };
        if !valid {
            return Err(update);
        }
        match update {
            RegistryUpdate::ValidationStarted { generation } => entry.generation = Some(generation),
            RegistryUpdate::Validation { result, .. } => {
                entry.generation = None;
                match result {
                    Ok(recipe) => {
                        entry.recipe = Some(recipe);
                        entry.view.state = RunState::Admitted;
                    }
                    Err(owner) => entry.own_error(&self.instance, RunState::StartError, owner),
                }
            }
            RegistryUpdate::NativeValidation { result, .. } => {
                entry.generation = None;
                match result {
                    Ok(recipe) => {
                        entry.native_recipe =
                            Some(crate::engine::domain::SelectedRecipe::Native(recipe));
                        entry.view.state = RunState::Admitted;
                    }
                    Err(owner) => entry.own_error(&self.instance, RunState::StartError, owner),
                }
            }
            RegistryUpdate::IntegrationValidation { result, .. } => {
                entry.generation = None;
                match result {
                    Ok(recipe) => {
                        entry.native_recipe = Some(
                            crate::engine::domain::SelectedRecipe::NativeIntegration(recipe),
                        );
                        entry.view.state = RunState::Admitted;
                    }
                    Err(owner) => entry.own_error(&self.instance, RunState::StartError, owner),
                }
            }
            RegistryUpdate::ValidationExpired { code, .. } => {
                let error = Failure::new(
                    self.instance.clone(),
                    code,
                    TextPreview::new("Validation ended before dispatch", false),
                    Some(handle.clone()),
                )
                .error;
                entry.fail(RunState::StartError, error);
            }
            RegistryUpdate::Dispatched => {
                entry.execution_started = true;
                entry.view.state = RunState::Running;
                entry.view.cleanup.native = NativeCleanup::InProgress;
                entry.cleanup();
            }
            RegistryUpdate::SpawnFailed(owner) => {
                entry.own_error(&self.instance, RunState::WorkerError, owner)
            }
            RegistryUpdate::Bound(binding) => {
                entry.view.report_id = Some(TextPreview::new(&binding.report_id, false));
                entry.view.saved_hint_sequence =
                    UInt::new(binding.hint_sequence).expect("checked hint");
                entry.binding = Some(binding);
                entry.cleanup();
            }
            RegistryUpdate::Refreshed(projection) => {
                entry.saved_guest = Some(projection.guest);
                entry.view.saved = Some(projection.saved);
                entry.view.snapshot_hint_sequence =
                    Some(UInt::new(projection.sampled_hint).expect("checked sampled hint"));
                entry.cleanup();
            }
            RegistryUpdate::Joined(completion) => match completion.result {
                Ok(summary) => {
                    entry.view.state = RunState::Finished;
                    entry.view.engine_result = Some(EngineResult {
                        outcome: summary.outcome,
                        counts: status_projection::counts(&summary.counts).expect("checked counts"),
                    });
                    entry.view.report_path = Some(path_preview(&summary.report_path));
                    entry.view.journal_path = Some(path_preview(&summary.journal_path));
                    entry.view.cleanup.native = NativeCleanup::Complete;
                    entry.cleanup();
                }
                Err(owner) => entry.own_error(&self.instance, RunState::WorkerError, owner),
            },
        }
        Ok(())
    }
    pub(crate) fn take_dispatch(&mut self, handle: &Handle) -> Option<ReadyRun> {
        let index = self.index(handle).ok()?;
        let entry = &mut self.entries[index];
        if entry.view.state != RunState::Admitted
            || entry.dispatch_taken
            || entry.token.is_cancelled()
        {
            return None;
        }
        let recipe = entry.recipe.take()?;
        let mut options = RunOptions::all(&recipe);
        options.profiles = entry.fingerprint.profiles.clone();
        options.scenarios = entry.fingerprint.scenarios.clone();
        options.pull = entry.fingerprint.pull;
        options.log_limit = entry.fingerprint.log_limit.get();
        entry.dispatch_taken = true;
        Some(ReadyRun {
            handle: handle.clone(),
            recipe,
            options,
            token: entry.token.clone(),
        })
    }
    pub(crate) fn take_native_dispatch(&mut self, handle: &Handle) -> Option<ReadyNativeRun> {
        let index = self.index(handle).ok()?;
        let entry = &mut self.entries[index];
        if entry.view.state != RunState::Admitted
            || entry.dispatch_taken
            || entry.token.is_cancelled()
        {
            return None;
        }
        let recipe = entry.native_recipe.take()?;
        let mut limits = match &recipe {
            crate::engine::domain::SelectedRecipe::Native(r) => r.recipe.limits.clone(),
            crate::engine::domain::SelectedRecipe::NativeIntegration(r) => {
                r.core.recipe.limits.clone()
            }
            _ => unreachable!(),
        };
        limits.output_bytes = limits.output_bytes.min(entry.fingerprint.log_limit.get());
        let options = crate::engine::domain::NativeRunOptions {
            output_root: std::path::PathBuf::new(),
            scenarios: entry.fingerprint.scenarios.clone(),
            limits,
        };
        entry.dispatch_taken = true;
        Some(ReadyNativeRun {
            handle: handle.clone(),
            recipe,
            options,
            token: entry.token.clone(),
        })
    }
    pub(crate) fn set_cleanup_blocked(&mut self, blocked: bool) {
        self.global_cleanup_blocked = blocked;
    }
    pub(crate) fn take_error_owners_for_shutdown(&mut self) -> Vec<OwnedError> {
        self.entries
            .iter_mut()
            .filter_map(|e| e.owner.take())
            .collect()
    }
}

/// Stream native path display through a fixed bound, without allocating a full
/// lossy conversion. Literal U+FFFD is lossless; native invalid encoding is not.
fn path_preview(path: &Path) -> TextPreview {
    struct Bounded(TextPreview);
    impl fmt::Write for Bounded {
        fn write_str(&mut self, value: &str) -> fmt::Result {
            let mut end = value.len().min(512 - self.0.text.len());
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            self.0.text.push_str(&value[..end]);
            if end < value.len() {
                self.0.truncated = true;
                Err(fmt::Error)
            } else {
                Ok(())
            }
        }
    }
    let mut result = Bounded(TextPreview {
        text: String::with_capacity(512),
        truncated: false,
        lossy: path.as_os_str().to_str().is_none(),
    });
    if write!(&mut result, "{}", path.display()).is_err() {
        result.0.truncated = true;
    }
    result.0
}
#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
