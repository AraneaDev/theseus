//! Runtime-interval PID pinning of the original registration.
use super::*;

pub(super) fn runtime_changed(o: &OriginalRegistration, status: ServiceStatus) -> bool {
    pinned_pid_changed(&o.runtime_pid, status)
}
/// Runtime interval of the original registration. Only the helper's own Start
/// or an admitted SUT transition opens it; the first live PID observed while
/// open is pinned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RuntimePin {
    Open,
    Pinned(u32),
    /// An admitted transition left the service stopped: any live PID before
    /// the helper's next Start is a foreign start.
    Closed,
}
/// First observed live PID is pinned; any different live PID, or any live PID
/// while closed, is a change. Statuses without a live PID never change it.
pub(super) fn pinned_pid_changed(pin: &Cell<RuntimePin>, status: ServiceStatus) -> bool {
    match (pin.get(), status.observed_process_id()) {
        (_, None) => false,
        (RuntimePin::Open, Some(pid)) => {
            pin.set(RuntimePin::Pinned(pid));
            false
        }
        (RuntimePin::Pinned(previous), Some(pid)) => previous != pid,
        (RuntimePin::Closed, Some(_)) => true,
    }
}
/// Qualification pins a running service's PID; a qualified service that is
/// stopped starts closed, so only the helper's own Start (or an admitted SUT
/// transition) opens its first runtime interval.
pub(super) fn initial_runtime_pin(status: Option<ServiceStatus>) -> Cell<RuntimePin> {
    let pin = Cell::new(
        status
            .and_then(|s| s.observed_process_id())
            .map_or(RuntimePin::Open, RuntimePin::Pinned),
    );
    close_stopped_interval(&pin, status);
    pin
}
/// An interval still open after an admitted transition observed the service
/// stopped (or stopping) is closed; a pending start keeps it open so the PID
/// the transition itself starts is pinned.
pub(super) fn close_stopped_interval(pin: &Cell<RuntimePin>, status: Option<ServiceStatus>) {
    if pin.get() == RuntimePin::Open
        && status
            .is_some_and(|s| matches!(s.state(), RuntimeState::Stopped | RuntimeState::StopPending))
    {
        pin.set(RuntimePin::Closed);
    }
}
