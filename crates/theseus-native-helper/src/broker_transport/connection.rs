use std::{collections::VecDeque, io};
use theseus_host::{
    desktop::{
        session::BrokerEndpoint,
        transport::{AuthenticatedStream, FrameIo, PeerCeiling},
    },
    principal::CurrentPrincipal,
    services::readiness::{NamedPipe, UnixPath},
    stdio::{ReadStep, WriteStep},
};
use theseus_native_client::{BrokerSession, Event, RequestId};
use theseus_native_protocol::v2;
use v2::broker::{BrokerPrincipal, IntegrationBrokerHeader, SuppliedDisplay};
pub struct ConnectionFailure {
    error: io::Error,
    owner: Option<Box<BrokerConnection>>,
}
impl ConnectionFailure {
    pub fn error(&self) -> &io::Error {
        &self.error
    }
    pub fn into_parts(self) -> (io::Error, Option<Box<BrokerConnection>>) {
        (self.error, self.owner)
    }
}
impl std::fmt::Debug for ConnectionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionFailure")
            .field("error", &self.error)
            .field("retained_owner", &self.owner.is_some())
            .finish()
    }
}
pub struct BrokerConnection {
    io: Option<AuthenticatedStream>,
    session: Option<BrokerSession>,
    input: Vec<u8>,
    offset: usize,
    pending: Vec<RequestId>,
    replies: VecDeque<v2::Reply>,
    errors: Vec<io::Error>,
    truncated: bool,
    failed: bool,
    stopping: bool,
}
impl BrokerConnection {
    pub fn connect(expected: &IntegrationBrokerHeader) -> Result<Self, ConnectionFailure> {
        expected.validate().map_err(|e| ConnectionFailure {
            error: io::Error::other(e),
            owner: None,
        })?;
        let endpoint = endpoint(&expected.broker_endpoint)
            .map_err(|error| ConnectionFailure { error, owner: None })?;
        let local = PeerCeiling {
            principal: principal(&expected.helper_principal),
            session_id: None,
        };
        let peer = PeerCeiling {
            principal: principal(&expected.broker_principal),
            session_id: match expected.display {
                SuppliedDisplay::WindowsInteractive { session_id, .. } => Some(session_id),
                _ => None,
            },
        };
        let stream = AuthenticatedStream::connect(endpoint, local, peer).map_err(|failure| {
            let (error, stream) = failure.into_parts();
            let owner = stream.map(|io| Box::new(Self::with_io(*io)));
            ConnectionFailure { error, owner }
        })?;
        let mut owner = Self::with_io(stream);
        match BrokerSession::new(expected.clone()) {
            Ok(session) => owner.session = Some(session),
            Err(error) => {
                return Err(ConnectionFailure {
                    error: io::Error::other(error),
                    owner: Some(Box::new(owner)),
                });
            }
        }
        Ok(owner)
    }
    fn with_io(io: AuthenticatedStream) -> Self {
        Self {
            io: Some(io),
            session: None,
            input: Vec::with_capacity(4096),
            offset: 0,
            pending: Vec::with_capacity(2),
            replies: VecDeque::with_capacity(2),
            errors: Vec::new(),
            truncated: false,
            failed: false,
            stopping: false,
        }
    }
    fn retain<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        match result {
            Ok(v) => Ok(v),
            Err(error) => {
                self.failed = true;
                let kind = error.kind();
                if self.errors.len() < 16 {
                    self.errors.push(error)
                } else {
                    self.truncated = true;
                }
                Err(io::Error::new(
                    kind,
                    "original broker conversation failure retained",
                ))
            }
        }
    }
    fn drive(&mut self) -> io::Result<()> {
        if self.failed || self.stopping {
            return Err(io::Error::other(
                "original broker connection failed/stopping",
            ));
        }
        let connected = self
            .io
            .as_mut()
            .ok_or_else(|| io::Error::other("connection settled"))?
            .poll_connected()?;
        if !connected {
            return Ok(());
        }
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| io::Error::other("broker session initialization failed"))?;
        if session.timed_out() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "original broker handshake deadline",
            ));
        }
        if let Some(bytes) = session.pending_output() {
            match self.io.as_mut().expect("original IO").try_write(bytes)? {
                WriteStep::Bytes(n) => session.advance_output(n).map_err(io::Error::other)?,
                WriteStep::Pending => {}
            }
        }
        for _ in 0..4 {
            if self.offset == self.input.len() {
                self.input.clear();
                self.offset = 0;
                let mut buffer = [0; 4096];
                match self
                    .io
                    .as_mut()
                    .expect("original IO")
                    .try_read(&mut buffer)?
                {
                    ReadStep::Bytes(n) => self.input.extend_from_slice(&buffer[..n]),
                    ReadStep::Pending => break,
                    ReadStep::Eof => {
                        session.eof().map_err(io::Error::other)?;
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "original broker connection lost",
                        ));
                    }
                }
            }
            let (consumed, event) = session
                .feed_input(&self.input[self.offset..])
                .map_err(io::Error::other)?;
            if consumed == 0 || consumed > self.input.len() - self.offset {
                return Err(io::Error::other("invalid original decoder progress"));
            }
            self.offset += consumed;
            match event {
                None | Some(Event::Ready) => {}
                Some(Event::Reply { request, reply }) => {
                    let index = self
                        .pending
                        .iter()
                        .position(|id| *id == request)
                        .ok_or_else(|| io::Error::other("reply did not match original request"))?;
                    self.pending.remove(index);
                    if self.replies.len() >= 2 {
                        return Err(io::Error::other("bounded original reply queue full"));
                    }
                    self.replies.push_back(reply);
                }
                Some(_) => return Err(io::Error::other("unexpected broker conversation event")),
            }
        }
        Ok(())
    }
    pub fn poll_ready(&mut self) -> io::Result<bool> {
        let result = self.drive();
        self.retain(result)?;
        Ok(self.session.as_ref().is_some_and(BrokerSession::is_ready))
    }
    pub fn start_request(&mut self, request: v2::Request) -> io::Result<()> {
        if !self.poll_ready()? {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "broker exact-header handshake pending",
            ));
        }
        let cancel = matches!(request, v2::Request::CancelDesktop { .. });
        if self.pending.len() >= 2
            || (!cancel && !self.pending.is_empty())
            || self.replies.len() + self.pending.len() >= 2
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "original broker request slot occupied",
            ));
        }
        let id = self
            .session
            .as_mut()
            .expect("ready broker session")
            .submit(request)
            .map_err(io::Error::other)?;
        self.pending.push(id);
        Ok(())
    }
    pub fn poll_reply(&mut self) -> io::Result<Option<v2::Reply>> {
        if let Some(reply) = self.replies.pop_front() {
            return Ok(Some(reply));
        }
        let result = self.drive();
        self.retain(result)?;
        Ok(self.replies.pop_front())
    }
    pub fn request_stop(&mut self) {
        self.stopping = true;
        if let Some(io) = &mut self.io {
            io.request_stop();
        }
    }
    pub fn poll_settle(&mut self) -> io::Result<bool> {
        if !self.stopping {
            return Ok(false);
        }
        let result = self.io.as_mut().map_or(Ok(true), FrameIo::poll_settle);
        if matches!(result, Ok(true)) {
            self.io = None;
        }
        self.retain(result)
    }
    pub fn is_settled(&self) -> bool {
        self.io.is_none()
    }
    pub fn retained_errors(&self) -> &[io::Error] {
        &self.errors
    }
    pub fn errors_truncated(&self) -> bool {
        self.truncated
    }
}
impl Drop for BrokerConnection {
    fn drop(&mut self) {
        self.request_stop();
        if !matches!(self.poll_settle(), Ok(true)) {
            std::mem::forget(self.io.take());
            std::mem::forget(self.session.take());
            std::mem::forget(std::mem::take(&mut self.input));
            std::mem::forget(std::mem::take(&mut self.pending));
            std::mem::forget(std::mem::take(&mut self.replies));
            std::mem::forget(std::mem::take(&mut self.errors));
        }
    }
}
pub(super) fn endpoint(value: &v2::LocalEndpoint) -> io::Result<BrokerEndpoint> {
    match value {
        v2::LocalEndpoint::UnixPath { path } => {
            Ok(BrokerEndpoint::Unix(UnixPath::new(path.clone())?))
        }
        v2::LocalEndpoint::NamedPipe { path } => {
            Ok(BrokerEndpoint::Pipe(NamedPipe::new(path.clone())?))
        }
    }
}
pub(super) fn principal(value: &BrokerPrincipal) -> CurrentPrincipal {
    match value {
        BrokerPrincipal::Posix {
            effective_uid,
            effective_gid,
        } => CurrentPrincipal::Posix {
            effective_uid: *effective_uid,
            effective_gid: *effective_gid,
        },
        BrokerPrincipal::Windows { token_user_sid } => CurrentPrincipal::Windows {
            token_user_sid: token_user_sid.clone(),
        },
    }
}
