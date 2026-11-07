use std::{any::Any, error::Error, fmt, io};
use theseus_host::process::SpawnFailure;
use theseus_native_protocol::v2;

/// Fixed classifications. Raw native diagnostics and helper stderr are never
/// incorporated into this value or its formatting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientError {
    InvalidCommand,
    NotReady,
    Busy,
    Exhausted,
    Preflight,
    PreflightTimeout,
    Protocol,
    Native,
    Spawn,
    StderrOverflow,
    RecoveryRequired,
    Capacity,
    NotSettled,
    Stopping,
    WorkerStart,
    WorkerPanic,
}
impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidCommand => "invalid explicit native command",
            Self::NotReady => "helper is not ready",
            Self::Busy => "request slots occupied",
            Self::Exhausted => "request namespace exhausted",
            Self::Preflight => "helper preflight mismatch",
            Self::PreflightTimeout => "helper preflight expired",
            Self::Protocol => "protocol rejected",
            Self::Native => "native transport failed",
            Self::Spawn => "helper spawn failed",
            Self::StderrOverflow => "helper stderr exceeded limit",
            Self::RecoveryRequired => "explicit recovery required",
            Self::Capacity => "recovery capacity reached",
            Self::NotSettled => "previous transport is unsettled",
            Self::Stopping => "transport is stopping",
            Self::WorkerStart => "settlement worker could not start",
            Self::WorkerPanic => "settlement worker panicked",
        })
    }
}
impl Error for ClientError {}

pub(crate) enum Source {
    Io(io::Error),
    Spawn(SpawnFailure),
    Protocol(v2::Error),
}
/// Singly owned original source and optional original worker panic payload.
/// Inspection is borrowed; its absence during handoff means temporarily unavailable.
pub struct Failure {
    pub(crate) source: Option<Source>,
    pub(crate) panic: Option<Box<dyn Any + Send + 'static>>,
    pub(crate) secondary: Option<Source>,
}
impl Failure {
    pub fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_ref().map(|s| match s {
            Source::Io(e) => e as &dyn Error,
            Source::Spawn(e) => e as &dyn Error,
            Source::Protocol(e) => e as &dyn Error,
        })
    }
    pub fn panic_payload(&self) -> Option<&(dyn Any + Send + 'static)> {
        self.panic.as_deref()
    }
    pub(crate) fn new(source: Source) -> Self {
        Self {
            source: Some(source),
            panic: None,
            secondary: None,
        }
    }
}
