use crate::{Envelope, Message, ProtocolError, Refusal, Role, SessionId};
use serde::{Deserialize, Serialize};
use std::io::Write;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEnvelope {
    version: u16,
    session: String,
    sequence: u64,
    message: WireMessage,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireMessage {
    Hello {
        role: WireRole,
        protocol_min: u16,
        protocol_max: u16,
    },
    Refused {
        code: WireRefusal,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireRole {
    Controller,
    Helper,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireRefusal {
    UnsupportedVersion,
    SessionMismatch,
    UnexpectedSequence,
    UnexpectedMessage,
}

pub(super) fn decode(body: &[u8]) -> Result<Envelope, ProtocolError> {
    if body.is_empty() || body.len() > crate::MAX_BODY_BYTES {
        return Err(ProtocolError::InvalidLength);
    }
    let text = std::str::from_utf8(body).map_err(|_| ProtocolError::InvalidUtf8)?;
    let value = crate::strict::parse(text).map_err(|_| ProtocolError::InvalidJson)?;
    let wire: WireEnvelope =
        serde_json::from_value(value).map_err(|_| ProtocolError::InvalidEnvelope)?;
    if wire.version != 1 {
        return Err(ProtocolError::UnsupportedVersion);
    }
    let session = SessionId::parse(&wire.session)?;
    let message = match wire.message {
        WireMessage::Hello {
            role,
            protocol_min,
            protocol_max,
        } => {
            if protocol_min != 1 || protocol_max != 1 {
                return Err(ProtocolError::UnsupportedVersion);
            }
            Message::Hello {
                role: match role {
                    WireRole::Controller => Role::Controller,
                    WireRole::Helper => Role::Helper,
                },
            }
        }
        WireMessage::Refused { code } => Message::Refused {
            code: match code {
                WireRefusal::UnsupportedVersion => Refusal::UnsupportedVersion,
                WireRefusal::SessionMismatch => Refusal::SessionMismatch,
                WireRefusal::UnexpectedSequence => Refusal::UnexpectedSequence,
                WireRefusal::UnexpectedMessage => Refusal::UnexpectedMessage,
            },
        },
    };
    Ok(Envelope {
        session,
        sequence: wire.sequence,
        message,
    })
}

pub(super) fn write_body(envelope: &Envelope, writer: impl Write) -> Result<(), ProtocolError> {
    let message = match &envelope.message {
        Message::Hello { role } => WireMessage::Hello {
            role: match role {
                Role::Controller => WireRole::Controller,
                Role::Helper => WireRole::Helper,
            },
            protocol_min: 1,
            protocol_max: 1,
        },
        Message::Refused { code } => WireMessage::Refused {
            code: match code {
                Refusal::UnsupportedVersion => WireRefusal::UnsupportedVersion,
                Refusal::SessionMismatch => WireRefusal::SessionMismatch,
                Refusal::UnexpectedSequence => WireRefusal::UnexpectedSequence,
                Refusal::UnexpectedMessage => WireRefusal::UnexpectedMessage,
            },
        },
    };
    // The only intermediate string is the fixed32-character session ID.
    let wire = WireEnvelope {
        version: 1,
        session: envelope.session.to_hex(),
        sequence: envelope.sequence,
        message,
    };
    serde_json::to_writer(writer, &wire).map_err(|_| ProtocolError::InvalidEnvelope)
}
