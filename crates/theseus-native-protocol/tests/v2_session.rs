use theseus_native_protocol::v2::*;
#[test]
fn new_guard_requires_both_hellos_and_description() {
    let mut guard = SessionGuard::new(SessionId::from_bytes([0; 16]), Role::Controller);
    assert!(!guard.is_established());
    assert!(!guard.is_ready());
    assert!(!guard.is_closed());
    let env = Envelope::new(
        SessionId::from_bytes([0; 16]),
        0,
        Message::Hello {
            role: Role::Controller,
        },
    )
    .unwrap();
    guard.accept_outbound(&env).unwrap();
    let env = Envelope::new(
        SessionId::from_bytes([0; 16]),
        0,
        Message::Hello { role: Role::Helper },
    )
    .unwrap();
    guard.accept_inbound(&env).unwrap();
    assert!(guard.is_established());
    assert!(!guard.is_ready());
}

fn sid() -> SessionId {
    SessionId::from_bytes([0; 16])
}
fn lease() -> LeaseId {
    LeaseId::from_bytes([0x22; 16])
}
fn op() -> OperationId {
    OperationId::from_bytes([0x33; 16])
}
fn path() -> LeafName {
    LeafName::new("probe.bin").unwrap()
}
fn run() -> Request {
    Request::Run {
        lease: lease(),
        operation: op(),
        program: ProgramId::new("probe_blocked").unwrap(),
    }
}
fn query() -> Request {
    Request::QueryOperation {
        lease: lease(),
        operation: op(),
    }
}
fn cancel() -> Request {
    Request::Cancel {
        lease: lease(),
        operation: op(),
    }
}
fn create() -> Request {
    Request::CreateFile {
        lease: lease(),
        operation: op(),
        path: path(),
        bytes: FileBytes::new(&[0, 255]).unwrap(),
    }
}
fn description(os: Os) -> Reply {
    Reply::Description {
        binding: BindingId::from_bytes([0x11; 16]),
        manifest_digest: Digest::from_bytes([0xaa; 32]),
        workspace_marker: WorkspaceMarker::from_bytes([0x99; 16]),
        os,
        helper_principal: if os == Os::Windows {
            Principal::Windows {
                token_user_sid: SidText::new("S-1-5").unwrap(),
            }
        } else {
            Principal::Posix {
                effective_uid: 0,
                effective_gid: 0,
            }
        },
        programs: ProgramList::new(vec![ProgramId::new("probe_blocked").unwrap()]).unwrap(),
    }
}
fn operation(state: OperationView) -> Reply {
    Reply::Operation {
        lease: lease(),
        operation: op(),
        state,
    }
}
fn running() -> OperationView {
    OperationView::Running
}
fn accepted() -> OperationView {
    OperationView::Accepted {
        operation_kind: OperationKind::Run,
    }
}
fn completed(stdout_bytes: u64, stderr_bytes: u64, truncated: bool) -> Outcome {
    Outcome::RunCompleted {
        reason: RunReason::CancelRequested,
        status: NativeStatus::PosixExit { code: 0 },
        cleanup: Cleanup::PosixLeaderReapedGroupTerminationRequested,
        stdout_bytes,
        stderr_bytes,
        truncated,
    }
}
fn launch(phase: LaunchPhase) -> Outcome {
    Outcome::LaunchFailed {
        phase,
        code: FailureCode::Io,
        cleanup: if phase == LaunchPhase::NeverCreated {
            SpawnCleanup::NoChildCreated
        } else {
            SpawnCleanup::PosixLeaderReapedGroupTerminationRequested
        },
    }
}
fn not_started() -> Outcome {
    Outcome::NotStarted {
        reason: NotStartedReason::CancelRequested,
    }
}
fn finished(outcome: Outcome) -> OperationView {
    OperationView::Finished { outcome }
}
fn unknown() -> OperationView {
    OperationView::Unknown {
        reason: UnknownReason::ProcessStateUnproved,
    }
}
fn output(channel: Channel, offset: u64, bytes: &[u8]) -> Message {
    Message::Output {
        lease: lease(),
        operation: op(),
        channel,
        offset,
        bytes: FileBytes::new(bytes).unwrap(),
    }
}
fn log(code: LogCode) -> Message {
    Message::Log {
        lease: lease(),
        operation: op(),
        code,
    }
}
struct Trace {
    guard: SessionGuard,
    local: Role,
    c: u64,
    h: u64,
}
impl Trace {
    fn new(local: Role) -> Self {
        Self {
            guard: SessionGuard::new(sid(), local),
            local,
            c: 0,
            h: 0,
        }
    }
    fn send(&mut self, sender: Role, message: Message) -> Result<(), Error> {
        let next = if sender == Role::Controller {
            &mut self.c
        } else {
            &mut self.h
        };
        let env = Envelope::new(sid(), *next, message).unwrap();
        *next += 1;
        if sender == self.local {
            self.guard.accept_outbound(&env)
        } else {
            self.guard.accept_inbound(&env)
        }
    }
    fn hellos(&mut self) {
        self.send(
            Role::Controller,
            Message::Hello {
                role: Role::Controller,
            },
        )
        .unwrap();
        self.send(Role::Helper, Message::Hello { role: Role::Helper })
            .unwrap();
    }
    fn req(&mut self, request: Request) -> u64 {
        let seq = self.c;
        self.send(Role::Controller, Message::Request { request })
            .unwrap();
        seq
    }
    fn reply(&mut self, reply_to: u64, result: Reply) -> Result<(), Error> {
        self.send(Role::Helper, Message::Response { reply_to, result })
    }
    fn ready(local: Role, os: Os) -> Self {
        let mut s = Self::new(local);
        s.hellos();
        let seq = s.req(Request::Describe);
        s.reply(seq, description(os)).unwrap();
        assert!(s.guard.is_ready());
        s
    }
    fn activate(&mut self, req: Request, state: OperationView) {
        let seq = self.req(req);
        self.reply(seq, operation(state)).unwrap();
    }
    fn finish(&mut self, outcome: Outcome) -> Result<(), Error> {
        let seq = self.req(query());
        self.reply(seq, operation(finished(outcome)))
    }
    fn terminal_failure(&mut self, actual: Result<(), Error>, expected: Error) {
        assert_eq!(actual, Err(expected));
        assert!(self.guard.is_closed());
        assert!(!self.guard.is_established());
        assert!(!self.guard.is_ready());
        let bad = Envelope::new(
            SessionId::from_bytes([9; 16]),
            u64::MAX,
            Message::Hello { role: Role::Helper },
        )
        .unwrap();
        assert_eq!(self.guard.accept_inbound(&bad), Err(Error::Closed));
        assert_eq!(self.guard.accept_outbound(&bad), Err(Error::Closed));
    }
}

#[test]
fn handshake_preflight_direction_sequence_and_closed_precedence() {
    for local in [Role::Controller, Role::Helper] {
        for bad in [
            Message::Hello { role: Role::Helper },
            Message::Closed {
                code: CloseCode::Normal,
            },
            Message::Request {
                request: Request::Describe,
            },
        ] {
            let mut t = Trace::new(local);
            let result = t.send(Role::Controller, bad);
            t.terminal_failure(result, Error::UnexpectedMessage);
        }
        let mut t = Trace::new(local);
        t.send(
            Role::Controller,
            Message::Hello {
                role: Role::Controller,
            },
        )
        .unwrap();
        let result = t.send(
            Role::Controller,
            Message::Request {
                request: Request::Describe,
            },
        );
        t.terminal_failure(result, Error::UnexpectedMessage);
        for sequence in [1, u64::MAX] {
            let mut t = Trace::new(local);
            let e = Envelope::new(sid(), sequence, Message::Hello { role: local }).unwrap();
            let result = t.guard.accept_outbound(&e);
            t.terminal_failure(result, Error::UnexpectedSequence);
        }
        let mut t = Trace::ready(local, Os::Linux);
        let e = Envelope::new(sid(), 0, Message::Hello { role: local }).unwrap();
        let result = t.guard.accept_outbound(&e);
        t.terminal_failure(result, Error::UnexpectedSequence);
        let mut t = Trace::new(local);
        let e = Envelope::new(
            SessionId::from_bytes([9; 16]),
            999,
            Message::Hello { role: local },
        )
        .unwrap();
        let result = t.guard.accept_outbound(&e);
        t.terminal_failure(result, Error::SessionMismatch);
        for sender in [Role::Controller, Role::Helper] {
            let mut t = Trace::ready(local, Os::Linux);
            t.req(run());
            t.send(
                sender,
                Message::Closed {
                    code: CloseCode::Normal,
                },
            )
            .unwrap();
            assert!(t.guard.is_closed());
        }
        for sender in [Role::Controller, Role::Helper] {
            let mut t = Trace::ready(local, Os::Linux);
            let message = if sender == Role::Helper {
                Message::Request {
                    request: Request::Describe,
                }
            } else {
                Message::Response {
                    reply_to: 1,
                    result: description(Os::Linux),
                }
            };
            let result = t.send(sender, message);
            t.terminal_failure(result, Error::UnexpectedMessage);
        }
    }
}

#[test]
fn describe_is_mandatory_one_outstanding_refusal_consumes_and_all_fields_are_immutable() {
    for local in [Role::Controller, Role::Helper] {
        for bad in [
            Message::Request { request: run() },
            output(Channel::Stdout, 0, b"a"),
            log(LogCode::CleanupPending),
        ] {
            let mut t = Trace::new(local);
            t.hellos();
            let sender = if matches!(bad, Message::Request { .. }) {
                Role::Controller
            } else {
                Role::Helper
            };
            let result = t.send(sender, bad);
            t.terminal_failure(result, Error::UnexpectedMessage);
        }
        let mut t = Trace::new(local);
        t.hellos();
        t.req(Request::Describe);
        let result = t.send(
            Role::Controller,
            Message::Request {
                request: Request::Describe,
            },
        );
        t.terminal_failure(result, Error::UnexpectedMessage);
        let mut t = Trace::new(local);
        t.hellos();
        let seq = t.req(Request::Describe);
        t.reply(
            seq,
            Reply::Refused {
                code: RefusalCode::Busy,
                inspect_failure: None,
            },
        )
        .unwrap();
        assert!(!t.guard.is_ready());
        let seq = t.req(Request::Describe);
        t.reply(seq, description(Os::Linux)).unwrap();
        let seq = t.req(Request::Describe);
        t.reply(seq, description(Os::Linux)).unwrap();
        assert!(t.guard.is_ready());
        for field in 0..7 {
            let mut t = Trace::ready(local, Os::Linux);
            let mut changed = description(Os::Linux);
            if let Reply::Description {
                binding,
                manifest_digest,
                workspace_marker,
                os,
                helper_principal,
                programs,
            } = &mut changed
            {
                match field {
                    0 => *binding = BindingId::from_bytes([1; 16]),
                    1 => *manifest_digest = Digest::from_bytes([1; 32]),
                    2 => *workspace_marker = WorkspaceMarker::from_bytes([1; 16]),
                    3 => *os = Os::Macos,
                    4 => {
                        *helper_principal = Principal::Posix {
                            effective_uid: 1,
                            effective_gid: 0,
                        }
                    }
                    5 => {
                        *helper_principal = Principal::Posix {
                            effective_uid: 0,
                            effective_gid: 1,
                        }
                    }
                    _ => {
                        *programs =
                            ProgramList::new(vec![ProgramId::new("different").unwrap()]).unwrap()
                    }
                }
            }
            let seq = t.req(Request::Describe);
            let result = t.reply(seq, changed);
            t.terminal_failure(result, Error::UnexpectedMessage);
        }
    }
}

#[test]
fn four_pending_requests_out_of_order_exactly_once_and_refused() {
    let mut t = Trace::ready(Role::Controller, Os::Linux);
    let seqs: Vec<_> = (0..4).map(|_| t.req(query())).collect();
    for &seq in seqs.iter().rev() {
        t.reply(
            seq,
            Reply::Refused {
                code: RefusalCode::Busy,
                inspect_failure: None,
            },
        )
        .unwrap();
    }
    let seq = t.req(query());
    t.reply(seq, operation(OperationView::Missing)).unwrap();
    let result = t.reply(seq, operation(OperationView::Missing));
    t.terminal_failure(result, Error::UnexpectedReply);
    let mut t = Trace::ready(Role::Helper, Os::Linux);
    for _ in 0..4 {
        t.req(query());
    }
    let result = t.send(Role::Controller, Message::Request { request: query() });
    t.terminal_failure(result, Error::TooManyRequests);
    for reply_to in [0, 999] {
        let mut t = Trace::ready(Role::Controller, Os::Linux);
        t.req(query());
        let result = t.reply(reply_to, operation(OperationView::Missing));
        t.terminal_failure(result, Error::UnexpectedReply);
    }
}

#[test]
fn every_reply_correlates_kind_ids_path_offset_and_bound() {
    let intent = IntentId::from_bytes([4; 16]);
    let binding = BindingId::from_bytes([5; 16]);
    let digest = Digest::from_bytes([6; 32]);
    let requests = vec![
        Request::Describe,
        Request::Acquire {
            intent,
            binding,
            manifest_digest: digest,
        },
        Request::QueryAcquire {
            intent,
            binding,
            manifest_digest: digest,
        },
        Request::Release { lease: lease() },
        Request::QueryLease { lease: lease() },
        run(),
        create(),
        query(),
        cancel(),
        Request::ReadFile {
            lease: lease(),
            path: path(),
            offset: 7,
            length: 2,
        },
        Request::InspectFile {
            lease: lease(),
            path: path(),
        },
    ];
    for request in requests {
        let mut t = Trace::ready(Role::Controller, Os::Linux);
        let seq = t.req(request);
        let result = t.reply(
            seq,
            Reply::FileInfo {
                lease: LeaseId::from_bytes([8; 16]),
                path: path(),
                length: 0,
                digest,
            },
        );
        t.terminal_failure(result, Error::UnexpectedReply);
    }
    for acquire in [false, true] {
        for wrong in 0..4 {
            let request = if acquire {
                Request::Acquire {
                    intent,
                    binding,
                    manifest_digest: digest,
                }
            } else {
                Request::QueryAcquire {
                    intent,
                    binding,
                    manifest_digest: digest,
                }
            };
            let reply = Reply::Acquired {
                intent: if wrong == 0 {
                    IntentId::from_bytes([7; 16])
                } else {
                    intent
                },
                binding: if wrong == 1 {
                    BindingId::from_bytes([7; 16])
                } else {
                    binding
                },
                manifest_digest: if wrong == 2 {
                    Digest::from_bytes([7; 32])
                } else {
                    digest
                },
                state: AcquireView::NotFound,
            };
            let mut t = Trace::ready(Role::Controller, Os::Linux);
            let seq = t.req(request);
            let result = t.reply(seq, reply);
            if !acquire && wrong == 3 {
                result.unwrap()
            } else {
                t.terminal_failure(result, Error::UnexpectedReply);
            }
        }
    }
    for wrong in 0..5 {
        let mut t = Trace::ready(Role::Helper, Os::Linux);
        let seq = t.req(Request::ReadFile {
            lease: lease(),
            path: path(),
            offset: 7,
            length: 2,
        });
        let result = t.reply(
            seq,
            Reply::FileBytes {
                lease: if wrong == 0 {
                    LeaseId::from_bytes([8; 16])
                } else {
                    lease()
                },
                path: if wrong == 1 {
                    LeafName::new("other").unwrap()
                } else {
                    path()
                },
                offset: if wrong == 2 { 8 } else { 7 },
                bytes: FileBytes::new(if wrong == 3 { b"abc" } else { b"ab" }).unwrap(),
                eof: false,
            },
        );
        if wrong == 4 {
            result.unwrap()
        } else {
            t.terminal_failure(result, Error::UnexpectedReply);
        }
    }
    for wrong in 0..2 {
        let mut t = Trace::ready(Role::Controller, Os::Linux);
        let seq = t.req(query());
        let result = t.reply(
            seq,
            Reply::Operation {
                lease: if wrong == 0 {
                    LeaseId::from_bytes([8; 16])
                } else {
                    lease()
                },
                operation: if wrong == 1 {
                    OperationId::from_bytes([8; 16])
                } else {
                    op()
                },
                state: OperationView::Missing,
            },
        );
        t.terminal_failure(result, Error::UnexpectedReply);
    }
}

#[test]
fn create_file_uses_actual_payload_sha_only_for_file_created_and_correct_kind() {
    // Independently calculated SHA256 of bytes 00ff (not the canonical request digest).
    let correct =
        Digest::parse("06eb7d6a69ee19e5fbdf749018d3d2abfa04bcbd1365db312eb86dc7169389b8").unwrap();
    for outcome in [
        Outcome::FileCreated {
            length: 2,
            digest: correct,
        },
        not_started(),
        Outcome::NotStarted {
            reason: NotStartedReason::Deadline,
        },
        Outcome::FileCreateFailed {
            code: FileCreateFailureCode::AlreadyExists,
        },
        Outcome::FileCreateFailed {
            code: FileCreateFailureCode::Permission,
        },
    ] {
        let mut t = Trace::ready(Role::Controller, Os::Linux);
        let seq = t.req(create());
        t.reply(seq, operation(finished(outcome))).unwrap();
    }
    for outcome in [
        Outcome::FileCreated {
            length: 1,
            digest: correct,
        },
        Outcome::FileCreated {
            length: 2,
            digest: Digest::from_bytes([0; 32]),
        },
        completed(0, 0, false),
        launch(LaunchPhase::NeverCreated),
    ] {
        let mut t = Trace::ready(Role::Controller, Os::Linux);
        let seq = t.req(create());
        let result = t.reply(seq, operation(finished(outcome)));
        t.terminal_failure(result, Error::UnexpectedReply);
    }
    for state in [
        OperationView::Missing,
        running(),
        accepted(),
        finished(Outcome::FileCreated {
            length: 2,
            digest: correct,
        }),
        finished(Outcome::FileCreateFailed {
            code: FileCreateFailureCode::Permission,
        }),
    ] {
        let request = if matches!(
            state,
            OperationView::Running | OperationView::Accepted { .. }
        ) {
            create()
        } else {
            run()
        };
        let mut t = Trace::ready(Role::Controller, Os::Linux);
        let seq = t.req(request);
        let result = t.reply(seq, operation(state));
        t.terminal_failure(result, Error::UnexpectedReply);
    }
}

#[test]
fn outcomes_always_match_fixed_native_os_including_query_and_cancel() {
    for os in [Os::Linux, Os::Macos, Os::Windows] {
        for request in [run(), query(), cancel()] {
            for windows in [false, true] {
                let outcome = if windows {
                    Outcome::RunCompleted {
                        reason: RunReason::Natural,
                        status: NativeStatus::WindowsExit { code: u32::MAX },
                        cleanup: Cleanup::WindowsProcessExitedJobEmpty,
                        stdout_bytes: 0,
                        stderr_bytes: 0,
                        truncated: false,
                    }
                } else {
                    completed(0, 0, false)
                };
                let mut t = Trace::ready(Role::Helper, os);
                let seq = t.req(request.clone());
                let result = t.reply(seq, operation(finished(outcome)));
                if (os == Os::Windows) == windows {
                    result.unwrap()
                } else {
                    t.terminal_failure(result, Error::InvalidValue);
                }
            }
        }
    }
    for os in [Os::Linux, Os::Macos, Os::Windows] {
        for cleanup in [
            SpawnCleanup::PosixLeaderReapedGroupTerminationRequested,
            SpawnCleanup::WindowsProcessExitedJobEmpty,
        ] {
            let mut t = Trace::ready(Role::Controller, os);
            let seq = t.req(query());
            let result = t.reply(
                seq,
                operation(finished(Outcome::LaunchFailed {
                    phase: LaunchPhase::MayRun,
                    code: FailureCode::Io,
                    cleanup,
                })),
            );
            if (os == Os::Windows) == (cleanup == SpawnCleanup::WindowsProcessExitedJobEmpty) {
                result.unwrap()
            } else {
                t.terminal_failure(result, Error::InvalidValue);
            }
        }
    }
}

#[test]
fn stream_activation_ids_active_operation_and_terminal_boundaries() {
    for message in [
        output(Channel::Stdout, 0, b"a"),
        log(LogCode::CleanupPending),
    ] {
        let mut t = Trace::ready(Role::Controller, Os::Linux);
        let result = t.send(Role::Helper, message);
        t.terminal_failure(result, Error::UnexpectedMessage);
    }
    for wrong in 0..2 {
        let mut t = Trace::ready(Role::Helper, Os::Linux);
        t.activate(run(), accepted());
        let message = Message::Output {
            lease: if wrong == 0 {
                LeaseId::from_bytes([7; 16])
            } else {
                lease()
            },
            operation: if wrong == 1 {
                OperationId::from_bytes([7; 16])
            } else {
                op()
            },
            channel: Channel::Stdout,
            offset: 0,
            bytes: FileBytes::new(b"a").unwrap(),
        };
        let result = t.send(Role::Helper, message);
        t.terminal_failure(result, Error::UnexpectedMessage);
    }
    let mut t = Trace::ready(Role::Controller, Os::Linux);
    t.activate(run(), accepted());
    let seq = t.req(Request::Run {
        lease: lease(),
        operation: OperationId::from_bytes([8; 16]),
        program: ProgramId::new("probe_blocked").unwrap(),
    });
    let result = t.reply(
        seq,
        Reply::Operation {
            lease: lease(),
            operation: OperationId::from_bytes([8; 16]),
            state: running(),
        },
    );
    t.terminal_failure(result, Error::UnexpectedMessage);
    let mut t = Trace::ready(Role::Controller, Os::Linux);
    t.activate(run(), accepted());
    let cancel = t.req(cancel());
    t.send(Role::Helper, output(Channel::Stdout, 0, b"a"))
        .unwrap();
    t.reply(cancel, operation(running())).unwrap();
    t.finish(completed(1, 0, false)).unwrap();
    let result = t.send(Role::Helper, log(LogCode::CleanupPending));
    t.terminal_failure(result, Error::UnexpectedMessage);
}

#[test]
fn fresh_resumed_channel_baselines_gaps_counters_and_refresh_history() {
    for request in [run(), query(), cancel()] {
        for truncated in [false, true] {
            let fresh = matches!(request, Request::Run { .. });
            let mut t = Trace::ready(Role::Controller, Os::Linux);
            t.activate(request.clone(), running());
            t.send(Role::Helper, output(Channel::Stdout, 40, b"ab"))
                .unwrap();
            t.send(Role::Helper, output(Channel::Stderr, 80, b"c"))
                .unwrap();
            let result = t.finish(completed(42, 81, truncated));
            if fresh && !truncated {
                t.terminal_failure(result, Error::UnexpectedMessage);
            } else {
                result.unwrap();
            }
        }
    }
    for request in [run(), query(), cancel()] {
        let mut t = Trace::ready(Role::Controller, Os::Linux);
        t.activate(run(), accepted());
        t.send(Role::Helper, output(Channel::Stdout, 0, b"ab"))
            .unwrap();
        t.activate(request, running());
        let result = t.send(Role::Helper, output(Channel::Stdout, 1, b"c"));
        t.terminal_failure(result, Error::UnexpectedMessage);
    }
    for truncated in [false, true] {
        let mut t = Trace::ready(Role::Controller, Os::Linux);
        t.activate(cancel(), running());
        t.send(Role::Helper, output(Channel::Stdout, 40, b"a"))
            .unwrap();
        t.send(Role::Helper, output(Channel::Stdout, 42, b"b"))
            .unwrap();
        t.activate(query(), accepted());
        let result = t.finish(completed(43, 0, truncated));
        if truncated {
            result.unwrap()
        } else {
            t.terminal_failure(result, Error::UnexpectedMessage);
        }
    }
    for (out, err) in [(0, 0), (1, 0), (2, 1)] {
        let mut t = Trace::ready(Role::Controller, Os::Linux);
        t.activate(run(), running());
        t.send(Role::Helper, output(Channel::Stdout, 0, b"ab"))
            .unwrap();
        t.send(Role::Helper, output(Channel::Stderr, 0, b"ab"))
            .unwrap();
        let result = t.finish(completed(out, err, true));
        t.terminal_failure(result, Error::UnexpectedMessage);
    }
    let mut t = Trace::ready(Role::Controller, Os::Linux);
    t.activate(cancel(), running());
    t.send(Role::Helper, output(Channel::Stdout, u64::MAX - 1, b"a"))
        .unwrap();
    t.finish(completed(u64::MAX, 0, false)).unwrap();
    assert_eq!(
        Envelope::new(sid(), 3, output(Channel::Stdout, u64::MAX, b"a")),
        Err(Error::InvalidValue)
    );
}

#[test]
fn total_decoded_budget_and_once_logs_survive_every_status_refresh() {
    assert_eq!(FileBytes::new(&[0; 4097]), Err(Error::LimitExceeded));
    assert_eq!(
        Envelope::new(sid(), 3, output(Channel::Stdout, 0, b"")),
        Err(Error::InvalidValue)
    );
    for request in [run(), query(), cancel()] {
        let mut t = Trace::ready(Role::Controller, Os::Linux);
        t.activate(run(), accepted());
        for i in 0..4 {
            let channel = if i % 2 == 0 {
                Channel::Stdout
            } else {
                Channel::Stderr
            };
            t.send(Role::Helper, output(channel, (i / 2) * 4096, &[0; 4096]))
                .unwrap();
            t.activate(request.clone(), running());
        }
        let result = t.send(Role::Helper, output(Channel::Stdout, 8192, b"a"));
        t.terminal_failure(result, Error::LimitExceeded);
    }
    for code in [
        LogCode::OutputTruncated,
        LogCode::CancelRequested,
        LogCode::DeadlineExpired,
        LogCode::CleanupPending,
    ] {
        for request in [run(), query(), cancel()] {
            let mut t = Trace::ready(Role::Controller, Os::Linux);
            t.activate(run(), accepted());
            t.send(Role::Helper, log(code)).unwrap();
            t.activate(request, running());
            let result = t.send(Role::Helper, log(code));
            t.terminal_failure(result, Error::UnexpectedMessage);
        }
    }
}

#[test]
fn counterless_terminal_claims_require_no_observed_output_or_truncation() {
    for evidence in 0..3 {
        for outcome in [
            not_started(),
            launch(LaunchPhase::NeverCreated),
            launch(LaunchPhase::MayRun),
        ] {
            let mut t = Trace::ready(Role::Controller, Os::Linux);
            t.activate(run(), running());
            match evidence {
                0 => t
                    .send(Role::Helper, output(Channel::Stdout, 0, b"a"))
                    .unwrap(),
                1 => t.send(Role::Helper, log(LogCode::OutputTruncated)).unwrap(),
                _ => t
                    .send(Role::Helper, output(Channel::Stderr, 5, b"a"))
                    .unwrap(),
            };
            t.activate(query(), accepted());
            let result = t.finish(outcome);
            t.terminal_failure(result, Error::UnexpectedMessage);
        }
    }
    for outcome in [
        not_started(),
        launch(LaunchPhase::NeverCreated),
        launch(LaunchPhase::MayRun),
    ] {
        let mut t = Trace::ready(Role::Controller, Os::Linux);
        t.activate(run(), accepted());
        t.send(Role::Helper, log(LogCode::CancelRequested)).unwrap();
        t.finish(outcome).unwrap();
    }
    let mut t = Trace::ready(Role::Controller, Os::Linux);
    t.activate(run(), running());
    t.send(Role::Helper, output(Channel::Stdout, 10, b"a"))
        .unwrap();
    let seq = t.req(query());
    t.reply(seq, operation(unknown())).unwrap();
    let seq = t.req(cancel());
    t.reply(seq, operation(unknown())).unwrap();
    let result = t.send(Role::Helper, output(Channel::Stdout, 11, b"a"));
    t.terminal_failure(result, Error::UnexpectedMessage);
}

#[test]
fn terminal_cache_equal_queries_missing_changes_resurrection_and_capacity() {
    for terminal in [finished(completed(0, 0, false)), unknown()] {
        for later in [
            OperationView::Missing,
            accepted(),
            running(),
            finished(not_started()),
            unknown(),
        ] {
            let mut t = Trace::ready(Role::Controller, Os::Linux);
            let seq = t.req(query());
            t.reply(seq, operation(terminal.clone())).unwrap();
            let seq = t.req(cancel());
            let result = t.reply(seq, operation(later.clone()));
            if terminal == later {
                result.unwrap()
            } else {
                t.terminal_failure(result, Error::UnexpectedMessage);
            }
        }
    }
    let mut t = Trace::ready(Role::Controller, Os::Linux);
    for n in 0..128 {
        let id = OperationId::from_bytes([n; 16]);
        let seq = t.req(Request::QueryOperation {
            lease: lease(),
            operation: id,
        });
        t.reply(
            seq,
            Reply::Operation {
                lease: lease(),
                operation: id,
                state: unknown(),
            },
        )
        .unwrap();
    }
    let seq = t.req(Request::QueryOperation {
        lease: lease(),
        operation: OperationId::from_bytes([0; 16]),
    });
    t.reply(
        seq,
        Reply::Operation {
            lease: lease(),
            operation: OperationId::from_bytes([0; 16]),
            state: unknown(),
        },
    )
    .unwrap();
    let seq = t.req(Request::QueryOperation {
        lease: lease(),
        operation: OperationId::from_bytes([128; 16]),
    });
    let result = t.reply(
        seq,
        Reply::Operation {
            lease: lease(),
            operation: OperationId::from_bytes([128; 16]),
            state: unknown(),
        },
    );
    t.terminal_failure(result, Error::LimitExceeded);
    // A new connection has no previous-connection truth; durable replay owns it.
    let mut t = Trace::ready(Role::Controller, Os::Linux);
    t.activate(query(), running());
}

// Independently constructed typed translations of all nine round1 external
// literal traces. No production encoder/decoder constructs these messages.
#[test]
fn nine_independent_round1_guard_oracle_traces() {
    let names = [
        "cancel_first_accepted_is_resumed_and_refresh_preserves_offsets",
        "cancel_first_running_is_resumed_refresh_cannot_reset_offsets",
        "output_then_never_created_is_invalid",
        "output_then_may_run_launch_failed_is_invalid",
        "truncation_log_without_output_then_not_started_is_invalid",
        "truncation_log_without_output_then_launch_failed_is_invalid",
        "silent_may_run_launch_failed_is_structurally_valid_not_a_no_effect_claim",
        "silent_not_started_is_structurally_valid_runtime_must_prove_no_attempt",
        "output_with_missing_terminal_facts_can_become_unknown_never_resurrect_stream",
    ];
    for (case, name) in names.iter().enumerate() {
        let mut t = Trace::ready(Role::Controller, Os::Linux);
        assert_eq!((t.c, t.h), (2, 2), "{name}");
        t.activate(
            cancel(),
            if matches!(case, 0 | 4 | 7) {
                accepted()
            } else {
                running()
            },
        );
        match case {
            0 | 1 | 2 | 3 | 8 => t
                .send(Role::Helper, output(Channel::Stdout, 40, b"ab"))
                .unwrap(),
            4 | 5 => t.send(Role::Helper, log(LogCode::OutputTruncated)).unwrap(),
            _ => (),
        }
        if case <= 1 {
            t.activate(query(), running());
            let result = t.send(
                Role::Helper,
                output(Channel::Stdout, if case == 0 { 42 } else { 41 }, b"c"),
            );
            if case == 1 {
                t.terminal_failure(result, Error::UnexpectedMessage);
                assert_eq!(
                    t.send(Role::Helper, output(Channel::Stdout, 42, b"c")),
                    Err(Error::Closed),
                    "{name}"
                );
            } else {
                result.unwrap();
                t.finish(completed(43, 0, false)).unwrap();
                assert_eq!((t.c, t.h), (5, 7));
            }
        } else {
            let seq = t.req(query());
            let state = match case {
                2 => finished(launch(LaunchPhase::NeverCreated)),
                3 | 5 | 6 => finished(launch(LaunchPhase::MayRun)),
                4 | 7 => finished(not_started()),
                8 => unknown(),
                _ => unreachable!(),
            };
            let result = t.reply(seq, operation(state));
            if (2..=5).contains(&case) {
                t.terminal_failure(result, Error::UnexpectedMessage);
            } else {
                result.unwrap();
                if case == 8 {
                    let result = t.send(Role::Helper, output(Channel::Stdout, 42, b"c"));
                    t.terminal_failure(result, Error::UnexpectedMessage);
                }
            }
        }
    }
}

#[test]
fn another_accepted_create_file_cannot_be_active_beside_run() {
    let mut t = Trace::ready(Role::Controller, Os::Linux);
    t.activate(run(), running());
    let other = OperationId::from_bytes([7; 16]);
    let seq = t.req(Request::CreateFile {
        lease: lease(),
        operation: other,
        path: path(),
        bytes: FileBytes::new(b"").unwrap(),
    });
    let result = t.reply(
        seq,
        Reply::Operation {
            lease: lease(),
            operation: other,
            state: OperationView::Accepted {
                operation_kind: OperationKind::CreateFile,
            },
        },
    );
    t.terminal_failure(result, Error::UnexpectedMessage);
}

#[test]
fn positive_correlation_for_acquisition_leases_reads_inspection_and_create_acceptance() {
    let intent = IntentId::from_bytes([4; 16]);
    let binding = BindingId::from_bytes([5; 16]);
    let manifest_digest = Digest::from_bytes([6; 32]);
    for request in [
        Request::Acquire {
            intent,
            binding,
            manifest_digest,
        },
        Request::QueryAcquire {
            intent,
            binding,
            manifest_digest,
        },
    ] {
        let mut t = Trace::ready(Role::Helper, Os::Linux);
        let seq = t.req(request);
        t.reply(
            seq,
            Reply::Acquired {
                intent,
                binding,
                manifest_digest,
                state: AcquireView::Lease {
                    lease: lease(),
                    state: LeaseView::Held { intent },
                },
            },
        )
        .unwrap();
    }
    for request in [
        Request::QueryLease { lease: lease() },
        Request::Release { lease: lease() },
    ] {
        for state in [
            LeaseView::Held { intent },
            LeaseView::AwaitingExternalReset { intent },
            LeaseView::Quarantined {
                intent,
                reason: UnknownReason::LedgerWriteUncertain,
            },
        ] {
            let mut t = Trace::ready(Role::Helper, Os::Linux);
            let seq = t.req(request.clone());
            t.reply(
                seq,
                Reply::Lease {
                    lease: lease(),
                    state,
                },
            )
            .unwrap();
        }
    }
    for (bytes, eof) in [(b"".as_slice(), true), (b"ab".as_slice(), false)] {
        let mut t = Trace::ready(Role::Helper, Os::Linux);
        let seq = t.req(Request::ReadFile {
            lease: lease(),
            path: path(),
            offset: 7,
            length: 2,
        });
        t.reply(
            seq,
            Reply::FileBytes {
                lease: lease(),
                path: path(),
                offset: 7,
                bytes: FileBytes::new(bytes).unwrap(),
                eof,
            },
        )
        .unwrap();
    }
    let mut t = Trace::ready(Role::Helper, Os::Linux);
    let seq = t.req(Request::InspectFile {
        lease: lease(),
        path: path(),
    });
    t.reply(
        seq,
        Reply::FileInfo {
            lease: lease(),
            path: path(),
            length: 4096,
            digest: manifest_digest,
        },
    )
    .unwrap();
    let mut t = Trace::ready(Role::Helper, Os::Linux);
    let seq = t.req(create());
    t.reply(
        seq,
        operation(OperationView::Accepted {
            operation_kind: OperationKind::CreateFile,
        }),
    )
    .unwrap();
    let result = t.send(Role::Helper, output(Channel::Stdout, 0, b"a"));
    t.terminal_failure(result, Error::UnexpectedMessage);
}

#[test]
fn counter_excess_alone_is_not_a_guard_delivery_completeness_claim() {
    let mut t = Trace::ready(Role::Controller, Os::Linux);
    t.activate(run(), accepted());
    t.send(Role::Helper, output(Channel::Stdout, 0, b"ab"))
        .unwrap();
    // Only explicit observed gaps/logged drops force truncated. Counters still
    // cover the observed end, but the guard does not certify complete delivery.
    t.finish(completed(99, 100, false)).unwrap();
}

#[test]
fn known_terminal_mutation_can_only_be_refused_but_new_session_can_replay_terminal() {
    for request in [run(), create()] {
        let terminal = if matches!(request, Request::Run { .. }) {
            finished(completed(0, 0, false))
        } else {
            finished(not_started())
        };
        for refuse in [false, true] {
            let mut t = Trace::ready(Role::Controller, Os::Linux);
            let seq = t.req(query());
            t.reply(seq, operation(terminal.clone())).unwrap();
            let seq = t.req(request.clone());
            let result = t.reply(
                seq,
                if refuse {
                    Reply::Refused {
                        code: RefusalCode::InvalidRequest,
                        inspect_failure: None,
                    }
                } else {
                    operation(terminal.clone())
                },
            );
            if refuse {
                result.unwrap()
            } else {
                t.terminal_failure(result, Error::UnexpectedReply);
            }
        }
        let mut t = Trace::ready(Role::Controller, Os::Linux);
        let seq = t.req(request);
        t.reply(seq, operation(terminal)).unwrap();
    }
}

fn capacity_id(n: u8) -> OperationId {
    let mut bytes = [0; 16];
    bytes[0] = n;
    OperationId::from_bytes(bytes)
}
fn query_id(t: &mut Trace, id: OperationId, state: OperationView) -> Result<(), Error> {
    let seq = t.req(Request::QueryOperation {
        lease: lease(),
        operation: id,
    });
    t.reply(
        seq,
        Reply::Operation {
            lease: lease(),
            operation: id,
            state,
        },
    )
}
fn active_run_and_127_terminal_prefix(local: Role) -> Trace {
    let mut t = Trace::ready(local, Os::Linux);
    t.activate(run(), accepted());
    t.send(Role::Helper, output(Channel::Stdout, 0, b"a"))
        .unwrap();
    for n in 0..127 {
        query_id(
            &mut t,
            capacity_id(n),
            if n % 2 == 0 {
                unknown()
            } else {
                finished(not_started())
            },
        )
        .unwrap();
    }
    t
}

#[test]
fn operation_capacity_reserves_active_run_terminal_slot_and_rejects_new_id() {
    for local in [Role::Controller, Role::Helper] {
        let mut t = active_run_and_127_terminal_prefix(local);
        let result = query_id(&mut t, capacity_id(127), unknown());
        t.terminal_failure(result, Error::LimitExceeded);
    }
}

#[test]
fn operation_capacity_allows_reserved_run_refresh_and_terminal_settlement() {
    // This is a separate trace: the rejected new ID above terminally closes its guard.
    for local in [Role::Controller, Role::Helper] {
        let mut t = active_run_and_127_terminal_prefix(local);
        t.activate(query(), running());
        t.activate(cancel(), accepted());
        t.send(Role::Helper, output(Channel::Stdout, 1, b"b"))
            .unwrap();
        t.finish(completed(2, 0, false)).unwrap();
        let seq = t.req(query());
        t.reply(seq, operation(finished(completed(2, 0, false))))
            .unwrap();
        assert!(t.guard.is_ready());
    }
}

#[test]
fn operation_capacity_counts_all_non_missing_ids_including_accepted_create_file() {
    for local in [Role::Controller, Role::Helper] {
        let mut t = Trace::ready(local, Os::Linux);
        for n in 0..128 {
            query_id(
                &mut t,
                capacity_id(n),
                OperationView::Accepted {
                    operation_kind: OperationKind::CreateFile,
                },
            )
            .unwrap();
        }
        // Missing provides no retained operation fact and must not take a slot.
        query_id(&mut t, capacity_id(128), OperationView::Missing).unwrap();
        // The already-known ID can still refresh and settle at the exact bound.
        query_id(
            &mut t,
            capacity_id(0),
            OperationView::Accepted {
                operation_kind: OperationKind::CreateFile,
            },
        )
        .unwrap();
        query_id(&mut t, capacity_id(0), finished(not_started())).unwrap();
        let result = query_id(
            &mut t,
            capacity_id(128),
            OperationView::Accepted {
                operation_kind: OperationKind::CreateFile,
            },
        );
        t.terminal_failure(result, Error::LimitExceeded);
    }
}
