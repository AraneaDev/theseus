use std::io;
use std::sync::{Arc, atomic::AtomicBool};

pub struct CancellationRegistration(Vec<signal_hook::SigId>);
impl CancellationRegistration {
    pub fn install(flag: Arc<AtomicBool>) -> io::Result<Self> {
        let mut registration = Self(Vec::new());
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            registration
                .0
                .push(signal_hook::flag::register(signal, flag.clone())?);
        }
        Ok(registration)
    }
}
impl Drop for CancellationRegistration {
    fn drop(&mut self) {
        for id in self.0.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}
