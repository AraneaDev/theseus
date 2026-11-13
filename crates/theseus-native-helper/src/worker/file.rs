use super::*;
impl Worker {
    pub(super) fn begin_file(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
        cancel: CancelToken,
        start: StartWatch,
    ) -> Result<(), WorkerPrimary> {
        if self.active.is_some() || self.file.is_some() || self.service_action.is_some() {
            self.refuse(ticket, v2::RefusalCode::Busy);
            return Ok(());
        }
        if self.service.as_ref().is_some_and(|s| s.uncertain) {
            self.refuse(ticket, v2::RefusalCode::OwnershipPending);
            return Ok(());
        }
        let v2::Request::CreateFile {
            lease, operation, ..
        } = &request
        else {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        };
        let (lease, operation) = (*lease, *operation);
        if !self.lease.as_ref().is_some_and(|(_, l, _)| *l == lease) {
            self.refuse(ticket, v2::RefusalCode::WrongLease);
            return Ok(());
        }
        if let Some(reason) = self.check_native_mapping(lease)? {
            self.respond(
                ticket,
                v2::Reply::PrefixAuthorityRevoked { lease, reason },
                None,
            );
            return Ok(());
        }
        let key = cancel.key;
        if key.lease != lease || key.operation != operation || start.key() != key {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        }
        let replay = self
            .store
            .as_ref()
            .unwrap()
            .restart_view()
            .replay_operation(&request);
        let now = Instant::now();
        match replay {
            Ok(v2::ledger::ReplayDecision::Recorded(view)) => {
                self.file = Some(PendingFileRequest {
                    request,
                    ticket: None,
                    owner: Box::new(crate::files::FileOwner::new()),
                    key: Some(key),
                    create: Some(FileCreate {
                        cancel,
                        start,
                        deadline: now,
                        terminal: Some(view.clone()),
                        published: true,
                    }),
                });
                self.response = Some((
                    ticket,
                    v2::Reply::Operation {
                        lease,
                        operation,
                        state: view.clone(),
                    },
                    None,
                ));
                self.publish(WorkerEvent::CacheUpdate(CachePatch::Terminal {
                    key,
                    view,
                    boundary: file_boundary(key),
                }));
                return Ok(());
            }
            Err(code) => {
                self.refuse(ticket, code);
                return Ok(());
            }
            Ok(v2::ledger::ReplayDecision::NotRecorded) => {}
        }
        if let Some(code) = self.held_refusal(lease) {
            self.refuse(ticket, code);
            return Ok(());
        }
        let digest = v2::request_digest(&request)
            .map_err(|_| WorkerPrimary::Io(io::Error::other("invalid file request")))?;
        self.append(Entry::OperationAccepted {
            lease,
            operation,
            request_digest: digest,
            operation_kind: v2::OperationKind::CreateFile,
        })?;
        #[cfg(feature = "test-support")]
        if let Some(control) = self
            .invocation
            .as_ref()
            .and_then(|inv| inv.fixture.as_ref())
        {
            control.mark_accepted_committed();
        }
        let now = Instant::now();
        let deadline = now + Duration::from_secs(30);
        self.file = Some(PendingFileRequest {
            request,
            ticket: None,
            owner: Box::new(crate::files::FileOwner::new()),
            key: Some(key),
            create: Some(FileCreate {
                cancel,
                start,
                deadline,
                terminal: None,
                published: false,
            }),
        });
        self.response = Some((
            ticket,
            v2::Reply::Operation {
                lease,
                operation,
                state: v2::OperationView::Accepted {
                    operation_kind: v2::OperationKind::CreateFile,
                },
            },
            Some(key),
        ));
        self.publish(WorkerEvent::CacheUpdate(CachePatch::Phase {
            key,
            kind: v2::OperationKind::CreateFile,
            phase: LivePhase::Accepted,
            accepted_at: Some(now),
            deadline: Some(deadline),
        }));
        Ok(())
    }
    fn inspect_native_file(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
        lease: v2::LeaseId,
        path: v2::LeafName,
    ) -> Result<(), WorkerPrimary> {
        if !matches!(
            self.invocation.as_ref().map(|i| i.profile),
            Some(v2::HelperProfile::Native { .. } | v2::HelperProfile::Integration { .. })
        ) {
            self.refuse(ticket, v2::RefusalCode::Unsupported);
            return Ok(());
        }
        if let Some(reason) = self.check_native_mapping(lease)? {
            self.respond(
                ticket,
                v2::Reply::PrefixAuthorityRevoked { lease, reason },
                None,
            );
            return Ok(());
        }
        self.file = Some(PendingFileRequest {
            request,
            ticket: Some(ticket),
            owner: Box::new(crate::files::FileOwner::new()),
            key: None,
            create: None,
        });
        let result = {
            let pending = self.file.as_mut().unwrap();
            let stop = &self.ports.stop;
            crate::files::inspect_native(
                &mut pending.owner,
                self.workspace.as_ref().unwrap(),
                &path,
                Instant::now() + Duration::from_secs(30),
                || stop.load(Ordering::Acquire),
            )
        };
        let mapping = self.check_native_mapping(lease)?;
        let reply = if let Some(reason) = mapping {
            v2::Reply::PrefixAuthorityRevoked { lease, reason }
        } else {
            match result {
                Ok(crate::files::NativeObservation::Present { length, sha256 }) => {
                    v2::Reply::NativeFileInfo {
                        lease,
                        path,
                        length,
                        sha256,
                    }
                }
                Ok(crate::files::NativeObservation::Absent) => {
                    v2::Reply::FileAbsent { lease, path }
                }
                Err(_) => v2::Reply::Refused {
                    code: v2::RefusalCode::Unsupported,
                    inspect_failure: None,
                },
            }
        };
        let pending = self.file.as_mut().unwrap();
        let ticket = pending.ticket.take().unwrap();
        let sticky = pending.owner.sticky_open;
        self.respond(ticket, reply, None);
        if sticky {
            self.stopping = true;
        } else {
            self.file.take();
        }
        Ok(())
    }
    pub(super) fn observe_file(
        &mut self,
        request: v2::Request,
        ticket: ResponseTicket,
    ) -> Result<(), WorkerPrimary> {
        let (lease, path) = match &request {
            v2::Request::ReadFile { lease, path, .. }
            | v2::Request::InspectFile { lease, path }
            | v2::Request::InspectNativeFile { lease, path } => (*lease, path.clone()),
            _ => {
                self.refuse(ticket, v2::RefusalCode::InvalidRequest);
                return Ok(());
            }
        };
        if let Some(code) = self.held_refusal(lease) {
            self.refuse(ticket, code);
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
        if matches!(request, v2::Request::InspectNativeFile { .. }) {
            return self.inspect_native_file(request, ticket, lease, path);
        }
        if let v2::Request::ReadFile { offset, length, .. } = &request
            && (*length == 0 || *length > 4096 || offset.checked_add(u64::from(*length)).is_none())
        {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        }
        self.file = Some(PendingFileRequest {
            request,
            ticket: Some(ticket),
            owner: Box::new(crate::files::FileOwner::new()),
            key: None,
            create: None,
        });
        let result = {
            let pending = self.file.as_mut().unwrap();
            crate::files::observe(&mut pending.owner, self.workspace.as_ref().unwrap(), &path)
        };
        let reply = match result {
            Ok(observed) => match &self.file.as_ref().unwrap().request {
                v2::Request::InspectFile { .. } => v2::Reply::FileInfo {
                    lease,
                    path,
                    length: observed.bytes.as_slice().len() as u16,
                    digest: observed.digest,
                },
                v2::Request::ReadFile { offset, length, .. } => {
                    let all = observed.bytes.as_slice();
                    let begin = if *offset >= all.len() as u64 {
                        all.len()
                    } else {
                        *offset as usize
                    };
                    let end = begin.saturating_add(usize::from(*length)).min(all.len());
                    v2::Reply::FileBytes {
                        lease,
                        path,
                        offset: *offset,
                        bytes: v2::FileBytes::new(&all[begin..end]).expect("bounded observation"),
                        eof: end == all.len(),
                    }
                }
                _ => unreachable!(),
            },
            Err(kind) => v2::Reply::Refused {
                code: if kind == crate::files::FileFailureKind::Changed {
                    v2::RefusalCode::Busy
                } else {
                    v2::RefusalCode::Unsupported
                },
                inspect_failure: None,
            },
        };
        let pending = self.file.as_mut().unwrap();
        let ticket = pending.ticket.take().unwrap();
        let sticky = pending.owner.sticky_open;
        self.respond(ticket, reply, None);
        if sticky {
            self.stopping = true;
        } else {
            // Synchronous observation completed; the request may retire its known
            // handle and source after handing back its reserved response.
            self.file.take();
        }
        Ok(())
    }
    pub(super) fn advance_file(&mut self) -> Result<(), WorkerPrimary> {
        let Some(pending) = &mut self.file else {
            return Ok(());
        };
        let Some(create) = &mut pending.create else {
            return Ok(());
        };
        let key = pending.key.unwrap();
        if let Some(view) = &create.terminal {
            if !create.published && self.pending.is_none() {
                let view = view.clone();
                create.published = true;
                self.publish(WorkerEvent::CacheUpdate(CachePatch::Terminal {
                    key,
                    view,
                    boundary: file_boundary(key),
                }));
            }
            return Ok(());
        }
        let expired = Instant::now() >= create.deadline;
        let cancelled = self.stopping
            || self.ports.stop.load(Ordering::Acquire)
            || create.cancel.flag.load(Ordering::Acquire)
            || matches!(create.start.poll(), StartState::Aborted);
        if expired || cancelled {
            return self.finish_file(v2::Outcome::NotStarted {
                reason: if expired {
                    v2::NotStartedReason::Deadline
                } else {
                    v2::NotStartedReason::CancelRequested
                },
            });
        }
        if !matches!(create.start.poll(), StartState::Granted) {
            return Ok(());
        }
        #[cfg(feature = "test-support")]
        if let Some(control) = self
            .invocation
            .as_ref()
            .and_then(|inv| inv.fixture.as_ref())
        {
            control.before_file_effect();
            let expired = Instant::now() >= create.deadline;
            let cancelled = self.stopping
                || self.ports.stop.load(Ordering::Acquire)
                || create.cancel.flag.load(Ordering::Acquire)
                || matches!(create.start.poll(), StartState::Aborted);
            if expired || cancelled {
                return self.finish_file(v2::Outcome::NotStarted {
                    reason: if expired {
                        v2::NotStartedReason::Deadline
                    } else {
                        v2::NotStartedReason::CancelRequested
                    },
                });
            }
            pending.owner.fixture = Some(control.clone());
        }
        let v2::Request::CreateFile { path, bytes, .. } = &pending.request else {
            unreachable!()
        };
        let result = crate::files::create(
            &mut pending.owner,
            self.workspace.as_ref().unwrap(),
            path,
            bytes,
        );
        match result {
            Ok(created) => self.finish_file(v2::Outcome::FileCreated {
                length: created.length,
                digest: created.digest,
            }),
            Err(_) => self.quarantine_file(),
        }
    }
    fn finish_file(&mut self, outcome: v2::Outcome) -> Result<(), WorkerPrimary> {
        let key = self.file.as_ref().unwrap().key.unwrap();
        self.append(Entry::OperationFinished {
            lease: key.lease,
            operation: key.operation,
            outcome: outcome.clone(),
        })?;
        self.file
            .as_mut()
            .unwrap()
            .create
            .as_mut()
            .unwrap()
            .terminal = Some(v2::OperationView::Finished { outcome });
        #[cfg(feature = "test-support")]
        if let Some(control) = self
            .invocation
            .as_ref()
            .and_then(|inv| inv.fixture.as_ref())
        {
            control.after_file_finished();
        }
        self.advance_file()
    }
    pub(super) fn quarantine_file(&mut self) -> Result<(), WorkerPrimary> {
        let key = self.file.as_ref().unwrap().key.unwrap();
        let reason = v2::UnknownReason::FileEffectUncertain;
        if let Err(error) = self.append(Entry::LeaseClosed {
            lease: key.lease,
            disposition: Disposition::Quarantined { reason },
        }) {
            if let WorkerPrimary::Store(error) = error {
                self.file.as_mut().unwrap().owner.ledger_error = Some(error);
            } else {
                self.fail(error);
            }
            self.stopping = true;
            self.transfer_file_failure();
            return Ok(());
        }
        #[cfg(feature = "test-support")]
        if let Some(control) = self
            .invocation
            .as_ref()
            .and_then(|inv| inv.fixture.as_ref())
        {
            control.mark_file_quarantine_committed();
        }
        let intent = self.lease.as_ref().unwrap().0;
        let view = v2::LeaseView::Quarantined { intent, reason };
        self.lease = Some((intent, key.lease, view.clone()));
        self.file
            .as_mut()
            .unwrap()
            .create
            .as_mut()
            .unwrap()
            .terminal = Some(v2::OperationView::Unknown { reason });
        self.publish(WorkerEvent::CacheUpdate(CachePatch::Lease {
            intent,
            lease: key.lease,
            view,
        }));
        if self.file.as_ref().unwrap().owner.sticky_open {
            self.stopping = true;
        }
        Ok(())
    }
    fn transfer_file_failure(&mut self) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if self.failure.is_none() {
            let primary = file
                .owner
                .primary
                .take()
                .map(WorkerPrimary::File)
                .or_else(|| file.owner.panic.take().map(WorkerPrimary::Panic))
                .or_else(|| file.owner.ledger_error.take().map(WorkerPrimary::Store));
            if let Some(primary) = primary {
                self.failure = Some(OwnedWorkerFailure {
                    primary,
                    retained_file: None,
                    retained_spawn: None,
                    retained_service_errors: vec![],
                    cleanup_errors: std::array::from_fn(|_| None),
                });
            }
        }
    }
    pub(super) fn retire_file(&mut self) {
        self.transfer_file_failure();
        if let Some(pending) = self.file.take()
            && let Some(failure) = &mut self.failure
        {
            failure.retained_file = Some(pending.owner);
        }
    }
    pub(super) fn file_panic(&mut self, payload: Box<dyn std::any::Any + Send>) {
        if let Some(pending) = &mut self.file {
            pending.owner.panic = Some(payload);
            if pending.owner.opening {
                pending.owner.sticky_open = true;
            }
            let uncertain = pending.owner.attempted
                && pending
                    .create
                    .as_ref()
                    .is_some_and(|c| c.terminal.is_none());
            if uncertain {
                let _ = self.quarantine_file();
            }
            self.transfer_file_failure();
            self.stopping = true;
        } else {
            self.fail(WorkerPrimary::Panic(payload));
        }
    }
}
