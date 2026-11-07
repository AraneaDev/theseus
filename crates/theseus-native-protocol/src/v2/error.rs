use std::fmt;
/// Fixed diagnostics never retain rejected peer data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidLength,
    InvalidUtf8,
    InvalidJson,
    InvalidEnvelope,
    UnsupportedVersion,
    InvalidValue,
    LimitExceeded,
    Truncated,
    Poisoned,
    SessionMismatch,
    UnexpectedSequence,
    UnexpectedMessage,
    UnexpectedReply,
    TooManyRequests,
    Closed,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidLength => "invalid v2 length",
            Self::InvalidUtf8 => "invalid v2 UTF-8",
            Self::InvalidJson => "invalid v2 JSON",
            Self::InvalidEnvelope => "invalid v2 envelope",
            Self::UnsupportedVersion => "unsupported v2 version",
            Self::InvalidValue => "invalid v2 value",
            Self::LimitExceeded => "v2 limit exceeded",
            Self::Truncated => "truncated v2 frame",
            Self::Poisoned => "v2 decoder poisoned",
            Self::SessionMismatch => "v2 session mismatch",
            Self::UnexpectedSequence => "unexpected v2 sequence",
            Self::UnexpectedMessage => "unexpected v2 message",
            Self::UnexpectedReply => "unexpected v2 reply",
            Self::TooManyRequests => "too many v2 requests",
            Self::Closed => "v2 session closed",
        })
    }
}
impl std::error::Error for Error {}
