//! Private borrowed wire projections and closed typed decoding.
use super::*;
use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::{Map, Value};
use std::{fmt, io::Write};
trait Wire: Sized {
    fn read(value: Value) -> Result<Self, Error>;
    fn write<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error>;
}
struct Ref<'a, T>(&'a T);
impl<T: Wire> Serialize for Ref<'_, T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.write(s)
    }
}
struct Object(Map<String, Value>);
impl Object {
    fn new(value: Value) -> Result<Self, Error> {
        match value {
            Value::Object(v) => Ok(Self(v)),
            _ => Err(Error::InvalidEnvelope),
        }
    }
    fn take<T: Wire>(&mut self, key: &str) -> Result<T, Error> {
        T::read(self.0.remove(key).ok_or(Error::InvalidEnvelope)?)
    }
    fn literal(&mut self, key: &str, expected: &str) -> Result<(), Error> {
        if self.take::<String>(key)? == expected {
            Ok(())
        } else {
            Err(Error::InvalidValue)
        }
    }
    fn end(self) -> Result<(), Error> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(Error::InvalidEnvelope)
        }
    }
}
impl Wire for String {
    fn read(v: Value) -> Result<Self, Error> {
        match v {
            Value::String(v) => Ok(v),
            _ => Err(Error::InvalidEnvelope),
        }
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self)
    }
}
macro_rules! scalar {
    ($t:ty,$method:ident,$ser:ident) => {
        impl Wire for $t {
            fn read(v: Value) -> Result<Self, Error> {
                v.$method()
                    .and_then(|v| <$t>::try_from(v).ok())
                    .ok_or(Error::InvalidValue)
            }
            fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.$ser(*self)
            }
        }
    };
}
scalar!(u64, as_u64, serialize_u64);
scalar!(u32, as_u64, serialize_u32);
scalar!(u16, as_u64, serialize_u16);
scalar!(u8, as_u64, serialize_u8);
impl Wire for bool {
    fn read(v: Value) -> Result<Self, Error> {
        v.as_bool().ok_or(Error::InvalidEnvelope)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bool(*self)
    }
}
impl Wire for SessionId {
    fn read(v: Value) -> Result<Self, Error> {
        Self::parse(&String::read(v)?).map_err(|_| Error::InvalidValue)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}
impl Wire for BindingId {
    fn read(v: Value) -> Result<Self, Error> {
        Self::parse(&String::read(v)?).map_err(|_| Error::InvalidValue)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}
impl Wire for WorkspaceMarker {
    fn read(v: Value) -> Result<Self, Error> {
        Self::parse(&String::read(v)?).map_err(|_| Error::InvalidValue)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}
impl Wire for IntentId {
    fn read(v: Value) -> Result<Self, Error> {
        Self::parse(&String::read(v)?).map_err(|_| Error::InvalidValue)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}
impl Wire for LeaseId {
    fn read(v: Value) -> Result<Self, Error> {
        Self::parse(&String::read(v)?).map_err(|_| Error::InvalidValue)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}
impl Wire for OperationId {
    fn read(v: Value) -> Result<Self, Error> {
        Self::parse(&String::read(v)?).map_err(|_| Error::InvalidValue)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}
impl Wire for Digest {
    fn read(v: Value) -> Result<Self, Error> {
        Self::parse(&String::read(v)?).map_err(|_| Error::InvalidValue)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}
impl Wire for ProgramId {
    fn read(v: Value) -> Result<Self, Error> {
        Self::new(&String::read(v)?)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}
impl Wire for LeafName {
    fn read(v: Value) -> Result<Self, Error> {
        Self::new(&String::read(v)?)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}
impl Wire for SidText {
    fn read(v: Value) -> Result<Self, Error> {
        Self::new(&String::read(v)?)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}
struct Hex<'a>(&'a [u8]);
impl fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}
impl Wire for FileBytes {
    fn read(v: Value) -> Result<Self, Error> {
        let text = String::read(v)?;
        if text.len() > 8192 {
            return Err(Error::LimitExceeded);
        }
        if !text.len().is_multiple_of(2) {
            return Err(Error::InvalidValue);
        }
        let nibble = |b| match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err(Error::InvalidValue),
        };
        let mut bytes = Vec::with_capacity(text.len() / 2);
        for pair in text.as_bytes().as_chunks::<2>().0 {
            bytes.push((nibble(pair[0])? << 4) | nibble(pair[1])?);
        }
        Self::new(&bytes)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&Hex(self.as_slice()))
    }
}
impl Wire for ProgramList {
    fn read(v: Value) -> Result<Self, Error> {
        let Value::Array(values) = v else {
            return Err(Error::InvalidEnvelope);
        };
        if values.is_empty() || values.len() > MAX_PROGRAMS {
            return Err(Error::LimitExceeded);
        }
        let mut result = Vec::with_capacity(values.len());
        for value in values {
            result.push(ProgramId::read(value)?)
        }
        Self::new(result)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = s.serialize_seq(Some(self.as_slice().len()))?;
        for program in self.as_slice() {
            seq.serialize_element(&Ref(program))?;
        }
        seq.end()
    }
}
impl Wire for Role {
    fn read(value: Value) -> Result<Self, Error> {
        let value = String::read(value)?;
        match value.as_str() {
            "controller" => Ok(Self::Controller),
            "helper" => Ok(Self::Helper),
            _ => Err(Error::InvalidValue),
        }
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Controller => s.serialize_str("controller"),
            Self::Helper => s.serialize_str("helper"),
        }
    }
}
impl Wire for Os {
    fn read(value: Value) -> Result<Self, Error> {
        let value = String::read(value)?;
        match value.as_str() {
            "linux" => Ok(Self::Linux),
            "macos" => Ok(Self::Macos),
            "windows" => Ok(Self::Windows),
            _ => Err(Error::InvalidValue),
        }
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Linux => s.serialize_str("linux"),
            Self::Macos => s.serialize_str("macos"),
            Self::Windows => s.serialize_str("windows"),
        }
    }
}
impl Wire for Channel {
    fn read(value: Value) -> Result<Self, Error> {
        let value = String::read(value)?;
        match value.as_str() {
            "stdout" => Ok(Self::Stdout),
            "stderr" => Ok(Self::Stderr),
            _ => Err(Error::InvalidValue),
        }
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Stdout => s.serialize_str("stdout"),
            Self::Stderr => s.serialize_str("stderr"),
        }
    }
}
impl Wire for OperationKind {
    fn read(value: Value) -> Result<Self, Error> {
        let value = String::read(value)?;
        match value.as_str() {
            "create_file" => Ok(Self::CreateFile),
            "run" => Ok(Self::Run),
            _ => Err(Error::InvalidValue),
        }
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::CreateFile => s.serialize_str("create_file"),
            Self::Run => s.serialize_str("run"),
        }
    }
}
impl Wire for RunReason {
    fn read(value: Value) -> Result<Self, Error> {
        let value = String::read(value)?;
        match value.as_str() {
            "natural" => Ok(Self::Natural),
            "cancel_requested" => Ok(Self::CancelRequested),
            "deadline" => Ok(Self::Deadline),
            _ => Err(Error::InvalidValue),
        }
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Natural => s.serialize_str("natural"),
            Self::CancelRequested => s.serialize_str("cancel_requested"),
            Self::Deadline => s.serialize_str("deadline"),
        }
    }
}
impl Wire for NotStartedReason {
    fn read(value: Value) -> Result<Self, Error> {
        let value = String::read(value)?;
        match value.as_str() {
            "cancel_requested" => Ok(Self::CancelRequested),
            "deadline" => Ok(Self::Deadline),
            _ => Err(Error::InvalidValue),
        }
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::CancelRequested => s.serialize_str("cancel_requested"),
            Self::Deadline => s.serialize_str("deadline"),
        }
    }
}
impl Wire for FileCreateFailureCode {
    fn read(value: Value) -> Result<Self, Error> {
        let value = String::read(value)?;
        match value.as_str() {
            "already_exists" => Ok(Self::AlreadyExists),
            "permission" => Ok(Self::Permission),
            _ => Err(Error::InvalidValue),
        }
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::AlreadyExists => s.serialize_str("already_exists"),
            Self::Permission => s.serialize_str("permission"),
        }
    }
}
impl Wire for LaunchPhase {
    fn read(value: Value) -> Result<Self, Error> {
        let value = String::read(value)?;
        match value.as_str() {
            "never_created" => Ok(Self::NeverCreated),
            "created_suspended" => Ok(Self::CreatedSuspended),
            "may_run" => Ok(Self::MayRun),
            _ => Err(Error::InvalidValue),
        }
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::NeverCreated => s.serialize_str("never_created"),
            Self::CreatedSuspended => s.serialize_str("created_suspended"),
            Self::MayRun => s.serialize_str("may_run"),
        }
    }
}
impl Wire for Cleanup {
    fn read(value: Value) -> Result<Self, Error> {
        let value = String::read(value)?;
        match value.as_str() {
            "posix_leader_reaped_group_termination_requested" => {
                Ok(Self::PosixLeaderReapedGroupTerminationRequested)
            }
            "windows_process_exited_job_empty" => Ok(Self::WindowsProcessExitedJobEmpty),
            _ => Err(Error::InvalidValue),
        }
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::PosixLeaderReapedGroupTerminationRequested => {
                s.serialize_str("posix_leader_reaped_group_termination_requested")
            }
            Self::WindowsProcessExitedJobEmpty => {
                s.serialize_str("windows_process_exited_job_empty")
            }
        }
    }
}
impl Wire for SpawnCleanup {
    fn read(value: Value) -> Result<Self, Error> {
        let value = String::read(value)?;
        match value.as_str() {
            "no_child_created" => Ok(Self::NoChildCreated),
            "posix_leader_reaped_group_termination_requested" => {
                Ok(Self::PosixLeaderReapedGroupTerminationRequested)
            }
            "windows_process_exited_job_empty" => Ok(Self::WindowsProcessExitedJobEmpty),
            _ => Err(Error::InvalidValue),
        }
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::NoChildCreated => s.serialize_str("no_child_created"),
            Self::PosixLeaderReapedGroupTerminationRequested => {
                s.serialize_str("posix_leader_reaped_group_termination_requested")
            }
            Self::WindowsProcessExitedJobEmpty => {
                s.serialize_str("windows_process_exited_job_empty")
            }
        }
    }
}
impl Wire for FailureCode {
    fn read(value: Value) -> Result<Self, Error> {
        let value = String::read(value)?;
        match value.as_str() {
            "io" => Ok(Self::Io),
            "permission" => Ok(Self::Permission),
            "unsupported" => Ok(Self::Unsupported),
            "invalid_catalog" => Ok(Self::InvalidCatalog),
            "native_failure" => Ok(Self::NativeFailure),
            _ => Err(Error::InvalidValue),
        }
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Io => s.serialize_str("io"),
            Self::Permission => s.serialize_str("permission"),
            Self::Unsupported => s.serialize_str("unsupported"),
            Self::InvalidCatalog => s.serialize_str("invalid_catalog"),
            Self::NativeFailure => s.serialize_str("native_failure"),
        }
    }
}
impl Wire for UnknownReason {
    fn read(value: Value) -> Result<Self, Error> {
        let value = String::read(value)?;
        match value.as_str() {
            "helper_restarted_with_unfinished_operation" => {
                Ok(Self::HelperRestartedWithUnfinishedOperation)
            }
            "ledger_write_uncertain" => Ok(Self::LedgerWriteUncertain),
            "process_state_unproved" => Ok(Self::ProcessStateUnproved),
            "file_effect_uncertain" => Ok(Self::FileEffectUncertain),
            _ => Err(Error::InvalidValue),
        }
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::HelperRestartedWithUnfinishedOperation => {
                s.serialize_str("helper_restarted_with_unfinished_operation")
            }
            Self::LedgerWriteUncertain => s.serialize_str("ledger_write_uncertain"),
            Self::ProcessStateUnproved => s.serialize_str("process_state_unproved"),
            Self::FileEffectUncertain => s.serialize_str("file_effect_uncertain"),
        }
    }
}
impl Wire for RefusalCode {
    fn read(value: Value) -> Result<Self, Error> {
        let value = String::read(value)?;
        match value.as_str() {
            "wrong_binding" => Ok(Self::WrongBinding),
            "wrong_manifest" => Ok(Self::WrongManifest),
            "wrong_lease" => Ok(Self::WrongLease),
            "unsupported" => Ok(Self::Unsupported),
            "busy" => Ok(Self::Busy),
            "replay_conflict" => Ok(Self::ReplayConflict),
            "capacity" => Ok(Self::Capacity),
            "ledger_unavailable" => Ok(Self::LedgerUnavailable),
            "ledger_unknown" => Ok(Self::LedgerUnknown),
            "lease_quarantined" => Ok(Self::LeaseQuarantined),
            "awaiting_external_reset" => Ok(Self::AwaitingExternalReset),
            "ownership_pending" => Ok(Self::OwnershipPending),
            "invalid_request" => Ok(Self::InvalidRequest),
            "not_found" => Ok(Self::NotFound),
            _ => Err(Error::InvalidValue),
        }
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::WrongBinding => s.serialize_str("wrong_binding"),
            Self::WrongManifest => s.serialize_str("wrong_manifest"),
            Self::WrongLease => s.serialize_str("wrong_lease"),
            Self::Unsupported => s.serialize_str("unsupported"),
            Self::Busy => s.serialize_str("busy"),
            Self::ReplayConflict => s.serialize_str("replay_conflict"),
            Self::Capacity => s.serialize_str("capacity"),
            Self::LedgerUnavailable => s.serialize_str("ledger_unavailable"),
            Self::LedgerUnknown => s.serialize_str("ledger_unknown"),
            Self::LeaseQuarantined => s.serialize_str("lease_quarantined"),
            Self::AwaitingExternalReset => s.serialize_str("awaiting_external_reset"),
            Self::OwnershipPending => s.serialize_str("ownership_pending"),
            Self::InvalidRequest => s.serialize_str("invalid_request"),
            Self::NotFound => s.serialize_str("not_found"),
        }
    }
}
impl Wire for LogCode {
    fn read(value: Value) -> Result<Self, Error> {
        let value = String::read(value)?;
        match value.as_str() {
            "output_truncated" => Ok(Self::OutputTruncated),
            "cancel_requested" => Ok(Self::CancelRequested),
            "deadline_expired" => Ok(Self::DeadlineExpired),
            "cleanup_pending" => Ok(Self::CleanupPending),
            _ => Err(Error::InvalidValue),
        }
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::OutputTruncated => s.serialize_str("output_truncated"),
            Self::CancelRequested => s.serialize_str("cancel_requested"),
            Self::DeadlineExpired => s.serialize_str("deadline_expired"),
            Self::CleanupPending => s.serialize_str("cleanup_pending"),
        }
    }
}
impl Wire for CloseCode {
    fn read(value: Value) -> Result<Self, Error> {
        let value = String::read(value)?;
        match value.as_str() {
            "normal" => Ok(Self::Normal),
            "protocol_error" => Ok(Self::ProtocolError),
            "sequence_exhausted" => Ok(Self::SequenceExhausted),
            "peer_lost" => Ok(Self::PeerLost),
            "shutdown_deadline" => Ok(Self::ShutdownDeadline),
            "ledger_unavailable" => Ok(Self::LedgerUnavailable),
            _ => Err(Error::InvalidValue),
        }
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Normal => s.serialize_str("normal"),
            Self::ProtocolError => s.serialize_str("protocol_error"),
            Self::SequenceExhausted => s.serialize_str("sequence_exhausted"),
            Self::PeerLost => s.serialize_str("peer_lost"),
            Self::ShutdownDeadline => s.serialize_str("shutdown_deadline"),
            Self::LedgerUnavailable => s.serialize_str("ledger_unavailable"),
        }
    }
}
impl Wire for Principal {
    fn read(value: Value) -> Result<Self, Error> {
        let mut object = Object::new(value)?;
        let kind = object.take::<String>("kind")?;
        let result = match kind.as_str() {
            "posix" => Self::Posix {
                effective_uid: object.take::<u32>("effective_uid")?,
                effective_gid: object.take::<u32>("effective_gid")?,
            },
            "windows" => Self::Windows {
                token_user_sid: object.take::<SidText>("token_user_sid")?,
            },
            _ => return Err(Error::InvalidEnvelope),
        };
        object.end()?;
        Ok(result)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Posix {
                effective_uid,
                effective_gid,
            } => {
                let mut map = s.serialize_map(Some(3))?;
                map.serialize_entry("kind", "posix")?;
                map.serialize_entry("effective_uid", &Ref(effective_uid))?;
                map.serialize_entry("effective_gid", &Ref(effective_gid))?;
                map.end()
            }
            Self::Windows { token_user_sid } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "windows")?;
                map.serialize_entry("token_user_sid", &Ref(token_user_sid))?;
                map.end()
            }
        }
    }
}
impl Wire for NativeStatus {
    fn read(value: Value) -> Result<Self, Error> {
        let mut object = Object::new(value)?;
        let kind = object.take::<String>("kind")?;
        let result = match kind.as_str() {
            "posix_exit" => Self::PosixExit {
                code: object.take::<u8>("code")?,
            },
            "posix_signal" => Self::PosixSignal {
                signal: object.take::<u16>("signal")?,
            },
            "windows_exit" => Self::WindowsExit {
                code: object.take::<u32>("code")?,
            },
            _ => return Err(Error::InvalidEnvelope),
        };
        object.end()?;
        Ok(result)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::PosixExit { code } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "posix_exit")?;
                map.serialize_entry("code", &Ref(code))?;
                map.end()
            }
            Self::PosixSignal { signal } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "posix_signal")?;
                map.serialize_entry("signal", &Ref(signal))?;
                map.end()
            }
            Self::WindowsExit { code } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "windows_exit")?;
                map.serialize_entry("code", &Ref(code))?;
                map.end()
            }
        }
    }
}
impl Wire for Message {
    fn read(value: Value) -> Result<Self, Error> {
        let mut object = Object::new(value)?;
        let kind = object.take::<String>("kind")?;
        let result = match kind.as_str() {
            "hello" => {
                if object.take::<u16>("protocol_min")? != 2
                    || object.take::<u16>("protocol_max")? != 2
                {
                    return Err(Error::UnsupportedVersion);
                };
                Self::Hello {
                    role: object.take::<Role>("role")?,
                }
            }
            "request" => Self::Request {
                request: object.take::<Request>("request")?,
            },
            "response" => Self::Response {
                reply_to: object.take::<u64>("reply_to")?,
                result: object.take::<Reply>("result")?,
            },
            "output" => Self::Output {
                lease: object.take::<LeaseId>("lease")?,
                operation: object.take::<OperationId>("operation")?,
                channel: object.take::<Channel>("channel")?,
                offset: object.take::<u64>("offset")?,
                bytes: object.take::<FileBytes>("bytes")?,
            },
            "log" => Self::Log {
                lease: object.take::<LeaseId>("lease")?,
                operation: object.take::<OperationId>("operation")?,
                code: object.take::<LogCode>("code")?,
            },
            "closed" => Self::Closed {
                code: object.take::<CloseCode>("code")?,
            },
            _ => return Err(Error::InvalidEnvelope),
        };
        object.end()?;
        Ok(result)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Hello { role } => {
                let mut map = s.serialize_map(Some(4))?;
                map.serialize_entry("kind", "hello")?;
                map.serialize_entry("role", &Ref(role))?;
                map.serialize_entry("protocol_min", &2u16)?;
                map.serialize_entry("protocol_max", &2u16)?;
                map.end()
            }
            Self::Request { request } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "request")?;
                map.serialize_entry("request", &RequestRef(request))?;
                map.end()
            }
            Self::Response { reply_to, result } => {
                let mut map = s.serialize_map(Some(3))?;
                map.serialize_entry("kind", "response")?;
                map.serialize_entry("reply_to", &Ref(reply_to))?;
                map.serialize_entry("result", &Ref(result))?;
                map.end()
            }
            Self::Output {
                lease,
                operation,
                channel,
                offset,
                bytes,
            } => {
                let mut map = s.serialize_map(Some(6))?;
                map.serialize_entry("kind", "output")?;
                map.serialize_entry("lease", &Ref(lease))?;
                map.serialize_entry("operation", &Ref(operation))?;
                map.serialize_entry("channel", &Ref(channel))?;
                map.serialize_entry("offset", &Ref(offset))?;
                map.serialize_entry("bytes", &Ref(bytes))?;
                map.end()
            }
            Self::Log {
                lease,
                operation,
                code,
            } => {
                let mut map = s.serialize_map(Some(4))?;
                map.serialize_entry("kind", "log")?;
                map.serialize_entry("lease", &Ref(lease))?;
                map.serialize_entry("operation", &Ref(operation))?;
                map.serialize_entry("code", &Ref(code))?;
                map.end()
            }
            Self::Closed { code } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "closed")?;
                map.serialize_entry("code", &Ref(code))?;
                map.end()
            }
        }
    }
}
impl Wire for Request {
    fn read(value: Value) -> Result<Self, Error> {
        let mut object = Object::new(value)?;
        let kind = object.take::<String>("kind")?;
        let result = match kind.as_str() {
            "describe" => Self::Describe,
            "acquire" => Self::Acquire {
                intent: object.take::<IntentId>("intent")?,
                binding: object.take::<BindingId>("binding")?,
                manifest_digest: object.take::<Digest>("manifest_digest")?,
            },
            "query_acquire" => Self::QueryAcquire {
                intent: object.take::<IntentId>("intent")?,
                binding: object.take::<BindingId>("binding")?,
                manifest_digest: object.take::<Digest>("manifest_digest")?,
            },
            "create_file" => Self::CreateFile {
                lease: object.take::<LeaseId>("lease")?,
                operation: object.take::<OperationId>("operation")?,
                path: object.take::<LeafName>("path")?,
                bytes: object.take::<FileBytes>("bytes")?,
            },
            "read_file" => Self::ReadFile {
                lease: object.take::<LeaseId>("lease")?,
                path: object.take::<LeafName>("path")?,
                offset: object.take::<u64>("offset")?,
                length: object.take::<u16>("length")?,
            },
            "inspect_file" => Self::InspectFile {
                lease: object.take::<LeaseId>("lease")?,
                path: object.take::<LeafName>("path")?,
            },
            "run" => Self::Run {
                lease: object.take::<LeaseId>("lease")?,
                operation: object.take::<OperationId>("operation")?,
                program: object.take::<ProgramId>("program")?,
            },
            "query_operation" => Self::QueryOperation {
                lease: object.take::<LeaseId>("lease")?,
                operation: object.take::<OperationId>("operation")?,
            },
            "cancel" => Self::Cancel {
                lease: object.take::<LeaseId>("lease")?,
                operation: object.take::<OperationId>("operation")?,
            },
            "release" => Self::Release {
                lease: object.take::<LeaseId>("lease")?,
            },
            "query_lease" => Self::QueryLease {
                lease: object.take::<LeaseId>("lease")?,
            },
            _ => return Err(Error::InvalidEnvelope),
        };
        object.end()?;
        Ok(result)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Describe => {
                let mut map = s.serialize_map(Some(1))?;
                map.serialize_entry("kind", "describe")?;
                map.end()
            }
            Self::Acquire {
                intent,
                binding,
                manifest_digest,
            } => {
                let mut map = s.serialize_map(Some(4))?;
                map.serialize_entry("kind", "acquire")?;
                map.serialize_entry("intent", &Ref(intent))?;
                map.serialize_entry("binding", &Ref(binding))?;
                map.serialize_entry("manifest_digest", &Ref(manifest_digest))?;
                map.end()
            }
            Self::QueryAcquire {
                intent,
                binding,
                manifest_digest,
            } => {
                let mut map = s.serialize_map(Some(4))?;
                map.serialize_entry("kind", "query_acquire")?;
                map.serialize_entry("intent", &Ref(intent))?;
                map.serialize_entry("binding", &Ref(binding))?;
                map.serialize_entry("manifest_digest", &Ref(manifest_digest))?;
                map.end()
            }
            Self::CreateFile {
                lease,
                operation,
                path,
                bytes,
            } => {
                let mut map = s.serialize_map(Some(5))?;
                map.serialize_entry("kind", "create_file")?;
                map.serialize_entry("lease", &Ref(lease))?;
                map.serialize_entry("operation", &Ref(operation))?;
                map.serialize_entry("path", &Ref(path))?;
                map.serialize_entry("bytes", &Ref(bytes))?;
                map.end()
            }
            Self::ReadFile {
                lease,
                path,
                offset,
                length,
            } => {
                let mut map = s.serialize_map(Some(5))?;
                map.serialize_entry("kind", "read_file")?;
                map.serialize_entry("lease", &Ref(lease))?;
                map.serialize_entry("path", &Ref(path))?;
                map.serialize_entry("offset", &Ref(offset))?;
                map.serialize_entry("length", &Ref(length))?;
                map.end()
            }
            Self::InspectFile { lease, path } => {
                let mut map = s.serialize_map(Some(3))?;
                map.serialize_entry("kind", "inspect_file")?;
                map.serialize_entry("lease", &Ref(lease))?;
                map.serialize_entry("path", &Ref(path))?;
                map.end()
            }
            Self::Run {
                lease,
                operation,
                program,
            } => {
                let mut map = s.serialize_map(Some(4))?;
                map.serialize_entry("kind", "run")?;
                map.serialize_entry("lease", &Ref(lease))?;
                map.serialize_entry("operation", &Ref(operation))?;
                map.serialize_entry("program", &Ref(program))?;
                map.end()
            }
            Self::QueryOperation { lease, operation } => {
                let mut map = s.serialize_map(Some(3))?;
                map.serialize_entry("kind", "query_operation")?;
                map.serialize_entry("lease", &Ref(lease))?;
                map.serialize_entry("operation", &Ref(operation))?;
                map.end()
            }
            Self::Cancel { lease, operation } => {
                let mut map = s.serialize_map(Some(3))?;
                map.serialize_entry("kind", "cancel")?;
                map.serialize_entry("lease", &Ref(lease))?;
                map.serialize_entry("operation", &Ref(operation))?;
                map.end()
            }
            Self::Release { lease } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "release")?;
                map.serialize_entry("lease", &Ref(lease))?;
                map.end()
            }
            Self::QueryLease { lease } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "query_lease")?;
                map.serialize_entry("lease", &Ref(lease))?;
                map.end()
            }
        }
    }
}
impl Wire for Reply {
    fn read(value: Value) -> Result<Self, Error> {
        let mut object = Object::new(value)?;
        let kind = object.take::<String>("kind")?;
        let result = match kind.as_str() {
            "description" => {
                object.literal("profile", "trusted_fixture_v1")?;
                object.literal("authority", "explicit_fixture_only")?;
                Self::Description {
                    binding: object.take::<BindingId>("binding")?,
                    manifest_digest: object.take::<Digest>("manifest_digest")?,
                    workspace_marker: object.take::<WorkspaceMarker>("workspace_marker")?,
                    os: object.take::<Os>("os")?,
                    helper_principal: object.take::<Principal>("helper_principal")?,
                    programs: object.take::<ProgramList>("programs")?,
                }
            }
            "acquired" => Self::Acquired {
                intent: object.take::<IntentId>("intent")?,
                binding: object.take::<BindingId>("binding")?,
                manifest_digest: object.take::<Digest>("manifest_digest")?,
                state: object.take::<AcquireView>("state")?,
            },
            "lease" => Self::Lease {
                lease: object.take::<LeaseId>("lease")?,
                state: object.take::<LeaseView>("state")?,
            },
            "operation" => Self::Operation {
                lease: object.take::<LeaseId>("lease")?,
                operation: object.take::<OperationId>("operation")?,
                state: object.take::<OperationView>("state")?,
            },
            "file_bytes" => Self::FileBytes {
                lease: object.take::<LeaseId>("lease")?,
                path: object.take::<LeafName>("path")?,
                offset: object.take::<u64>("offset")?,
                bytes: object.take::<FileBytes>("bytes")?,
                eof: object.take::<bool>("eof")?,
            },
            "file_info" => Self::FileInfo {
                lease: object.take::<LeaseId>("lease")?,
                path: object.take::<LeafName>("path")?,
                length: object.take::<u16>("length")?,
                digest: object.take::<Digest>("digest")?,
            },
            "refused" => Self::Refused {
                code: object.take::<RefusalCode>("code")?,
            },
            _ => return Err(Error::InvalidEnvelope),
        };
        object.end()?;
        Ok(result)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Description {
                binding,
                manifest_digest,
                workspace_marker,
                os,
                helper_principal,
                programs,
            } => {
                let mut map = s.serialize_map(Some(9))?;
                map.serialize_entry("kind", "description")?;
                map.serialize_entry("profile", "trusted_fixture_v1")?;
                map.serialize_entry("authority", "explicit_fixture_only")?;
                map.serialize_entry("binding", &Ref(binding))?;
                map.serialize_entry("manifest_digest", &Ref(manifest_digest))?;
                map.serialize_entry("workspace_marker", &Ref(workspace_marker))?;
                map.serialize_entry("os", &Ref(os))?;
                map.serialize_entry("helper_principal", &Ref(helper_principal))?;
                map.serialize_entry("programs", &Ref(programs))?;
                map.end()
            }
            Self::Acquired {
                intent,
                binding,
                manifest_digest,
                state,
            } => {
                let mut map = s.serialize_map(Some(5))?;
                map.serialize_entry("kind", "acquired")?;
                map.serialize_entry("intent", &Ref(intent))?;
                map.serialize_entry("binding", &Ref(binding))?;
                map.serialize_entry("manifest_digest", &Ref(manifest_digest))?;
                map.serialize_entry("state", &Ref(state))?;
                map.end()
            }
            Self::Lease { lease, state } => {
                let mut map = s.serialize_map(Some(3))?;
                map.serialize_entry("kind", "lease")?;
                map.serialize_entry("lease", &Ref(lease))?;
                map.serialize_entry("state", &Ref(state))?;
                map.end()
            }
            Self::Operation {
                lease,
                operation,
                state,
            } => {
                let mut map = s.serialize_map(Some(4))?;
                map.serialize_entry("kind", "operation")?;
                map.serialize_entry("lease", &Ref(lease))?;
                map.serialize_entry("operation", &Ref(operation))?;
                map.serialize_entry("state", &Ref(state))?;
                map.end()
            }
            Self::FileBytes {
                lease,
                path,
                offset,
                bytes,
                eof,
            } => {
                let mut map = s.serialize_map(Some(6))?;
                map.serialize_entry("kind", "file_bytes")?;
                map.serialize_entry("lease", &Ref(lease))?;
                map.serialize_entry("path", &Ref(path))?;
                map.serialize_entry("offset", &Ref(offset))?;
                map.serialize_entry("bytes", &Ref(bytes))?;
                map.serialize_entry("eof", &Ref(eof))?;
                map.end()
            }
            Self::FileInfo {
                lease,
                path,
                length,
                digest,
            } => {
                let mut map = s.serialize_map(Some(5))?;
                map.serialize_entry("kind", "file_info")?;
                map.serialize_entry("lease", &Ref(lease))?;
                map.serialize_entry("path", &Ref(path))?;
                map.serialize_entry("length", &Ref(length))?;
                map.serialize_entry("digest", &Ref(digest))?;
                map.end()
            }
            Self::Refused { code } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "refused")?;
                map.serialize_entry("code", &Ref(code))?;
                map.end()
            }
        }
    }
}
impl Wire for AcquireView {
    fn read(value: Value) -> Result<Self, Error> {
        let mut object = Object::new(value)?;
        let kind = object.take::<String>("kind")?;
        let result = match kind.as_str() {
            "not_found" => Self::NotFound,
            "lease" => Self::Lease {
                lease: object.take::<LeaseId>("lease")?,
                state: object.take::<LeaseView>("state")?,
            },
            _ => return Err(Error::InvalidEnvelope),
        };
        object.end()?;
        Ok(result)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::NotFound => {
                let mut map = s.serialize_map(Some(1))?;
                map.serialize_entry("kind", "not_found")?;
                map.end()
            }
            Self::Lease { lease, state } => {
                let mut map = s.serialize_map(Some(3))?;
                map.serialize_entry("kind", "lease")?;
                map.serialize_entry("lease", &Ref(lease))?;
                map.serialize_entry("state", &Ref(state))?;
                map.end()
            }
        }
    }
}
impl Wire for LeaseView {
    fn read(value: Value) -> Result<Self, Error> {
        let mut object = Object::new(value)?;
        let kind = object.take::<String>("kind")?;
        let result = match kind.as_str() {
            "held" => Self::Held {
                intent: object.take::<IntentId>("intent")?,
            },
            "awaiting_external_reset" => Self::AwaitingExternalReset {
                intent: object.take::<IntentId>("intent")?,
            },
            "quarantined" => Self::Quarantined {
                intent: object.take::<IntentId>("intent")?,
                reason: object.take::<UnknownReason>("reason")?,
            },
            _ => return Err(Error::InvalidEnvelope),
        };
        object.end()?;
        Ok(result)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Held { intent } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "held")?;
                map.serialize_entry("intent", &Ref(intent))?;
                map.end()
            }
            Self::AwaitingExternalReset { intent } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "awaiting_external_reset")?;
                map.serialize_entry("intent", &Ref(intent))?;
                map.end()
            }
            Self::Quarantined { intent, reason } => {
                let mut map = s.serialize_map(Some(3))?;
                map.serialize_entry("kind", "quarantined")?;
                map.serialize_entry("intent", &Ref(intent))?;
                map.serialize_entry("reason", &Ref(reason))?;
                map.end()
            }
        }
    }
}
impl Wire for OperationView {
    fn read(value: Value) -> Result<Self, Error> {
        let mut object = Object::new(value)?;
        let kind = object.take::<String>("kind")?;
        let result = match kind.as_str() {
            "missing" => Self::Missing,
            "accepted" => Self::Accepted {
                operation_kind: object.take::<OperationKind>("operation_kind")?,
            },
            "running" => {
                object.literal("operation_kind", "run")?;
                Self::Running
            }
            "finished" => Self::Finished {
                outcome: decode_outcome(object.0.remove("outcome").ok_or(Error::InvalidEnvelope)?)?,
            },
            "unknown" => Self::Unknown {
                reason: object.take::<UnknownReason>("reason")?,
            },
            _ => return Err(Error::InvalidEnvelope),
        };
        object.end()?;
        Ok(result)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Missing => {
                let mut map = s.serialize_map(Some(1))?;
                map.serialize_entry("kind", "missing")?;
                map.end()
            }
            Self::Accepted { operation_kind } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "accepted")?;
                map.serialize_entry("operation_kind", &Ref(operation_kind))?;
                map.end()
            }
            Self::Running => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "running")?;
                map.serialize_entry("operation_kind", "run")?;
                map.end()
            }
            Self::Finished { outcome } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "finished")?;
                map.serialize_entry("outcome", &OutcomeRef(outcome))?;
                map.end()
            }
            Self::Unknown { reason } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "unknown")?;
                map.serialize_entry("reason", &Ref(reason))?;
                map.end()
            }
        }
    }
}
impl Wire for Outcome {
    fn read(value: Value) -> Result<Self, Error> {
        let mut object = Object::new(value)?;
        let kind = object.take::<String>("kind")?;
        let result = match kind.as_str() {
            "file_created" => Self::FileCreated {
                length: object.take::<u16>("length")?,
                digest: object.take::<Digest>("digest")?,
            },
            "not_started" => Self::NotStarted {
                reason: object.take::<NotStartedReason>("reason")?,
            },
            "file_create_failed" => Self::FileCreateFailed {
                code: object.take::<FileCreateFailureCode>("code")?,
            },
            "run_completed" => Self::RunCompleted {
                reason: object.take::<RunReason>("reason")?,
                status: object.take::<NativeStatus>("status")?,
                cleanup: object.take::<Cleanup>("cleanup")?,
                stdout_bytes: object.take::<u64>("stdout_bytes")?,
                stderr_bytes: object.take::<u64>("stderr_bytes")?,
                truncated: object.take::<bool>("truncated")?,
            },
            "launch_failed" => Self::LaunchFailed {
                phase: object.take::<LaunchPhase>("phase")?,
                code: object.take::<FailureCode>("code")?,
                cleanup: object.take::<SpawnCleanup>("cleanup")?,
            },
            _ => return Err(Error::InvalidEnvelope),
        };
        object.end()?;
        Ok(result)
    }
    fn write<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::FileCreated { length, digest } => {
                let mut map = s.serialize_map(Some(3))?;
                map.serialize_entry("kind", "file_created")?;
                map.serialize_entry("length", &Ref(length))?;
                map.serialize_entry("digest", &Ref(digest))?;
                map.end()
            }
            Self::NotStarted { reason } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "not_started")?;
                map.serialize_entry("reason", &Ref(reason))?;
                map.end()
            }
            Self::FileCreateFailed { code } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "file_create_failed")?;
                map.serialize_entry("code", &Ref(code))?;
                map.end()
            }
            Self::RunCompleted {
                reason,
                status,
                cleanup,
                stdout_bytes,
                stderr_bytes,
                truncated,
            } => {
                let mut map = s.serialize_map(Some(7))?;
                map.serialize_entry("kind", "run_completed")?;
                map.serialize_entry("reason", &Ref(reason))?;
                map.serialize_entry("status", &Ref(status))?;
                map.serialize_entry("cleanup", &Ref(cleanup))?;
                map.serialize_entry("stdout_bytes", &Ref(stdout_bytes))?;
                map.serialize_entry("stderr_bytes", &Ref(stderr_bytes))?;
                map.serialize_entry("truncated", &Ref(truncated))?;
                map.end()
            }
            Self::LaunchFailed {
                phase,
                code,
                cleanup,
            } => {
                let mut map = s.serialize_map(Some(4))?;
                map.serialize_entry("kind", "launch_failed")?;
                map.serialize_entry("phase", &Ref(phase))?;
                map.serialize_entry("code", &Ref(code))?;
                map.serialize_entry("cleanup", &Ref(cleanup))?;
                map.end()
            }
        }
    }
}
pub(crate) struct RequestRef<'a>(pub(crate) &'a Request);
impl Serialize for RequestRef<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.write(s)
    }
}
pub(crate) struct OutcomeRef<'a>(pub(crate) &'a Outcome);
impl Serialize for OutcomeRef<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.write(s)
    }
}
pub(crate) fn decode_outcome(value: Value) -> Result<Outcome, Error> {
    let outcome = Outcome::read(value)?;
    super::types::validate_outcome(&outcome)?;
    Ok(outcome)
}
fn has_null(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(v) => v.iter().any(has_null),
        Value::Object(v) => v.values().any(has_null),
        _ => false,
    }
}
pub(super) fn decode(body: &[u8]) -> Result<Envelope, Error> {
    let text = std::str::from_utf8(body).map_err(|_| Error::InvalidUtf8)?;
    let value = crate::strict::parse(text).map_err(|_| Error::InvalidJson)?;
    let version = value
        .as_object()
        .and_then(|v| v.get("version"))
        .and_then(Value::as_u64)
        .ok_or(Error::InvalidEnvelope)?;
    if version != 2 {
        return Err(Error::UnsupportedVersion);
    }
    if has_null(&value) {
        return Err(Error::InvalidEnvelope);
    }
    let mut object = Object::new(value)?;
    object.take::<u64>("version")?;
    let session = object.take("session")?;
    let sequence = object.take("sequence")?;
    let message = object.take("message")?;
    object.end()?;
    Envelope::new(session, sequence, message)
}
struct EnvelopeRef<'a>(&'a Envelope);
impl Serialize for EnvelopeRef<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(4))?;
        map.serialize_entry("version", &2u16)?;
        map.serialize_entry("session", &Ref(self.0.session()))?;
        map.serialize_entry("sequence", &self.0.sequence())?;
        map.serialize_entry("message", &Ref(self.0.message()))?;
        map.end()
    }
}
pub(super) fn write_body(envelope: &Envelope, writer: impl Write) -> Result<(), Error> {
    serde_json::to_writer(writer, &EnvelopeRef(envelope)).map_err(|_| Error::InvalidEnvelope)
}
