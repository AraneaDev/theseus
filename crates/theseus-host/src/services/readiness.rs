//! Bounded TheseusFixtureReadyV1 codec and explicitly admitted native IPC.
//! Account proofs are distinct from the original accepted Start instance binding.
pub use super::endpoint::UnixPath;
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::LocalChallenge;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{LocalChallenge, PipePeerExpectation};

pub const MAX_FRAME_BYTES: usize = 4096;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadyProtocol {
    TheseusFixtureReadyV1,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactRole {
    Base,
    Target,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadinessExpectation {
    pub artifact: ArtifactRole,
    pub artifact_length: u64,
    pub artifact_sha256: [u8; 32],
    pub version_literal: String,
    pub data_sha256: [u8; 32],
    pub configuration_digest: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadinessChallenge {
    pub protocol: ReadyProtocol,
    pub lease: String,
    pub slot: String,
    pub resource_binding_digest: [u8; 32],
    pub start_or_activation_operation: String,
    pub action_digest: [u8; 32],
    pub nonce: [u8; 16],
    pub expectation_digest: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadinessResponse {
    pub protocol: ReadyProtocol,
    pub lease: String,
    pub slot: String,
    pub resource_binding_digest: [u8; 32],
    pub start_or_activation_operation: String,
    pub action_digest: [u8; 32],
    pub nonce: [u8; 16],
    pub artifact_length: u64,
    pub artifact_sha256: [u8; 32],
    pub version_literal: String,
    pub data_sha256: [u8; 32],
    pub configuration_digest: [u8; 32],
    pub instance: String,
    pub initialized: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerExpectation {
    pub uid: u32,
    pub gid: u32,
    /// Bound to an original held process or cooperative manager-instance receipt
    /// by the caller, never synthesized from a reported or reattached PID.
    pub cooperative_instance: String,
    pub instance_binding_digest: [u8; 32],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerCredentials {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadinessState {
    Ready,
    Failed,
    Incomplete,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadinessFailure {
    WrongPeer,
    WrongProtocol,
    WrongLease,
    WrongSlot,
    WrongResourceBinding,
    WrongAction,
    WrongNonce,
    WrongArtifact,
    WrongVersion,
    WrongData,
    WrongConfiguration,
    WrongInstance,
    NotInitialized,
    MalformedResponse,
    FrameTooLarge,
    PeerClosed,
    Deadline,
    Cancelled,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadinessObservation {
    pub state: ReadinessState,
    pub peer: Option<PeerCredentials>,
    /// Measured Windows token user; never a synthesized POSIX principal.
    pub peer_sid: Option<Sid>,
    pub response: Option<ReadinessResponse>,
    pub failure: Option<ReadinessFailure>,
    pub authenticated_instance: bool,
    pub instance_binding_digest: [u8; 32],
}
fn completed_probe(value: Option<ReadinessObservation>) -> io::Result<ReadinessObservation> {
    value.ok_or_else(|| invalid("blocking probe unexpectedly yielded"))
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Strict UTF-8 canonical netstrings, with a closed message tag and exact arity.
/// Returned bytes are the body; transport prefixes a four-byte big-endian length.
pub fn encode_challenge(challenge: &ReadinessChallenge) -> io::Result<Vec<u8>> {
    validate_challenge(challenge)?;
    encode_fields(&[
        "theseus_fixture_ready_v1".into(),
        "challenge".into(),
        challenge.lease.clone(),
        challenge.slot.clone(),
        hex(&challenge.resource_binding_digest),
        challenge.start_or_activation_operation.clone(),
        hex(&challenge.action_digest),
        hex(&challenge.nonce),
        hex(&challenge.expectation_digest),
    ])
}
pub fn decode_challenge(bytes: &[u8]) -> io::Result<ReadinessChallenge> {
    let f = decode_fields(bytes, 9)?;
    check_tag(&f, "challenge")?;
    let value = ReadinessChallenge {
        protocol: ReadyProtocol::TheseusFixtureReadyV1,
        lease: f[2].into(),
        slot: f[3].into(),
        resource_binding_digest: unhex(f[4])?,
        start_or_activation_operation: f[5].into(),
        action_digest: unhex(f[6])?,
        nonce: unhex(f[7])?,
        expectation_digest: unhex(f[8])?,
    };
    validate_challenge(&value)?;
    Ok(value)
}
pub fn encode_response(response: &ReadinessResponse) -> io::Result<Vec<u8>> {
    validate_response(response)?;
    encode_fields(&[
        "theseus_fixture_ready_v1".into(),
        "response".into(),
        response.lease.clone(),
        response.slot.clone(),
        hex(&response.resource_binding_digest),
        response.start_or_activation_operation.clone(),
        hex(&response.action_digest),
        hex(&response.nonce),
        response.artifact_length.to_string(),
        hex(&response.artifact_sha256),
        response.version_literal.clone(),
        hex(&response.data_sha256),
        hex(&response.configuration_digest),
        response.instance.clone(),
        if response.initialized {
            "true"
        } else {
            "false"
        }
        .into(),
    ])
}
pub fn decode_response(bytes: &[u8]) -> io::Result<ReadinessResponse> {
    let f = decode_fields(bytes, 15)?;
    check_tag(&f, "response")?;
    let initialized = match f[14] {
        "true" => true,
        "false" => false,
        _ => return Err(invalid("invalid initialized boolean")),
    };
    let value = ReadinessResponse {
        protocol: ReadyProtocol::TheseusFixtureReadyV1,
        lease: f[2].into(),
        slot: f[3].into(),
        resource_binding_digest: unhex(f[4])?,
        start_or_activation_operation: f[5].into(),
        action_digest: unhex(f[6])?,
        nonce: unhex(f[7])?,
        artifact_length: decimal(f[8])?,
        artifact_sha256: unhex(f[9])?,
        version_literal: f[10].into(),
        data_sha256: unhex(f[11])?,
        configuration_digest: unhex(f[12])?,
        instance: f[13].into(),
        initialized,
    };
    validate_response(&value)?;
    Ok(value)
}
fn encode_fields(fields: &[String]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    for field in fields {
        out.extend_from_slice(field.len().to_string().as_bytes());
        out.push(b':');
        out.extend_from_slice(field.as_bytes());
        out.push(b',');
    }
    if out.len() > MAX_FRAME_BYTES {
        return Err(invalid("readiness frame exceeds 4096 bytes"));
    }
    Ok(out)
}
fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        write!(text, "{byte:02x}").expect("string formatting");
    }
    text
}

fn decimal(value: &str) -> io::Result<u64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|c| c.is_ascii_digit())
    {
        return Err(invalid("noncanonical unsigned integer"));
    }
    value.parse().map_err(|_| invalid("integer overflow"))
}
fn unhex<const N: usize>(value: &str) -> io::Result<[u8; N]> {
    if value.len() != N * 2
        || !value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(invalid("noncanonical hex commitment"));
    }
    let mut out = [0; N];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| invalid("invalid hex"))?;
    }
    Ok(out)
}
fn decode_fields(bytes: &[u8], count: usize) -> io::Result<Vec<&str>> {
    if bytes.len() > MAX_FRAME_BYTES || std::str::from_utf8(bytes).is_err() {
        return Err(invalid("unbounded or non-UTF8 readiness frame"));
    }
    let mut rest = bytes;
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let colon = rest
            .iter()
            .position(|&c| c == b':')
            .ok_or_else(|| invalid("missing netstring length"))?;
        if colon > 4 {
            return Err(invalid("unbounded netstring length"));
        }
        let length =
            decimal(std::str::from_utf8(&rest[..colon]).map_err(|_| invalid("invalid length"))?)?
                as usize;
        rest = &rest[colon + 1..];
        if length >= rest.len() || rest[length] != b',' {
            return Err(invalid("partial or malformed netstring"));
        }
        fields.push(
            std::str::from_utf8(&rest[..length]).map_err(|_| invalid("netstring splits UTF8"))?,
        );
        rest = &rest[length + 1..];
    }
    if !rest.is_empty() {
        return Err(invalid("unknown trailing readiness fields"));
    }
    Ok(fields)
}
fn check_tag(fields: &[&str], kind: &str) -> io::Result<()> {
    if fields[0] != "theseus_fixture_ready_v1" || fields[1] != kind {
        Err(invalid("unsupported readiness protocol or message kind"))
    } else {
        Ok(())
    }
}
fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
}
fn literal(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.as_bytes().contains(&0)
}
fn validate_challenge(c: &ReadinessChallenge) -> io::Result<()> {
    if !identifier(&c.lease, 128)
        || !identifier(&c.slot, 32)
        || !identifier(&c.start_or_activation_operation, 128)
    {
        Err(invalid("invalid readiness correlation identifier"))
    } else {
        Ok(())
    }
}
fn validate_response(r: &ReadinessResponse) -> io::Result<()> {
    if !identifier(&r.lease, 128)
        || !identifier(&r.slot, 32)
        || !identifier(&r.start_or_activation_operation, 128)
        || !literal(&r.version_literal)
        || !literal(&r.instance)
    {
        Err(invalid("invalid readiness response field"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn challenge() -> ReadinessChallenge {
        ReadinessChallenge {
            protocol: ReadyProtocol::TheseusFixtureReadyV1,
            lease: "lease-original".into(),
            slot: "service".into(),
            resource_binding_digest: [1; 32],
            start_or_activation_operation: "start-original".into(),
            action_digest: [2; 32],
            nonce: [3; 16],
            expectation_digest: [4; 32],
        }
    }
    pub(super) fn expectation() -> ReadinessExpectation {
        ReadinessExpectation {
            artifact: ArtifactRole::Target,
            artifact_length: 42,
            artifact_sha256: [5; 32],
            version_literal: "1.2.3=fixture\nutf8-é".into(),
            data_sha256: [6; 32],
            configuration_digest: [7; 32],
        }
    }
    pub(super) fn response() -> ReadinessResponse {
        let c = challenge();
        let e = expectation();
        ReadinessResponse {
            protocol: c.protocol,
            lease: c.lease,
            slot: c.slot,
            resource_binding_digest: c.resource_binding_digest,
            start_or_activation_operation: c.start_or_activation_operation,
            action_digest: c.action_digest,
            nonce: c.nonce,
            artifact_length: e.artifact_length,
            artifact_sha256: e.artifact_sha256,
            version_literal: e.version_literal,
            data_sha256: e.data_sha256,
            configuration_digest: e.configuration_digest,
            instance: "original-invocation".into(),
            initialized: true,
        }
    }
    #[test]
    fn comparison_uses_original_expectation_and_separates_application_mismatch() {
        let c = challenge();
        let e = expectation();
        let r = response();
        assert_eq!(compare(&c, &e, "original-invocation", &r), None);
        assert_eq!(
            compare(&c, &e, "other-original-instance", &r),
            Some(ReadinessFailure::WrongInstance)
        );
        let mut wrong_version = r.clone();
        wrong_version.version_literal = "different".into();
        let failure = compare(&c, &e, "original-invocation", &wrong_version);
        assert_eq!(failure, Some(ReadinessFailure::WrongVersion));
        assert!(correlation_authenticated(failure));
        let mut wrong_nonce = r;
        wrong_nonce.nonce = [88; 16];
        let failure = compare(&c, &e, "original-invocation", &wrong_nonce);
        assert_eq!(failure, Some(ReadinessFailure::WrongNonce));
        assert!(!correlation_authenticated(failure));
        assert!(!correlation_authenticated(Some(
            ReadinessFailure::WrongPeer
        )));
        assert!(!correlation_authenticated(Some(
            ReadinessFailure::MalformedResponse
        )));
    }
    #[test]
    fn strict_wire_roundtrip_preserves_every_binding_and_literal() {
        let c = challenge();
        assert_eq!(decode_challenge(&encode_challenge(&c).unwrap()).unwrap(), c);
        let r = response();
        assert_eq!(decode_response(&encode_response(&r).unwrap()).unwrap(), r);
    }
    #[test]
    fn strict_wire_rejects_unknown_trailing_and_noncanonical_fields() {
        let body = encode_response(&response()).unwrap();
        let mut extra = body.clone();
        extra.extend_from_slice(b"1:x,");
        assert!(decode_response(&extra).is_err());
        let mut leading = vec![b'0'];
        leading.extend_from_slice(&body);
        assert!(decode_response(&leading).is_err());
        let mut invalid_utf8 = body;
        invalid_utf8[5] = 0xff;
        assert!(decode_response(&invalid_utf8).is_err());
        assert!(decode_response(&vec![b'x'; 4097]).is_err());
    }
}

fn deadline_failure(
    deadline: Instant,
    cancel: &AtomicBool,
) -> io::Result<Option<ReadinessFailure>> {
    deadline_failure_at(deadline, Instant::now(), cancel.load(Ordering::Acquire))
}
fn deadline_failure_at(
    deadline: Instant,
    now: Instant,
    cancelled: bool,
) -> io::Result<Option<ReadinessFailure>> {
    if cancelled {
        return Ok(Some(ReadinessFailure::Cancelled));
    }
    let Some(left) = deadline.checked_duration_since(now) else {
        return Ok(Some(ReadinessFailure::Deadline));
    };
    if left > Duration::from_secs(30) {
        return Err(invalid("readiness deadline exceeds 30 seconds"));
    }
    Ok(None)
}
/// Application facts retain separately authenticated original-peer correlation.
fn correlation_authenticated(failure: Option<ReadinessFailure>) -> bool {
    matches!(
        failure,
        None | Some(
            ReadinessFailure::WrongArtifact
                | ReadinessFailure::WrongVersion
                | ReadinessFailure::WrongData
                | ReadinessFailure::WrongConfiguration
                | ReadinessFailure::NotInitialized
        )
    )
}
fn compare(
    c: &ReadinessChallenge,
    e: &ReadinessExpectation,
    cooperative_instance: &str,
    r: &ReadinessResponse,
) -> Option<ReadinessFailure> {
    if r.protocol != c.protocol {
        Some(ReadinessFailure::WrongProtocol)
    } else if r.lease != c.lease {
        Some(ReadinessFailure::WrongLease)
    } else if r.slot != c.slot {
        Some(ReadinessFailure::WrongSlot)
    } else if r.resource_binding_digest != c.resource_binding_digest {
        Some(ReadinessFailure::WrongResourceBinding)
    } else if r.start_or_activation_operation != c.start_or_activation_operation
        || r.action_digest != c.action_digest
    {
        Some(ReadinessFailure::WrongAction)
    } else if r.nonce != c.nonce {
        Some(ReadinessFailure::WrongNonce)
    } else if r.instance != cooperative_instance {
        Some(ReadinessFailure::WrongInstance)
    } else if r.artifact_length != e.artifact_length || r.artifact_sha256 != e.artifact_sha256 {
        Some(ReadinessFailure::WrongArtifact)
    } else if r.version_literal != e.version_literal {
        Some(ReadinessFailure::WrongVersion)
    } else if r.data_sha256 != e.data_sha256 {
        Some(ReadinessFailure::WrongData)
    } else if r.configuration_digest != e.configuration_digest {
        Some(ReadinessFailure::WrongConfiguration)
    } else if !r.initialized {
        Some(ReadinessFailure::NotInitialized)
    } else {
        None
    }
}

#[cfg(test)]
mod platform_model_tests {
    #[test]
    fn scheduling_slice_expiry_is_not_original_probe_expiry() {
        let start = Instant::now();
        let original = start + Duration::from_secs(5);
        let slice = start + Duration::from_millis(1);
        let completed = start + Duration::from_millis(5);
        assert_eq!(
            deadline_failure_at(slice, completed, false).unwrap(),
            Some(ReadinessFailure::Deadline)
        );
        assert_eq!(
            deadline_failure_at(original, completed, false).unwrap(),
            None
        );
        assert_eq!(
            deadline_failure_at(original, original + Duration::from_nanos(1), false).unwrap(),
            Some(ReadinessFailure::Deadline)
        );
        assert_eq!(
            deadline_failure_at(original, completed, true).unwrap(),
            Some(ReadinessFailure::Cancelled)
        );
    }

    use super::*;
    #[test]
    fn closed_pipe_names_refuse_remote_paths_and_dacl_principals_are_canonical() {
        assert_eq!(
            NamedPipe::new(r"\\.\pipe\theseus.fixture-1".into())
                .unwrap()
                .as_str(),
            r"\\.\pipe\theseus.fixture-1"
        );
        for path in [
            r"\\server\pipe\theseus.fixture",
            r"\\.\pipe\other",
            r"\\.\pipe\theseus.a/b",
            r"\\.\pipe\theseus.",
        ] {
            assert!(NamedPipe::new(path.into()).is_err());
        }
        assert_eq!(Sid::new("S-1-5-18".into()).unwrap().as_str(), "S-1-5-18");
        for value in [
            "S-1-1-0",
            "S-1-5-32-545",
            "S-01-5-18",
            "S-1-5-018",
            "S-1-5-18)(A;;GA;;;WD)",
        ] {
            assert!(Sid::new(value.into()).is_err(), "{value}");
        }
    }
}

/// Canonical local name rendered as the only supported pipe namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedPipe(String);
impl NamedPipe {
    pub fn new(path: String) -> io::Result<Self> {
        let name = path
            .strip_prefix(r"\\.\pipe\theseus.")
            .ok_or_else(|| invalid("only an explicit local Theseus pipe is admitted"))?;
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        {
            return Err(invalid("invalid closed named-pipe endpoint"));
        }
        Ok(Self(path))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
/// A canonical Windows token-user SID, never a POSIX account or broad group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sid(String);
impl Sid {
    pub fn new(value: String) -> io::Result<Self> {
        if value.len() > 184
            || !value.starts_with("S-1-5-")
            || matches!(value.as_str(), "S-1-5-11" | "S-1-5-32-545")
        {
            return Err(invalid("invalid token-user SID"));
        }
        let parts: Vec<_> = value[6..].split('-').collect();
        if parts.is_empty()
            || parts.len() > 15
            || parts
                .iter()
                .any(|p| decimal(p).is_err() || decimal(p).is_ok_and(|v| v > u32::MAX as u64))
        {
            return Err(invalid("invalid canonical SID subauthority"));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
