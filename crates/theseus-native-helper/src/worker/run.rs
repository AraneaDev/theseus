use super::*;
impl Worker {
    pub(super) fn begin(
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
        let v2::Request::Run {
            lease,
            operation,
            program,
        } = &request
        else {
            self.refuse(ticket, v2::RefusalCode::InvalidRequest);
            return Ok(());
        };
        let lease = *lease;
        let operation = *operation;
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
        match self
            .store
            .as_ref()
            .unwrap()
            .restart_view()
            .replay_operation(&request)
        {
            Ok(v2::ledger::ReplayDecision::NotRecorded) => {}
            Ok(v2::ledger::ReplayDecision::Recorded(view)) => {
                let now = Instant::now();
                let out = OutputState::new(key);
                let boundary = out.boundary();
                self.active = Some(Active {
                    key,
                    request_digest: v2::request_digest(&request)
                        .map_err(|e| WorkerPrimary::Io(io::Error::other(e)))?,
                    cancel,
                    start,
                    _accepted: None,
                    accepted_at: now,
                    deadline: now,
                    attempted: false,
                    child: None,
                    spawn_failure: None,
                    out,
                    stdout_eof: true,
                    stderr_eof: true,
                    status: None,
                    reason: v2::RunReason::Natural,
                    outstanding: [None; 4],
                    boundary: Some(boundary),
                    committed: true,
                    outcome: None,
                    cleanup_rounds: 0,
                    terminal: true,
                    returned: 0,
                    abandoned: false,
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
                    boundary,
                }));
                return Ok(());
            }
            Err(code) => {
                self.refuse(ticket, code);
                return Ok(());
            }
        }
        if !self
            .invocation
            .as_ref()
            .is_some_and(|i| i.catalog.iter().any(|c| &c.id == program))
        {
            self.refuse(ticket, v2::RefusalCode::Unsupported);
            return Ok(());
        }
        self.last_spawn = None;
        let digest = v2::ledger::request_digest(&request)
            .map_err(|_| WorkerPrimary::Io(io::Error::other("invalid request digest")))?;
        let committed = self.append(Entry::OperationAccepted {
            lease,
            operation,
            request_digest: digest,
            operation_kind: v2::OperationKind::Run,
        })?;
        #[cfg(feature = "test-support")]
        if let Some(control) = self
            .invocation
            .as_ref()
            .and_then(|inv| inv.fixture.as_ref())
        {
            control.mark_accepted_committed();
        }
        let accepted_at = Instant::now();
        let deadline = accepted_at + Duration::from_secs(30);
        self.active = Some(Active {
            key,
            request_digest: digest,
            cancel,
            start,
            _accepted: Some(committed),
            accepted_at,
            deadline,
            attempted: false,
            child: None,
            spawn_failure: None,
            out: OutputState::new(key),
            stdout_eof: false,
            stderr_eof: false,
            status: None,
            reason: v2::RunReason::Natural,
            outstanding: [None; 4],
            boundary: None,
            committed: false,
            outcome: None,
            cleanup_rounds: 0,
            terminal: false,
            returned: 0,
            abandoned: false,
        });
        #[cfg(feature = "test-support")]
        {
            self.active.as_mut().unwrap().out.fixture =
                self.invocation.as_ref().and_then(|inv| inv.fixture.clone());
        }
        // Catalog selection is retained by digest and immutable invocation; no peer argv.
        self.program = Some(program.clone());
        self.response = Some((
            ticket,
            v2::Reply::Operation {
                lease,
                operation,
                state: v2::OperationView::Accepted {
                    operation_kind: v2::OperationKind::Run,
                },
            },
            Some(key),
        ));
        self.publish(WorkerEvent::CacheUpdate(CachePatch::Phase {
            key,
            kind: v2::OperationKind::Run,
            phase: LivePhase::Accepted,
            accepted_at: Some(accepted_at),
            deadline: Some(deadline),
        }));
        Ok(())
    }
    pub(super) fn returns(&mut self) {
        for _ in 0..4 {
            match self.ports.returned.try_recv() {
                Ok(DataReturn::Reusable {
                    key,
                    ordinal,
                    disposition,
                    permit,
                }) => {
                    if let Some(a) = &mut self.active {
                        if key != a.key
                            || ordinal != a.returned + 1
                            || ordinal > a.out.ordinal
                            || a.outstanding.get(permit.id as usize) != Some(&Some(ordinal))
                        {
                            self.fail(WorkerPrimary::Io(io::Error::other("invalid token return")));
                            self.tokens.push(permit);
                            return;
                        }
                        a.outstanding[permit.id as usize] = None;
                        a.returned = ordinal;
                        a.abandoned |=
                            matches!(disposition, ReturnDisposition::AbandonedBeforeSubmit);
                    }
                    self.tokens.push(permit);
                }
                Ok(DataReturn::ProducerDrained { key, final_ordinal }) => {
                    if self
                        .service_action
                        .as_ref()
                        .is_some_and(|p| p.key == key && final_ordinal == 0 && p.published)
                    {
                        if let Some(service) = &mut self.service
                            && let Err(e) = service.retire()
                        {
                            service.errors.push(e);
                            service.uncertain = true;
                        }
                        self.service_action = None;
                        continue;
                    }
                    if self.file.as_ref().is_some_and(|p| {
                        p.key == Some(key)
                            && final_ordinal == 0
                            && p.create.as_ref().is_some_and(|c| c.published)
                    }) {
                        self.retire_file();
                        continue;
                    }
                    if self.active.as_ref().is_some_and(|a| {
                        a.key == key
                            && a.terminal
                            && !a.abandoned
                            && a.boundary.is_some_and(|b| b.final_ordinal == final_ordinal)
                            && a.returned == final_ordinal
                    }) {
                        self.active = None;
                        self.program = None;
                    } else {
                        self.fail(WorkerPrimary::Io(io::Error::other(
                            "invalid producer drain",
                        )));
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.stopping = true;
                    break;
                }
            }
        }
    }
    pub(super) fn advance(&mut self) -> Result<(), WorkerPrimary> {
        self.advance_at(Instant::now())
    }
    pub(super) fn advance_at(&mut self, now: Instant) -> Result<(), WorkerPrimary> {
        let a = self.active.as_mut().unwrap();
        if a.terminal {
            // CachePatch::Terminal is the sole frozen boundary publication.
            // A second event could outlive retirement and hit the next RunKey.
            return Ok(());
        }
        if a.committed {
            if self.pending.is_none() {
                let key = a.key;
                let boundary = a.boundary.unwrap();
                let view = v2::OperationView::Finished {
                    outcome: a.outcome.take().unwrap(),
                };
                a.terminal = true;
                self.publish(WorkerEvent::CacheUpdate(CachePatch::Terminal {
                    key,
                    view,
                    boundary,
                }));
            }
            return Ok(());
        }
        let expired = now >= a.deadline;
        let cancelled = self.stopping
            || a.cancel.flag.load(Ordering::Acquire)
            || matches!(a.start.poll(), StartState::Aborted);
        if expired {
            a.reason = v2::RunReason::Deadline;
        } else if cancelled {
            a.reason = v2::RunReason::CancelRequested;
        }
        if !a.attempted {
            if expired || cancelled {
                let reason = if expired {
                    v2::NotStartedReason::Deadline
                } else {
                    v2::NotStartedReason::CancelRequested
                };
                return self.finish(v2::Outcome::NotStarted { reason });
            }
            if let Some(desktop) = &mut self.desktop {
                let program = self.program.as_ref().ok_or_else(|| {
                    WorkerPrimary::Io(io::Error::other("original Run program missing"))
                })?;
                match desktop.prepare_original_run(
                    self.store.as_mut().unwrap(),
                    a._accepted.as_ref().ok_or_else(|| {
                        WorkerPrimary::Io(io::Error::other(
                            "original Run acceptance capability missing",
                        ))
                    })?,
                    program,
                    a.deadline,
                ) {
                    Ok(true) => {}
                    Ok(false) => return Ok(()),
                    Err(error) => {
                        desktop.fail(error);
                        return self.finish(v2::Outcome::NotStarted {
                            reason: v2::NotStartedReason::CancelRequested,
                        });
                    }
                }
            }
            if !matches!(a.start.poll(), StartState::Granted) {
                return Ok(());
            }
            #[cfg(feature = "test-support")]
            if let Some(control) = self
                .invocation
                .as_ref()
                .and_then(|inv| inv.fixture.as_ref())
            {
                control.before_spawn();
                // A fixture gate may hold across cancellation, stop or deadline.
                let expired = Instant::now() >= a.deadline;
                let cancelled = self.stopping
                    || self.ports.stop.load(Ordering::Acquire)
                    || a.cancel.flag.load(Ordering::Acquire)
                    || matches!(a.start.poll(), StartState::Aborted);
                if expired || cancelled {
                    let reason = if expired {
                        v2::NotStartedReason::Deadline
                    } else {
                        v2::NotStartedReason::CancelRequested
                    };
                    return self.finish(v2::Outcome::NotStarted { reason });
                }
            }
            a.attempted = true;
            let command = &self
                .invocation
                .as_ref()
                .unwrap()
                .catalog
                .iter()
                .find(|c| Some(&c.id) == self.program.as_ref())
                .unwrap()
                .command;
            #[cfg(feature = "test-support")]
            if let Some(control) = self
                .invocation
                .as_ref()
                .and_then(|inv| inv.fixture.as_ref())
            {
                control.mark_spawn_attempted();
            }
            match OwnedChild::spawn(command, InputMode::Null) {
                Ok(child) => {
                    a.child = Some(child);
                    #[cfg(feature = "test-support")]
                    if let Some(control) = self
                        .invocation
                        .as_ref()
                        .and_then(|inv| inv.fixture.as_ref())
                    {
                        control.mark_spawn_owned();
                        control.after_owned_spawn();
                    }
                }
                Err(failure) => a.spawn_failure = Some(failure),
            }
            if a.child.is_some() && self.pending.is_none() {
                let key = a.key;
                let deadline = a.deadline;
                let accepted_at = a.accepted_at;
                self.publish(WorkerEvent::CacheUpdate(CachePatch::Phase {
                    key,
                    kind: v2::OperationKind::Run,
                    phase: LivePhase::Running,
                    accepted_at: Some(accepted_at),
                    deadline: Some(deadline),
                }));
            }
        }
        let a = self.active.as_mut().unwrap();
        if let Some(failure) = &mut a.spawn_failure {
            if let HostCleanup::Pending(cleanup) = &mut failure.cleanup {
                if !cleanup.try_complete().map_err(WorkerPrimary::Io)? {
                    return Ok(());
                }
                failure.cleanup = HostCleanup::Complete;
            }
            let phase = match failure.launch {
                theseus_host::process::LaunchPhase::NeverCreated => v2::LaunchPhase::NeverCreated,
                theseus_host::process::LaunchPhase::CreatedSuspended => {
                    v2::LaunchPhase::CreatedSuspended
                }
                theseus_host::process::LaunchPhase::MayRun => v2::LaunchPhase::MayRun,
            };
            let cleanup = match phase {
                v2::LaunchPhase::NeverCreated => v2::SpawnCleanup::NoChildCreated,
                _ => {
                    if cfg!(windows) {
                        v2::SpawnCleanup::WindowsProcessExitedJobEmpty
                    } else {
                        v2::SpawnCleanup::PosixLeaderReapedGroupTerminationRequested
                    }
                }
            };
            return self.finish(v2::Outcome::LaunchFailed {
                phase,
                code: v2::FailureCode::NativeFailure,
                cleanup,
            });
        }
        let a = self.active.as_mut().unwrap();
        if let Some(child) = &mut a.child {
            read_stream(
                &self.ports,
                &mut self.tokens,
                &mut a.out,
                &mut a.outstanding,
                &mut child.stdout,
                &mut a.stdout_eof,
                v2::Channel::Stdout,
            )
            .map_err(WorkerPrimary::Io)?;
            read_stream(
                &self.ports,
                &mut self.tokens,
                &mut a.out,
                &mut a.outstanding,
                &mut child.stderr,
                &mut a.stderr_eof,
                v2::Channel::Stderr,
            )
            .map_err(WorkerPrimary::Io)?;
            // Observe natural exit with reserved original child authority before cleanup.
            if a.status.is_none()
                && (expired
                    || cancelled
                    || child.child.exited_reserved().map_err(WorkerPrimary::Io)?)
            {
                if self.pending.is_none() {
                    let event = WorkerEvent::CacheUpdate(CachePatch::Phase {
                        key: a.key,
                        kind: v2::OperationKind::Run,
                        phase: LivePhase::CleanupPending,
                        accepted_at: Some(a.accepted_at),
                        deadline: Some(a.deadline),
                    });
                    match self.ports.events.try_send(event) {
                        Ok(()) => {}
                        Err(TrySendError::Full(e)) => self.pending = Some(e),
                        Err(TrySendError::Disconnected(e)) => {
                            self.pending = Some(e);
                            self.stopping = true;
                        }
                    }
                }
                child.child.terminate_owned().map_err(WorkerPrimary::Io)?;
                a.status = child
                    .child
                    .try_reap_and_release()
                    .map_err(WorkerPrimary::Io)?;
            }
            if let Some(observed_status) = a.status {
                // A single-process fixture's output reaches EOF; for other descendants close
                // owned endpoints after bounded reads and honestly mark omitted output.
                if !(a.stdout_eof && a.stderr_eof) {
                    a.cleanup_rounds = a.cleanup_rounds.saturating_add(1);
                    if a.cleanup_rounds < 8 {
                        return Ok(());
                    }
                    a.out.truncated = true;
                }
                child
                    .stdout
                    .cancel_and_complete()
                    .map_err(WorkerPrimary::Io)?;
                child
                    .stderr
                    .cancel_and_complete()
                    .map_err(WorkerPrimary::Io)?;
                let native = status(observed_status).map_err(WorkerPrimary::Io)?;
                let outcome = v2::Outcome::RunCompleted {
                    reason: a.reason,
                    status: native,
                    cleanup: cleanup_fact(),
                    stdout_bytes: a.out.stdout,
                    stderr_bytes: a.out.stderr,
                    truncated: a.out.truncated,
                };
                a.child = None;
                return self.finish(outcome);
            }
        }
        Ok(())
    }
    pub(super) fn finish(&mut self, outcome: v2::Outcome) -> Result<(), WorkerPrimary> {
        let a = self.active.as_mut().unwrap();
        // Producer boundary freezes before the physical terminal barrier. No client
        // acknowledgment or ProducerDrained participates in this persistence decision.
        let boundary = *a.boundary.get_or_insert_with(|| a.out.boundary());
        let key = a.key;
        if !a.committed && self.pending.is_none() {
            let accepted_at = a.accepted_at;
            let deadline = a.deadline;
            self.publish(WorkerEvent::CacheUpdate(CachePatch::Phase {
                key,
                kind: v2::OperationKind::Run,
                phase: LivePhase::TerminalCommitPending,
                accepted_at: Some(accepted_at),
                deadline: Some(deadline),
            }));
        }
        let a = self.active.as_mut().unwrap();
        if !a.committed {
            self.append(Entry::OperationFinished {
                lease: key.lease,
                operation: key.operation,
                outcome: outcome.clone(),
            })?;
            #[cfg(feature = "test-support")]
            if let Some(control) = self
                .invocation
                .as_ref()
                .and_then(|inv| inv.fixture.as_ref())
            {
                control.mark_terminal_committed();
            }
            self.active.as_mut().unwrap().committed = true;
            self.last_spawn = self.active.as_mut().unwrap().spawn_failure.take();
            // Only a durably completed original Run may authorize a SUT transition.
            if let (Some(service), Some(program), Some(active)) =
                (&mut self.service, &self.program, &self.active)
            {
                service.note_sut(
                    program,
                    active.key.lease,
                    active.key.operation,
                    active.request_digest,
                    &outcome,
                );
            }
            if let (Some(desktop), Some(program), Some(active), Some(store)) = (
                &mut self.desktop,
                &self.program,
                &self.active,
                &mut self.store,
            ) {
                desktop
                    .note_sut(
                        store,
                        program,
                        active.key.lease,
                        active.key.operation,
                        active.request_digest,
                        &outcome,
                    )
                    .map_err(WorkerPrimary::Io)?;
            }
            self.active.as_mut().unwrap().outcome = Some(outcome);
        }
        if self.pending.is_none() {
            let a = self.active.as_mut().unwrap();
            let view = v2::OperationView::Finished {
                outcome: a.outcome.take().unwrap(),
            };
            a.terminal = true;
            self.publish(WorkerEvent::CacheUpdate(CachePatch::Terminal {
                key,
                view,
                boundary,
            }));
        }
        Ok(())
    }
    pub(super) fn settle_native(&mut self) {
        if let Some(desktop) = &mut self.desktop {
            while !desktop.poll_settle() {
                thread::sleep(Duration::from_millis(1));
            }
        }
        if self.service.as_ref().is_some_and(|s| s.unsettled())
            || self.prefix_mapping_error.is_some()
            || self
                .file
                .as_ref()
                .is_some_and(|f| f.owner.sticky_open || f.owner.opening)
        {
            while !self.flush() {
                thread::sleep(Duration::from_millis(1));
            }
            self.publish(WorkerEvent::CacheUpdate(CachePatch::Stopped {
                store_poisoned: self.poisoned(),
                native_settled: false,
            }));
            while !self.flush() {
                thread::sleep(Duration::from_millis(1));
            }
            // HostDir may retain lower-level pending native authority. Keep the
            // whole worker alive until the existing process shutdown budget fires.
            loop {
                std::hint::black_box(&self.file);
                thread::park();
            }
        }

        // Retain every authority on errors and retry; completion never means detached.
        loop {
            let mut complete = true;
            if let Some(a) = &mut self.active {
                if let Some(child) = &mut a.child {
                    let stdout = child.stdout.cancel_and_complete();
                    let stderr = child.stderr.cancel_and_complete();
                    let reap = child.child.try_reap_and_release();
                    complete = stdout.is_ok() && stderr.is_ok() && matches!(reap, Ok(Some(_)));
                    if complete {
                        a.child = None;
                    } else {
                        self.cleanup_results([stdout.err(), stderr.err(), reap.err()]);
                    }
                }
                if let Some(a) = &mut self.active
                    && let Some(f) = &mut a.spawn_failure
                    && let HostCleanup::Pending(cleanup) = &mut f.cleanup
                {
                    match cleanup.try_complete() {
                        Ok(true) => f.cleanup = HostCleanup::Complete,
                        Ok(false) => complete = false,
                        Err(e) => {
                            complete = false;
                            self.cleanup_error(e);
                        }
                    }
                }
            }
            if complete {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
}
/// Bounded nonblocking reads per stream per worker tick. One read per tick
/// capped child output at one 4 KiB read per sleep; small native pipes (macOS
/// 16 KiB) then throttled the child far below the observed deadline budget.
const READS_PER_TICK: usize = 16;
fn read_stream(
    ports: &WorkerPorts,
    tokens: &mut Vec<DataPermit>,
    out: &mut OutputState,
    outstanding: &mut [Option<u32>; 4],
    reader: &mut PipeReader,
    eof: &mut bool,
    channel: v2::Channel,
) -> io::Result<()> {
    for read in 0..READS_PER_TICK {
        // The first read keeps the original per-tick behavior. Later reads
        // never discard a still-complete transcript for want of a permit:
        // pipe backpressure waits for permits returned before the next tick.
        if read > 0 && tokens.is_empty() && !out.discard_preserves_completeness() {
            break;
        }
        if !read_stream_once(ports, tokens, out, outstanding, reader, eof, channel)? {
            break;
        }
    }
    Ok(())
}
/// Returns whether child bytes were read.
fn read_stream_once(
    ports: &WorkerPorts,
    tokens: &mut Vec<DataPermit>,
    out: &mut OutputState,
    outstanding: &mut [Option<u32>; 4],
    reader: &mut PipeReader,
    eof: &mut bool,
    channel: v2::Channel,
) -> io::Result<bool> {
    if *eof {
        return Ok(false);
    }
    let mut discard = [0; 4096];
    let n = match reader.try_read(&mut discard) {
        Ok(0) => {
            *eof = true;
            return Ok(false);
        }
        Ok(n) => n,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
        Err(e) => return Err(e),
    };
    let (offset, retain) = out.observe(channel, n, !tokens.is_empty())?;
    if retain == 0 {
        #[cfg(feature = "test-support")]
        if tokens.is_empty()
            && let Some(control) = &out.fixture
        {
            // Nonempty child bytes are actually discarded without a permit.
            control.mark_capture_saturated();
        }
        return Ok(true);
    }
    let mut permit = tokens.pop().unwrap();
    permit.storage[..retain].copy_from_slice(&discard[..retain]);
    let chunk = DataChunk {
        key: out.key,
        ordinal: out.ordinal + 1,
        channel,
        offset,
        len: retain as u16,
        permit,
    };
    let id = chunk.permit.id as usize;
    match ports.data.try_send(chunk) {
        Ok(()) => {
            out.ordinal += 1;
            outstanding[id] = Some(out.ordinal);
        }
        Err(TrySendError::Full(chunk)) => {
            out.truncated = true;
            tokens.push(chunk.permit)
        }
        Err(TrySendError::Disconnected(chunk)) => {
            out.truncated = true;
            tokens.push(chunk.permit)
        }
    }
    Ok(true)
}
