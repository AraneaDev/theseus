//! Query completion routing.
use super::super::{
    framing::SlotId,
    owned_error::OwnedError,
    query::{QueryCompletion, QueryDestination, QueryValue},
    registry::RegistryUpdate,
    types::*,
    worker::EngineWorker,
};
use super::{ActiveRun, ServerRuntime, Transport};
use std::time::{Duration, Instant};
impl<T: Transport> ServerRuntime<T> {
    pub(super) fn complete_query(&mut self, completion: QueryCompletion, now: Instant) {
        let panic = completion.worker_panicked
            || completion
                .result
                .as_ref()
                .err()
                .is_some_and(OwnedError::has_worker_panic);
        let active = self.active_query.take();
        let live = active.as_ref().is_some_and(|q| {
            q.live
                && q.generation == completion.generation
                && q.destination == completion.destination
        }) && !self.shutdown.active()
            && !panic;
        if !live {
            // The completed query has left active_query already. Preserve its
            // admission cancellation boundary before entering shutdown.
            if (panic || self.shutdown.active())
                && let Some(active) = &active
            {
                active.token.cancel();
                if let QueryDestination::Admission(handle) = &active.destination {
                    let _ = self
                        .registry
                        .cancel(handle, CancellationReason::ServerShutdown);
                }
            }
            match completion.result {
                Err(owner) if panic => {
                    self.owners.coordinator_owner(owner);
                    self.begin_shutdown(now, true);
                }
                Err(owner) => {
                    if owner.view().cleanup_blocked {
                        self.owners.push(owner);
                    }
                }
                Ok(_) if panic => self.fatal(now, "query worker panicked without an error owner"),
                Ok(_) => (),
            }
            return;
        }
        match completion.destination {
            QueryDestination::Admission(handle) => {
                self.complete_admission(handle, completion.generation, completion.result, now)
            }
            QueryDestination::Request(slot) => self.complete_request(slot, completion.result, now),
        }
    }
    fn complete_admission(
        &mut self,
        handle: Handle,
        generation: u64,
        result: Result<QueryValue, OwnedError>,
        now: Instant,
    ) {
        let update = match result {
            Ok(QueryValue::Validated(recipe)) => RegistryUpdate::Validation {
                generation,
                result: Ok(recipe),
            },
            Ok(QueryValue::ValidatedIntegration(recipe)) => RegistryUpdate::IntegrationValidation {
                generation,
                result: Ok(*recipe),
            },
            Ok(QueryValue::ValidatedNative(recipe)) => RegistryUpdate::NativeValidation {
                generation,
                result: Ok(recipe),
            },
            Err(owner) => RegistryUpdate::Validation {
                generation,
                result: Err(owner),
            },
            Ok(_) => {
                self.fatal(now, "invalid admission completion");
                return;
            }
        };
        self.apply(&handle, update);
        if let Some(run) = self.registry.take_native_dispatch(&handle) {
            if self.engine.is_some() {
                self.fatal(now, "engine slot already occupied");
                return;
            }
            let Some(roots) = self.roots.as_ref() else {
                self.fatal(now, "missing opened roots");
                return;
            };
            let token = run.token.clone();
            let spawned = match roots.native_target.clone() {
                Some(launch) => EngineWorker::spawn_selected_native(
                    run.handle.clone(),
                    run.recipe,
                    run.options,
                    run.token,
                    roots.output.clone(),
                    launch,
                ),
                None => Err(QueryError::new(
                    ErrorCode::InvalidArguments,
                    anyhow::anyhow!("native server target unavailable"),
                )),
            };
            match spawned {
                Ok(worker) => {
                    self.engine = Some(worker);
                    self.active_run = Some(ActiveRun {
                        handle: handle.clone(),
                        token,
                        deadline: now + Duration::from_secs(self.max_run_seconds),
                    });
                    self.apply(&handle, RegistryUpdate::Dispatched);
                }
                Err(error) => {
                    self.worker_failed = true;
                    self.apply(
                        &handle,
                        RegistryUpdate::SpawnFailed(OwnedError::deferred(error)),
                    );
                }
            }
        } else if let Some(run) = self.registry.take_dispatch(&handle) {
            if self.engine.is_some() {
                self.fatal(now, "engine slot already occupied");
                return;
            }
            let Some(roots) = self.roots.as_ref() else {
                self.fatal(now, "missing opened roots");
                return;
            };
            let token = run.token.clone();
            match EngineWorker::spawn(
                run.handle.clone(),
                run.recipe,
                run.options,
                run.token,
                roots.output.clone(),
            ) {
                Ok(worker) => {
                    self.engine = Some(worker);
                    self.active_run = Some(ActiveRun {
                        handle: handle.clone(),
                        token,
                        deadline: now + Duration::from_secs(self.max_run_seconds),
                    });
                    self.apply(&handle, RegistryUpdate::Dispatched);
                }
                Err(error) => {
                    // Thread creation failed: no producer/native operation exists.
                    self.worker_failed = true;
                    self.apply(
                        &handle,
                        RegistryUpdate::SpawnFailed(OwnedError::deferred(error)),
                    );
                }
            }
        }
    }
    fn complete_request(
        &mut self,
        slot: SlotId,
        result: Result<QueryValue, OwnedError>,
        now: Instant,
    ) {
        let result = match result {
            Err(owner) => {
                let request = self.requests[usize::from(slot.index)]
                    .as_ref()
                    .filter(|r| r.slot == slot);
                let failure = Failure::new(
                    self.instance.clone(),
                    owner.view().code,
                    owner.view().message.clone(),
                    request.and_then(|r| r.handle.clone()),
                );
                if owner.view().cleanup_blocked {
                    self.owners.push(owner);
                } else if let Some(request) = self.requests[usize::from(slot.index)]
                    .as_mut()
                    .filter(|r| r.slot == slot)
                {
                    request.error = Some(owner);
                }
                Err(failure)
            }
            Ok(QueryValue::Inspect(data)) => Ok(ToolData::RecipeInspect(data)),
            Ok(QueryValue::Evidence(data)) => Ok(ToolData::RunEvidence(data)),
            Ok(QueryValue::Refreshed(projection)) => {
                let Some(handle) = self.requests[usize::from(slot.index)]
                    .as_ref()
                    .filter(|r| r.slot == slot)
                    .and_then(|r| r.handle.clone())
                else {
                    return;
                };
                self.apply(&handle, RegistryUpdate::Refreshed(projection));
                self.registry.cached(&handle).map(|run| {
                    ToolData::RunStatus(StatusData {
                        run: run.clone(),
                        refresh: Refresh::Loaded,
                        poll_after_ms: 500,
                    })
                })
            }
            Ok(
                QueryValue::Validated(_)
                | QueryValue::ValidatedNative(_)
                | QueryValue::ValidatedIntegration(_),
            ) => {
                self.fatal(now, "invalid request completion");
                return;
            }
        };
        self.reply(slot, result, now);
    }
}
