//! Per-tool request dispatch.
use super::super::{
    framing::SlotId,
    query::{QueryDestination, QueryWork},
    registry::{AdmissionDecision, RegistryUpdate},
    types::*,
};
use super::{ServerRuntime, Transport};
use std::time::Instant;
impl<T: Transport> ServerRuntime<T> {
    pub(super) fn tool(&mut self, slot: SlotId, call: ToolCall, now: Instant) {
        let reply = self.requests[usize::from(slot.index)]
            .as_ref()
            .expect("reserved request")
            .reply
            .clone()
            .expect("tool request has reply context");
        match call {
            ToolCall::RecipeInspect(args) => {
                if self
                    .submit(
                        QueryDestination::Request(slot),
                        QueryWork::Inspect { args, reply },
                        now,
                    )
                    .is_none()
                {
                    self.reply_failure(slot, ErrorCode::QueryBusy, now);
                }
            }
            ToolCall::RunStart(args) => self.tool_run_start(slot, args, now),
            ToolCall::RunList(args) => {
                let result = self.registry.list(&args, &reply).map(ToolData::RunList);
                self.reply(slot, result, now);
            }
            ToolCall::RunCancel(args) => self.tool_run_cancel(slot, args, now),
            ToolCall::RunStatus(args) => self.tool_run_status(slot, args, now),
            ToolCall::RunEvidence(args) => self.tool_run_evidence(slot, args, reply, now),
        }
    }
    fn tool_run_start(&mut self, slot: SlotId, args: StartArgs, now: Instant) {
        self.requests[usize::from(slot.index)]
            .as_mut()
            .expect("request")
            .admission = true;
        match self.registry.admit(args, self.query_available()) {
            Err(failure) => self.reply(slot, Err(failure), now),
            Ok(AdmissionDecision::Replay(data)) => {
                self.requests[usize::from(slot.index)]
                    .as_mut()
                    .expect("reserved request")
                    .handle = Some(data.handle.clone());
                self.reply(slot, Ok(ToolData::RunStart(data)), now);
            }
            Ok(AdmissionDecision::Reserved { handle, validation }) => {
                self.requests[usize::from(slot.index)]
                    .as_mut()
                    .expect("reserved request")
                    .handle = Some(handle.clone());
                if let Some(generation) = self.submit(
                    QueryDestination::Admission(handle.clone()),
                    QueryWork::ValidateStart(validation),
                    now,
                ) {
                    self.apply(&handle, RegistryUpdate::ValidationStarted { generation });
                    self.reply(
                        slot,
                        Ok(ToolData::RunStart(StartData {
                            handle,
                            state: RunState::Validating,
                            replayed: false,
                            cancellation_requested: false,
                            lifetime: Lifetime::ServerProcess,
                            poll_after_ms: 500,
                        })),
                        now,
                    );
                } else {
                    let _ = self
                        .registry
                        .cancel(&handle, CancellationReason::ServerShutdown);
                    self.fatal(now, "reserved admission could not submit validation");
                }
            }
        }
    }
    fn tool_run_cancel(&mut self, slot: SlotId, args: CancelArgs, now: Instant) {
        let result = if args.server_instance != self.instance {
            Err(self.failure(ErrorCode::StaleInstance, Some(args.handle.clone())))
        } else {
            self.registry
                .cancel(&args.handle, CancellationReason::Client)
                .map(ToolData::RunCancel)
        };
        if result.is_ok()
            && let Some(q) = &mut self.active_query
            && q.destination == QueryDestination::Admission(args.handle.clone())
        {
            q.token.cancel();
            q.live = false;
        }
        self.reply(slot, result, now);
    }
    fn tool_run_status(&mut self, slot: SlotId, args: StatusArgs, now: Instant) {
        self.requests[usize::from(slot.index)]
            .as_mut()
            .expect("request")
            .handle = Some(args.handle.clone());
        if args.server_instance != self.instance {
            self.reply_failure(slot, ErrorCode::StaleInstance, now);
            return;
        }
        let run = match self.registry.cached(&args.handle) {
            Ok(v) => v.clone(),
            Err(f) => {
                self.reply(slot, Err(f), now);
                return;
            }
        };
        let binding = self.registry.binding(&args.handle).ok().flatten().cloned();
        let refresh = if !args.refresh {
            Refresh::Cached
        } else if binding.is_none() {
            Refresh::Unavailable
        } else if !self.query_available() {
            Refresh::Busy
        } else {
            self.submit(
                QueryDestination::Request(slot),
                QueryWork::RefreshStatus {
                    binding: binding.expect("checked binding"),
                    sampled_hint: run.saved_hint_sequence.get(),
                },
                now,
            );
            return;
        };
        self.reply(
            slot,
            Ok(ToolData::RunStatus(StatusData {
                run,
                refresh,
                poll_after_ms: 500,
            })),
            now,
        );
    }
    fn tool_run_evidence(
        &mut self,
        slot: SlotId,
        args: EvidenceArgs,
        reply: ReplyContext,
        now: Instant,
    ) {
        self.requests[usize::from(slot.index)]
            .as_mut()
            .expect("request")
            .handle = Some(args.handle.clone());
        if args.server_instance != self.instance {
            self.reply_failure(slot, ErrorCode::StaleInstance, now);
            return;
        }
        let binding = match self.registry.binding(&args.handle) {
            Ok(Some(b)) => b.clone(),
            Ok(None) => {
                self.reply_failure(slot, ErrorCode::EvidenceInvalid, now);
                return;
            }
            Err(f) => {
                self.reply(slot, Err(f), now);
                return;
            }
        };
        let sampled_hint = self
            .registry
            .cached(&args.handle)
            .expect("binding exists")
            .saved_hint_sequence
            .get();
        if self
            .submit(
                QueryDestination::Request(slot),
                QueryWork::Evidence {
                    binding,
                    args,
                    sampled_hint,
                    reply,
                },
                now,
            )
            .is_none()
        {
            self.reply_failure(slot, ErrorCode::QueryBusy, now);
        }
    }
}
