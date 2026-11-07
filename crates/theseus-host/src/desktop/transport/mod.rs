//! Explicit kernel-authenticated local broker I/O. Stopping a connection settles
//! only its original local endpoints; it grants no broker/process lifetime right.
//! Caller contract: endpoint and ancestor mappings remain exclusive and quiescent
//! throughout effects. Retained mapping checks do not enforce exclusion against
//! a hostile same-principal namespace writer.
use crate::{
    desktop::session::BrokerEndpoint,
    principal::CurrentPrincipal,
    stdio::{ReadStep, WriteStep},
};
use std::io;
mod identity;
pub use identity::{
    DirectoryOpenResult, open_child_directory_classified, retained_directory_identity,
};
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerCeiling {
    pub principal: CurrentPrincipal,
    pub session_id: Option<u32>,
}
/// Kernel measurements, never reconstructed from a declared broker header.
/// POSIX process sessions are not graphical login-session evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeasuredIdentity {
    pub principal: CurrentPrincipal,
    pub diagnostic_pid: Option<u32>,
    pub session_id: Option<u32>,
    pub posix_process_session: Option<i32>,
    pub window_station: Option<String>,
    pub desktop: Option<String>,
}
pub fn measure_local_identity() -> io::Result<MeasuredIdentity> {
    platform::measure_local_identity()
}
fn admit(identity: &MeasuredIdentity, ceiling: &PeerCeiling) -> io::Result<()> {
    if identity.principal != ceiling.principal
        || ceiling
            .session_id
            .is_some_and(|session| identity.session_id != Some(session))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "kernel peer/local process differs from explicit ceiling",
        ));
    }
    Ok(())
}
pub trait FrameIo {
    fn try_read(&mut self, bytes: &mut [u8]) -> io::Result<ReadStep>;
    fn try_write(&mut self, bytes: &[u8]) -> io::Result<WriteStep>;
    fn write_in_flight(&self) -> bool;
    fn request_stop(&mut self);
    fn poll_settle(&mut self) -> io::Result<bool>;
    fn is_settled(&self) -> bool;
}
pub struct TransportStartFailure {
    error: io::Error,
    owner: Option<Box<AuthenticatedStream>>,
}
impl TransportStartFailure {
    pub fn error(&self) -> &io::Error {
        &self.error
    }
    pub fn into_parts(self) -> (io::Error, Option<Box<AuthenticatedStream>>) {
        (self.error, self.owner)
    }
}
impl std::fmt::Debug for TransportStartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportStartFailure")
            .field("error", &self.error)
            .field("retained_owner", &self.owner.is_some())
            .finish()
    }
}
pub struct AuthenticatedStream {
    native: Option<platform::Stream>,
    errors: Vec<io::Error>,
    truncated: bool,
    failed: bool,
    stopping: bool,
}
impl AuthenticatedStream {
    pub fn connect(
        endpoint: BrokerEndpoint,
        local: PeerCeiling,
        peer: PeerCeiling,
    ) -> Result<Self, TransportStartFailure> {
        let native = platform::Stream::connect(endpoint, local, peer)
            .map_err(|error| TransportStartFailure { error, owner: None })?;
        Ok(Self::from_native(native))
    }
    fn from_native(native: platform::Stream) -> Self {
        Self {
            native: Some(native),
            errors: Vec::new(),
            truncated: false,
            failed: false,
            stopping: false,
        }
    }
    fn remember<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                let kind = error.kind();
                if self.errors.len() < 16 {
                    self.errors.push(error)
                } else {
                    self.truncated = true;
                }
                Err(io::Error::new(
                    kind,
                    "original broker transport error retained",
                ))
            }
        }
    }
    pub fn identity(&self) -> Option<&MeasuredIdentity> {
        self.native.as_ref().and_then(platform::Stream::identity)
    }
    pub fn local_identity(&self) -> Option<&MeasuredIdentity> {
        self.native.as_ref().map(platform::Stream::local_identity)
    }
    pub fn retained_errors(&self) -> &[io::Error] {
        &self.errors
    }
    pub fn errors_truncated(&self) -> bool {
        self.truncated
    }
    pub fn poll_connected(&mut self) -> io::Result<bool> {
        if self.failed || self.stopping {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "original connection revoked/stopping",
            ));
        }
        let result = self
            .native
            .as_mut()
            .ok_or_else(|| io::Error::other("connection already settled"))?
            .poll_connected();
        if result.is_err() {
            self.failed = true;
        }
        self.remember(result)
    }
}
impl FrameIo for AuthenticatedStream {
    fn try_read(&mut self, bytes: &mut [u8]) -> io::Result<ReadStep> {
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bounded read buffer required",
            ));
        }
        if !self.poll_connected()? {
            return Ok(ReadStep::Pending);
        }
        let result = self
            .native
            .as_mut()
            .expect("original stream")
            .try_read(bytes);
        self.remember(result)
    }
    fn try_write(&mut self, bytes: &[u8]) -> io::Result<WriteStep> {
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bounded write buffer required",
            ));
        }
        if !self.poll_connected()? {
            return Ok(WriteStep::Pending);
        }
        let result = self
            .native
            .as_mut()
            .expect("original stream")
            .try_write(bytes);
        self.remember(result)
    }
    fn write_in_flight(&self) -> bool {
        self.native
            .as_ref()
            .is_some_and(platform::Stream::write_in_flight)
    }
    fn request_stop(&mut self) {
        self.stopping = true;
        if let Some(native) = &mut self.native {
            native.request_stop();
        }
    }
    fn poll_settle(&mut self) -> io::Result<bool> {
        if !self.stopping {
            return Ok(false);
        }
        let result = self
            .native
            .as_mut()
            .map_or(Ok(true), platform::Stream::poll_settle);
        if matches!(result, Ok(true)) {
            self.native = None;
        }
        self.remember(result)
    }
    fn is_settled(&self) -> bool {
        self.native.is_none()
    }
}
impl Drop for AuthenticatedStream {
    fn drop(&mut self) {
        self.request_stop();
        if !matches!(self.poll_settle(), Ok(true)) {
            std::mem::forget(self.native.take());
            std::mem::forget(std::mem::take(&mut self.errors));
        }
    }
}
pub struct ListenerStartFailure {
    error: io::Error,
    owner: Option<Box<LocalListener>>,
}
impl ListenerStartFailure {
    pub fn error(&self) -> &io::Error {
        &self.error
    }
    pub fn into_parts(self) -> (io::Error, Option<Box<LocalListener>>) {
        (self.error, self.owner)
    }
}
impl std::fmt::Debug for ListenerStartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListenerStartFailure")
            .field("error", &self.error)
            .field("retained_owner", &self.owner.is_some())
            .finish()
    }
}
pub struct LocalListener {
    native: Option<platform::Listener>,
    errors: Vec<io::Error>,
    truncated: bool,
    stopping: bool,
}
impl LocalListener {
    pub fn bind(
        endpoint: BrokerEndpoint,
        local: PeerCeiling,
        peer: PeerCeiling,
    ) -> Result<Self, ListenerStartFailure> {
        let native = platform::Listener::prepare(endpoint, local, peer)
            .map_err(|error| ListenerStartFailure { error, owner: None })?;
        let mut owner = Self {
            native: Some(native),
            errors: Vec::new(),
            truncated: false,
            stopping: false,
        };
        if let Err(error) = owner
            .native
            .as_mut()
            .expect("original listener")
            .start_bind()
        {
            return Err(ListenerStartFailure {
                error,
                owner: Some(Box::new(owner)),
            });
        }
        Ok(owner)
    }
    fn remember<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                let kind = error.kind();
                if self.errors.len() < 16 {
                    self.errors.push(error)
                } else {
                    self.truncated = true;
                }
                Err(io::Error::new(
                    kind,
                    "original broker listener error retained",
                ))
            }
        }
    }
    pub fn local_identity(&self) -> Option<&MeasuredIdentity> {
        self.native.as_ref().map(platform::Listener::local_identity)
    }
    pub fn try_accept(&mut self) -> io::Result<Option<AuthenticatedStream>> {
        if self.stopping {
            return Err(io::Error::other("listener stopping"));
        }
        let result = self
            .native
            .as_mut()
            .ok_or_else(|| io::Error::other("listener settled"))?
            .try_accept();
        self.remember(result)
            .map(|value| value.map(AuthenticatedStream::from_native))
    }
    pub fn request_stop(&mut self) {
        self.stopping = true;
        if let Some(native) = &mut self.native {
            native.request_stop();
        }
    }
    pub fn poll_settle(&mut self) -> io::Result<bool> {
        if !self.stopping {
            return Ok(false);
        }
        let result = self
            .native
            .as_mut()
            .map_or(Ok(true), platform::Listener::poll_settle);
        if matches!(result, Ok(true)) {
            self.native = None;
        }
        self.remember(result)
    }
    pub fn is_settled(&self) -> bool {
        self.native.is_none()
    }
    pub fn retained_errors(&self) -> &[io::Error] {
        &self.errors
    }
    pub fn errors_truncated(&self) -> bool {
        self.truncated
    }
}
impl Drop for LocalListener {
    fn drop(&mut self) {
        self.request_stop();
        if !matches!(self.poll_settle(), Ok(true)) {
            std::mem::forget(self.native.take());
            std::mem::forget(std::mem::take(&mut self.errors));
        }
    }
}
#[cfg(test)]
mod tests;
