use crate::{Invocation, RuntimeError};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    time::Instant,
};
use theseus_native_helper_store::StoreError;
use theseus_native_protocol::v2;
pub(crate) struct ControlPermit {
    id: u8,
} //only0..1, no Clone
pub(crate) struct DataPermit {
    pub(crate) id: u8,
    pub(crate) storage: Box<[u8; 4096]>,
} //only0..3, no Clone
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RunKey {
    pub(crate) lease: v2::LeaseId,
    pub(crate) operation: v2::OperationId,
    pub(crate) generation: u64,
}
pub(crate) struct ResponseTicket {
    pub(crate) reply_to: u64,
    pub(crate) permit: ControlPermit,
}
pub(crate) struct CancelToken {
    pub(crate) key: RunKey,
    pub(crate) flag: Arc<AtomicBool>,
}
pub(crate) struct StartCell {
    key: RunKey,
    state: AtomicU8,
} //0 Waiting,1 Granted,2 Aborted
pub(crate) struct StartGrant {
    cell: Arc<StartCell>,
    request_sequence: u64,
} //dispatcher only
pub(crate) struct StartWatch {
    cell: Arc<StartCell>,
} //worker observation only
pub(crate) struct AcceptedAdmission {
    pub(crate) key: RunKey,
    pub(crate) reply_to: u64,
    pub(crate) outbound_sequence: u64,
}
pub(crate) enum StartState {
    Waiting,
    Granted,
    Aborted,
}

pub(crate) struct DataChunk {
    pub(crate) key: RunKey,
    pub(crate) ordinal: u32,
    pub(crate) channel: v2::Channel,
    pub(crate) offset: u64,
    pub(crate) len: u16,
    pub(crate) permit: DataPermit,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FinalBoundary {
    pub(crate) key: RunKey,
    pub(crate) final_ordinal: u32,
    pub(crate) stdout_bytes: u64,
    pub(crate) stderr_bytes: u64,
    pub(crate) truncated: bool,
}
pub(crate) enum ReturnDisposition {
    TransportAcknowledged,
    AbandonedBeforeSubmit,
}
pub(crate) enum DataReturn {
    Reusable {
        key: RunKey,
        ordinal: u32,
        disposition: ReturnDisposition,
        permit: DataPermit,
    },
    ProducerDrained {
        key: RunKey,
        final_ordinal: u32,
    },
}
pub(crate) struct RecoveredOperation {
    pub(crate) lease: v2::LeaseId,
    pub(crate) operation: v2::OperationId,
    pub(crate) request_digest: v2::Digest,
    pub(crate) kind: v2::OperationKind,
    pub(crate) view: v2::OperationView,
}
pub(crate) enum LivePhase {
    AdmissionPending,
    Accepted,
    Running,
    CleanupPending,
    TerminalCommitPending,
}
pub(crate) enum CachePatch {
    Initialized {
        description: v2::Reply,
        lease: Option<(v2::IntentId, v2::LeaseId, v2::LeaseView)>,
        operations: Vec<RecoveredOperation>,
    }, //validated<=128, Description variant only
    Lease {
        intent: v2::IntentId,
        lease: v2::LeaseId,
        view: v2::LeaseView,
    },
    Phase {
        key: RunKey,
        kind: v2::OperationKind,
        phase: LivePhase,
        accepted_at: Option<Instant>,
        deadline: Option<Instant>,
    },
    Terminal {
        key: RunKey,
        view: v2::OperationView,
        boundary: FinalBoundary,
    },
    Stopped {
        store_poisoned: bool,
        native_settled: bool,
    },
}
pub(crate) enum WorkerPrimary {
    Io(io::Error),
    Store(StoreError),
    Spawn(theseus_host::process::SpawnFailure),
    Panic(Box<dyn std::any::Any + Send + 'static>),
}
pub struct OwnedWorkerFailure {
    pub(crate) primary: WorkerPrimary,
    pub(crate) retained_spawn: Option<Box<theseus_host::process::SpawnFailure>>,
    pub(crate) cleanup_errors: [Option<io::Error>; 8],
}
pub(crate) enum WorkerCompletion {
    Clean {
        store_poisoned: bool,
        native_settled: bool,
    },
    Failed(OwnedWorkerFailure),
}
pub(crate) enum WorkerEvent {
    CacheUpdate(CachePatch),
    ResponseReady {
        ticket: ResponseTicket,
        reply: v2::Reply,
        start_key: Option<RunKey>,
    },
}
pub(crate) enum WorkerCommand {
    Init(Invocation),
    Acquire {
        request: v2::Request,
        ticket: ResponseTicket,
    },
    Run {
        request: v2::Request,
        ticket: ResponseTicket,
        cancel: CancelToken,
        start: StartWatch,
    },
    Release {
        lease: v2::LeaseId,
        ticket: ResponseTicket,
    },
    Stop,
}
pub(crate) struct WorkerPorts {
    pub(crate) commands: Receiver<WorkerCommand>,
    pub(crate) events: SyncSender<WorkerEvent>,
    pub(crate) data: SyncSender<DataChunk>,
    pub(crate) returned: Receiver<DataReturn>,
    pub(crate) stop: Arc<AtomicBool>,
}
pub(crate) struct DispatcherPorts {
    pub(crate) commands: SyncSender<WorkerCommand>,
    pub(crate) events: Receiver<WorkerEvent>,
    pub(crate) data: Receiver<DataChunk>,
    pub(crate) returned: SyncSender<DataReturn>,
    pub(crate) stop: Arc<AtomicBool>,
}
impl StartGrant {
    pub(crate) fn grant(&mut self, proof: AcceptedAdmission) -> Result<(), RuntimeError> {
        if proof.key != self.cell.key
            || proof.reply_to != self.request_sequence
            || proof.outbound_sequence == 0
        {
            return Err(RuntimeError::InvalidGrant);
        }
        self.cell
            .state
            .compare_exchange(0, 1, Ordering::Release, Ordering::Relaxed)
            .map(|_| ())
            .map_err(|_| RuntimeError::InvalidGrant)
    }
    pub(crate) fn abort(&mut self) {
        let _ = self
            .cell
            .state
            .compare_exchange(0, 2, Ordering::Release, Ordering::Relaxed);
    }
}
impl StartWatch {
    pub(crate) fn poll(&self) -> StartState {
        match self.cell.state.load(Ordering::Acquire) {
            1 => StartState::Granted,
            2 => StartState::Aborted,
            _ => StartState::Waiting,
        }
    }
    pub(crate) fn key(&self) -> RunKey {
        self.cell.key
    }
}
pub(crate) fn start_pair(key: RunKey, request_sequence: u64) -> (StartGrant, StartWatch) {
    let cell = Arc::new(StartCell {
        key,
        state: AtomicU8::new(0),
    });
    (
        StartGrant {
            cell: cell.clone(),
            request_sequence,
        },
        StartWatch { cell },
    )
}
pub(crate) fn channels() -> (
    DispatcherPorts,
    WorkerPorts,
    [ControlPermit; 2],
    [DataPermit; 4],
) {
    let (commands_tx, commands_rx) = sync_channel(1);
    let (events_tx, events_rx) = sync_channel(2);
    let (data_tx, data_rx) = sync_channel(4);
    let (returned_tx, returned_rx) = sync_channel(4);
    let stop = Arc::new(AtomicBool::new(false));
    (
        DispatcherPorts {
            commands: commands_tx,
            events: events_rx,
            data: data_rx,
            returned: returned_tx,
            stop: stop.clone(),
        },
        WorkerPorts {
            commands: commands_rx,
            events: events_tx,
            data: data_tx,
            returned: returned_rx,
            stop,
        },
        std::array::from_fn(|i| ControlPermit { id: i as u8 }),
        std::array::from_fn(|i| DataPermit {
            id: i as u8,
            storage: Box::new([0; 4096]),
        }),
    )
}
impl ControlPermit {
    pub(crate) fn id(&self) -> u8 {
        self.id
    }
}
impl OwnedWorkerFailure {
    pub fn spawn_failure(&self) -> Option<&theseus_host::process::SpawnFailure> {
        match &self.primary {
            WorkerPrimary::Spawn(f) => Some(f),
            _ => self.retained_spawn.as_deref(),
        }
    }
    pub fn classification(&self) -> &'static str {
        match &self.primary {
            WorkerPrimary::Io(_) => "io",
            WorkerPrimary::Store(_) => "store",
            WorkerPrimary::Spawn(_) => "spawn",
            WorkerPrimary::Panic(_) => "panic",
        }
    }
    pub fn cleanup_error_count(&self) -> usize {
        self.cleanup_errors.iter().filter(|e| e.is_some()).count()
    }
    pub fn panic_payload(&self) -> Option<&(dyn std::any::Any + Send + 'static)> {
        match &self.primary {
            WorkerPrimary::Panic(p) => Some(p.as_ref()),
            _ => None,
        }
    }
}
impl std::fmt::Display for OwnedWorkerFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("helper worker failure")
    }
}
impl std::fmt::Debug for OwnedWorkerFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for OwnedWorkerFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.primary {
            WorkerPrimary::Io(e) => Some(e),
            WorkerPrimary::Store(e) => Some(e),
            WorkerPrimary::Spawn(e) => Some(e),
            WorkerPrimary::Panic(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admission_grant_is_exact_and_one_shot() {
        let key = RunKey {
            lease: v2::LeaseId::from_bytes([1; 16]),
            operation: v2::OperationId::from_bytes([2; 16]),
            generation: 7,
        };
        let (mut grant, watch) = start_pair(key, 9);
        assert!(matches!(watch.poll(), StartState::Waiting));
        assert!(
            grant
                .grant(AcceptedAdmission {
                    key,
                    reply_to: 8,
                    outbound_sequence: 1
                })
                .is_err()
        );
        grant
            .grant(AcceptedAdmission {
                key,
                reply_to: 9,
                outbound_sequence: 1,
            })
            .unwrap();
        assert!(matches!(watch.poll(), StartState::Granted));
        assert!(
            grant
                .grant(AcceptedAdmission {
                    key,
                    reply_to: 9,
                    outbound_sequence: 2
                })
                .is_err()
        );
        grant.abort();
        assert!(matches!(watch.poll(), StartState::Granted));
    }
}
#[cfg(test)]
mod failure_tests {
    use super::*;
    #[derive(Debug)]
    struct Hostile;
    impl std::fmt::Display for Hostile {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            panic!("source formatter must never run")
        }
    }
    impl std::error::Error for Hostile {}
    #[test]
    fn bounded_failure_formatting_never_formats_retained_sources() {
        let failure = OwnedWorkerFailure {
            primary: WorkerPrimary::Io(io::Error::other(Hostile)),
            retained_spawn: None,
            cleanup_errors: std::array::from_fn(|_| None),
        };
        assert_eq!(
            format!("{failure:?} {failure}"),
            "helper worker failure helper worker failure"
        );
        assert_eq!(failure.classification(), "io");
        assert!(std::error::Error::source(&failure).is_some());
        let error = RuntimeError::Transport(io::Error::other(Hostile));
        assert_eq!(
            format!("{error:?} {error}"),
            "helper runtime failure helper runtime failure"
        );
    }
    #[test]
    fn panic_payload_retains_exact_original_value() {
        let payload = Box::new([31u8; 32]);
        let original = payload.as_ptr();
        let failure = OwnedWorkerFailure {
            primary: WorkerPrimary::Panic(payload),
            retained_spawn: None,
            cleanup_errors: std::array::from_fn(|_| None),
        };
        assert_eq!(
            failure
                .panic_payload()
                .unwrap()
                .downcast_ref::<[u8; 32]>()
                .unwrap()
                .as_ptr(),
            original
        );
        assert_eq!(failure.classification(), "panic");
    }
}
