use super::*;
impl Worker {
    pub(super) fn inspect_service(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
    ) -> Result<(), WorkerPrimary> {
        let v2::Request::InspectService { lease, slot } = request else {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        };
        if self.active.is_some() || self.file.is_some() || self.service_action.is_some() {
            self.refuse(ticket, v2::RefusalCode::Busy);
            return Ok(());
        }
        if self.service.as_ref().is_some_and(|s| s.uncertain) {
            self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            return Ok(());
        }
        if let Some(code) = self.held_refusal(lease) {
            self.refuse(ticket, code);
            return Ok(());
        }
        let Some(service) = &mut self.service else {
            self.refuse(ticket, v2::RefusalCode::Unsupported);
            return Ok(());
        };
        match service.inspect(
            lease,
            &slot,
            Instant::now() + Duration::from_secs(30),
            &self.ports.stop,
        ) {
            Ok(observation) => self.respond(
                ticket,
                v2::Reply::ServiceObservation {
                    lease,
                    observation: Box::new(observation),
                },
                None,
            ),
            Err(error) => {
                // Same OwnershipPending refusal; only the closed step name of
                // the failed inspection travels, never the retained error text.
                let failure = crate::integration::service::inspect_failure(&error);
                service.errors.push(error);
                self.respond(
                    ticket,
                    v2::Reply::Refused {
                        code: v2::RefusalCode::OwnershipPending,
                        inspect_failure: Some(failure),
                    },
                    None,
                );
            }
        }
        Ok(())
    }
    pub(super) fn begin_service(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
        cancel: CancelToken,
        start: StartWatch,
    ) -> Result<(), WorkerPrimary> {
        if self.service.is_none() {
            self.refuse(ticket, v2::RefusalCode::Unsupported);
            return Ok(());
        }
        if self.active.is_some() || self.file.is_some() || self.service_action.is_some() {
            self.refuse(ticket, v2::RefusalCode::Busy);
            return Ok(());
        }
        if self.service.as_ref().is_some_and(|s| s.uncertain) {
            self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            return Ok(());
        }
        let v2::Request::ServiceAction {
            lease, operation, ..
        } = &request
        else {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        };
        let (lease, operation) = (*lease, *operation);
        let key = cancel.key;
        if key.lease != lease || key.operation != operation || start.key() != key {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        }
        if let Some(code) = self.held_refusal(lease) {
            self.refuse(ticket, code);
            return Ok(());
        }
        let replay = self
            .store
            .as_ref()
            .unwrap()
            .restart_view()
            .replay_operation(&request);
        match replay {
            Ok(v2::ledger::ReplayDecision::Recorded(view)) => {
                self.respond(
                    ticket,
                    v2::Reply::Operation {
                        lease,
                        operation,
                        state: view,
                    },
                    None,
                );
                return Ok(());
            }
            Err(code) => {
                self.refuse(ticket, code);
                return Ok(());
            }
            Ok(v2::ledger::ReplayDecision::NotRecorded) => (),
        }
        let digest =
            v2::request_digest(&request).map_err(|e| WorkerPrimary::Io(io::Error::other(e)))?;
        self.append(Entry::OperationAccepted {
            lease,
            operation,
            request_digest: digest,
            operation_kind: v2::OperationKind::ServiceAction,
        })?;
        let now = Instant::now();
        let deadline = now + Duration::from_secs(30);
        self.service_action = Some(PendingService {
            request,
            key,
            cancel,
            start,
            deadline,
            attempted: false,
            terminal: None,
            published: false,
        });
        self.response = Some((
            ticket,
            v2::Reply::Operation {
                lease,
                operation,
                state: v2::OperationView::Accepted {
                    operation_kind: v2::OperationKind::ServiceAction,
                },
            },
            Some(key),
        ));
        self.publish(WorkerEvent::CacheUpdate(CachePatch::Phase {
            key,
            kind: v2::OperationKind::ServiceAction,
            phase: LivePhase::Accepted,
            accepted_at: Some(now),
            deadline: Some(deadline),
        }));
        Ok(())
    }
    pub(super) fn advance_service(&mut self) -> Result<(), WorkerPrimary> {
        let Some(pending) = &mut self.service_action else {
            return Ok(());
        };
        let key = pending.key;
        if let Some(view) = &pending.terminal {
            if !pending.published && self.pending.is_none() {
                let view = view.clone();
                pending.published = true;
                self.publish(WorkerEvent::CacheUpdate(CachePatch::Terminal {
                    key,
                    view,
                    boundary: file_boundary(key),
                }));
            }
            return Ok(());
        }
        let stopped = self.stopping
            || self.ports.stop.load(Ordering::Acquire)
            || pending.cancel.flag.load(Ordering::Acquire)
            || matches!(pending.start.poll(), StartState::Aborted);
        let expired = Instant::now() >= pending.deadline;
        if !pending.attempted && (stopped || expired) {
            return self.finish_service(v2::Outcome::NotStarted {
                reason: if expired {
                    v2::NotStartedReason::Deadline
                } else {
                    v2::NotStartedReason::CancelRequested
                },
            });
        }
        if !matches!(pending.start.poll(), StartState::Granted) {
            return Ok(());
        }
        if self.stopping || self.ports.stop.load(Ordering::Acquire) {
            pending.cancel.flag.store(true, Ordering::Release);
        }
        let service = self.service.as_mut().unwrap();
        let result = if pending.attempted {
            service.poll(pending.deadline, &pending.cancel.flag)
        } else {
            pending.attempted = true;
            service
                .note_action(&pending.request)
                .map_err(WorkerPrimary::Io)?;
            let v2::Request::ServiceAction {
                slot,
                prior_binding_digest,
                action,
                ..
            } = &pending.request
            else {
                unreachable!()
            };
            service.action(
                slot,
                *prior_binding_digest,
                *action,
                pending.deadline,
                &pending.cancel.flag,
            )
        };
        match result {
            Ok(Some(outcome)) => {
                self.finish_service(v2::Outcome::ServiceActionCompleted { outcome })
            }
            Ok(None) => Ok(()),
            Err(error) => {
                service.errors.push(error);
                service.uncertain = true;
                self.finish_service(v2::Outcome::ServiceActionCompleted {
                    outcome: v2::ServiceOutcome::Unknown {
                        reason: "native service action retains unresolved authority".into(),
                    },
                })
            }
        }
    }
    fn finish_service(&mut self, outcome: v2::Outcome) -> Result<(), WorkerPrimary> {
        let key = self.service_action.as_ref().unwrap().key;
        self.append(Entry::OperationFinished {
            lease: key.lease,
            operation: key.operation,
            outcome: outcome.clone(),
        })?;
        self.service_action.as_mut().unwrap().terminal =
            Some(v2::OperationView::Finished { outcome });
        self.advance_service()
    }
}
