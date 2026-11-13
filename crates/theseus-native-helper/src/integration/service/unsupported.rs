//! Service runtime stubs for platforms without a native service manager.
use super::*;
impl ServiceRuntime {
    pub(crate) fn inspect(
        &mut self,
        _lease: v2::LeaseId,
        _slot: &v2::ResourceSlot,
        _deadline: Instant,
        _cancel: &AtomicBool,
    ) -> io::Result<v2::ServiceObservation> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "systemd requires Linux",
        ))
    }
    pub(crate) fn action(
        &mut self,
        _slot: &v2::ResourceSlot,
        _prior: v2::Digest,
        _action: v2::ServiceAction,
        _deadline: Instant,
        _cancel: &AtomicBool,
    ) -> io::Result<Option<v2::ServiceOutcome>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "systemd requires Linux",
        ))
    }
    pub(crate) fn poll(
        &mut self,
        _deadline: Instant,
        _cancel: &AtomicBool,
    ) -> io::Result<Option<v2::ServiceOutcome>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "systemd requires Linux",
        ))
    }
    pub(crate) fn probe(
        &mut self,
        _request: &v2::Request,
        _deadline: Instant,
        _cancel: &AtomicBool,
    ) -> io::Result<v2::ReadinessObservation> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "service readiness requires Linux",
        ))
    }
    pub(crate) fn retire(&mut self) -> io::Result<()> {
        Ok(())
    }
}
