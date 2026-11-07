use crate::{Envelope, Message, ProtocolError, Role, SessionId};

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Waiting,
    Established,
    Failed,
}

/// Validates one peer handshake. Establishment is neither authentication nor
/// authority to reserve a target or execute an operation.
pub struct HandshakeGuard {
    session: SessionId,
    peer: Role,
    state: State,
}
impl HandshakeGuard {
    pub fn new(session: SessionId, peer: Role) -> Self {
        Self {
            session,
            peer,
            state: State::Waiting,
        }
    }

    pub fn accept(&mut self, envelope: &Envelope) -> Result<(), ProtocolError> {
        if self.state == State::Failed {
            return Err(ProtocolError::UnexpectedMessage);
        }
        let result = if envelope.session != self.session {
            Err(ProtocolError::SessionMismatch)
        } else if envelope.sequence != 0 {
            Err(ProtocolError::UnexpectedSequence)
        } else if self.state == State::Established {
            Err(ProtocolError::UnexpectedMessage)
        } else {
            match envelope.message {
                Message::Hello { role } if role == self.peer => Ok(()),
                _ => Err(ProtocolError::UnexpectedMessage),
            }
        };
        self.state = if result.is_ok() {
            State::Established
        } else {
            State::Failed
        };
        result
    }

    pub fn is_established(&self) -> bool {
        self.state == State::Established
    }
}
