use std::io;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    SetConsoleCtrlHandler,
};

struct State {
    registered: bool,
    flags: Vec<Arc<AtomicBool>>,
}
static STATE: Mutex<State> = Mutex::new(State {
    registered: false,
    flags: Vec::new(),
});

// Windows invokes this on a separate thread. The lock serializes pointer access
// with Drop; no raw callback pointer can outlive its owning Arc. The handler
// performs no application cleanup, I/O, allocation, or process management.
unsafe extern "system" fn control(event: u32) -> i32 {
    if !matches!(
        event,
        CTRL_C_EVENT
            | CTRL_BREAK_EVENT
            | CTRL_CLOSE_EVENT
            | CTRL_LOGOFF_EVENT
            | CTRL_SHUTDOWN_EVENT
    ) {
        return 0;
    }
    let state = STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    for flag in &state.flags {
        flag.store(true, Ordering::SeqCst);
    }
    i32::from(!state.flags.is_empty())
}

pub struct CancellationRegistration(Arc<AtomicBool>);
impl CancellationRegistration {
    pub fn install(flag: Arc<AtomicBool>) -> io::Result<Self> {
        let mut state = STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        if !state.registered {
            // SAFETY: control has static code lifetime and exclusively accesses
            // callback data through STATE's synchronized owned Arcs.
            if unsafe { SetConsoleCtrlHandler(Some(control), 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            state.registered = true;
        }
        state.flags.push(flag.clone());
        Ok(Self(flag))
    }
}
impl Drop for CancellationRegistration {
    fn drop(&mut self) {
        let mut state = STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(index) = state
            .flags
            .iter()
            .position(|flag| Arc::ptr_eq(flag, &self.0))
        {
            state.flags.swap_remove(index);
        }
        if state.flags.is_empty() {
            // On unregister failure the static handler remains safe: there are no
            // dangling callback targets, and the next install reuses registration.
            if unsafe { SetConsoleCtrlHandler(Some(control), 0) } != 0 {
                state.registered = false;
            }
        }
    }
}
