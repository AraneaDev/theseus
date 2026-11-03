use std::{error::Error, fmt};

/// Opaque caller-supplied session identity. This does not authenticate a peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionId([u8; 16]);

impl SessionId {
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Accept only the single canonical lowercase hexadecimal spelling.
    pub fn parse(text: &str) -> Result<Self, ProtocolError> {
        let input = text.as_bytes();
        if input.len() != 32 {
            return Err(ProtocolError::InvalidSessionId);
        }
        let nibble = |byte| match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            _ => Err(ProtocolError::InvalidSessionId),
        };
        let mut bytes = [0; 16];
        let (pairs, _) = input.as_chunks::<2>();
        for (output, pair) in bytes.iter_mut().zip(pairs) {
            *output = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Ok(Self(bytes))
    }

    /// Exactly32 ASCII characters, independent of locale or host platform.
    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut text = String::with_capacity(32);
        for byte in self.0 {
            text.push(HEX[usize::from(byte >> 4)] as char);
            text.push(HEX[usize::from(byte & 15)] as char);
        }
        text
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Controller,
    Helper,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    UnsupportedVersion,
    SessionMismatch,
    UnexpectedSequence,
    UnexpectedMessage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Hello { role: Role },
    Refused { code: Refusal },
}

/// A handshake message; it grants no execution or reservation authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    pub session: SessionId,
    pub sequence: u64,
    pub message: Message,
}

/// Fixed diagnostics never retain or render rejected peer input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    InvalidLength,
    InvalidUtf8,
    InvalidJson,
    InvalidEnvelope,
    UnsupportedVersion,
    InvalidSessionId,
    Truncated,
    Poisoned,
    SessionMismatch,
    UnexpectedSequence,
    UnexpectedMessage,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidLength => "invalid frame length",
            Self::InvalidUtf8 => "invalid frame UTF-8",
            Self::InvalidJson => "invalid frame JSON",
            Self::InvalidEnvelope => "invalid protocol envelope",
            Self::UnsupportedVersion => "unsupported protocol version",
            Self::InvalidSessionId => "invalid session identity",
            Self::Truncated => "truncated protocol frame",
            Self::Poisoned => "protocol decoder is closed",
            Self::SessionMismatch => "session identity mismatch",
            Self::UnexpectedSequence => "unexpected protocol sequence",
            Self::UnexpectedMessage => "unexpected protocol message",
        };
        formatter.write_str(message)
    }
}
impl Error for ProtocolError {}
