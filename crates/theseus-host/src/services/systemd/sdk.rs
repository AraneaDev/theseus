//! Small owning, lazily loaded sd-bus ABI. No symbols are linked at build time.
use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    io, ptr,
    rc::Rc,
};

type BusPtr = *mut c_void;
type MessagePtr = *mut c_void;
type SlotPtr = *mut c_void;
type Handler = unsafe extern "C" fn(MessagePtr, *mut c_void, *mut BusError) -> c_int;
#[repr(C)]
pub(super) struct BusError {
    name: *const c_char,
    message: *const c_char,
    free: c_int,
}

macro_rules! sdk {
    ($($field:ident : $ty:ty = $symbol:literal),* $(,)?) => {
        pub(super) struct Api { handle: *mut c_void, $(pub $field: $ty,)* }
        impl Api {
            pub fn load() -> io::Result<Rc<Self>> {
                // SAFETY: a fixed library name and RTLD_LOCAL; never user-supplied code.
                let handle = unsafe { libc::dlopen(c"libsystemd.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
                if handle.is_null() { return Err(io::Error::new(io::ErrorKind::Unsupported, "libsystemd unavailable")); }
                let loaded = (|| -> io::Result<Self> {
                    Ok(Self { handle, $($field: {
                        // SAFETY: ABI signatures below match systemd 262 sd-bus.h.
                        let symbol = unsafe { libc::dlsym(handle, concat!($symbol, "\0").as_ptr().cast()) };
                        if symbol.is_null() { return Err(io::Error::new(io::ErrorKind::Unsupported, concat!("missing ", $symbol))); }
                        unsafe { std::mem::transmute::<*mut c_void, $ty>(symbol) }
                    },)* })
                })();
                match loaded {
                    Ok(api) => Ok(Rc::new(api)),
                    Err(error) => { unsafe { libc::dlclose(handle) }; Err(error) }
                }
            }
        }
        impl Drop for Api { fn drop(&mut self) { unsafe { libc::dlclose(self.handle) }; } }
    }
}
sdk! {
    new: unsafe extern "C" fn(*mut BusPtr)->c_int = "sd_bus_new",
    set_address: unsafe extern "C" fn(BusPtr,*const c_char)->c_int = "sd_bus_set_address",
    set_bus_client: unsafe extern "C" fn(BusPtr,c_int)->c_int = "sd_bus_set_bus_client",
    set_interactive: unsafe extern "C" fn(BusPtr,c_int)->c_int = "sd_bus_set_allow_interactive_authorization",
    set_timeout: unsafe extern "C" fn(BusPtr,u64)->c_int = "sd_bus_set_method_call_timeout",
    start: unsafe extern "C" fn(BusPtr)->c_int = "sd_bus_start",
    close_unref: unsafe extern "C" fn(BusPtr)->BusPtr = "sd_bus_close_unref",
    get_fd: unsafe extern "C" fn(BusPtr)->c_int = "sd_bus_get_fd",
    get_events: unsafe extern "C" fn(BusPtr)->c_int = "sd_bus_get_events",
    process: unsafe extern "C" fn(BusPtr,*mut MessagePtr)->c_int = "sd_bus_process",
    get_owner_creds: unsafe extern "C" fn(BusPtr,u64,*mut *mut c_void)->c_int = "sd_bus_get_owner_creds",
    creds_uid: unsafe extern "C" fn(*mut c_void,*mut u32)->c_int = "sd_bus_creds_get_euid",
    creds_unref: unsafe extern "C" fn(*mut c_void)->*mut c_void = "sd_bus_creds_unref",
    message_new: unsafe extern "C" fn(BusPtr,*mut MessagePtr,*const c_char,*const c_char,*const c_char,*const c_char)->c_int = "sd_bus_message_new_method_call",
    message_auto_start: unsafe extern "C" fn(MessagePtr,c_int)->c_int = "sd_bus_message_set_auto_start",
    message_interactive: unsafe extern "C" fn(MessagePtr,c_int)->c_int = "sd_bus_message_set_allow_interactive_authorization",
    message_append_basic: unsafe extern "C" fn(MessagePtr,c_char,*const c_void)->c_int = "sd_bus_message_append_basic",
    message_open: unsafe extern "C" fn(MessagePtr,c_char,*const c_char)->c_int = "sd_bus_message_open_container",
    message_close: unsafe extern "C" fn(MessagePtr)->c_int = "sd_bus_message_close_container",
    message_enter: unsafe extern "C" fn(MessagePtr,c_char,*const c_char)->c_int = "sd_bus_message_enter_container",
    message_exit: unsafe extern "C" fn(MessagePtr)->c_int = "sd_bus_message_exit_container",
    message_read_basic: unsafe extern "C" fn(MessagePtr,c_char,*mut c_void)->c_int = "sd_bus_message_read_basic",
    message_skip: unsafe extern "C" fn(MessagePtr,*const c_char)->c_int = "sd_bus_message_skip",
    message_ref: unsafe extern "C" fn(MessagePtr)->MessagePtr = "sd_bus_message_ref",
    message_unref: unsafe extern "C" fn(MessagePtr)->MessagePtr = "sd_bus_message_unref",
    message_error: unsafe extern "C" fn(MessagePtr)->*const BusError = "sd_bus_message_get_error",
    call_async: unsafe extern "C" fn(BusPtr,*mut SlotPtr,MessagePtr,Handler,*mut c_void,u64)->c_int = "sd_bus_call_async",
    match_signal: unsafe extern "C" fn(BusPtr,*mut SlotPtr,*const c_char,*const c_char,*const c_char,*const c_char,Handler,*mut c_void)->c_int = "sd_bus_match_signal",
    slot_unref: unsafe extern "C" fn(SlotPtr)->SlotPtr = "sd_bus_slot_unref",
}

pub(super) fn check(code: c_int) -> io::Result<c_int> {
    if code < 0 {
        Err(io::Error::from_raw_os_error(-code))
    } else {
        Ok(code)
    }
}
pub(super) fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn text(value: &str) -> io::Result<CString> {
    CString::new(value).map_err(|_| invalid("NUL in native value"))
}
fn copied(value: *const c_char, limit: usize) -> io::Result<String> {
    if value.is_null() {
        return Err(invalid("missing native string"));
    }
    // sd-bus owns and NUL-terminates this pointer for the message lifetime.
    let bytes = unsafe { CStr::from_ptr(value) }.to_bytes();
    if bytes.len() > limit {
        return Err(invalid("native string exceeds bound"));
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| invalid("native string is not UTF-8"))
}

pub(super) struct Bus {
    pub ptr: BusPtr,
    pub api: Rc<Api>,
    retain: bool,
}
impl Bus {
    pub fn new(address: &str) -> io::Result<Self> {
        let api = Api::load()?;
        let mut ptr = ptr::null_mut();
        check(unsafe { (api.new)(&mut ptr) })?;
        let bus = Self {
            ptr,
            api,
            retain: false,
        };
        let address = text(address)?;
        check(unsafe { (bus.api.set_address)(ptr, address.as_ptr()) })?;
        check(unsafe { (bus.api.set_bus_client)(ptr, 1) })?;
        check(unsafe { (bus.api.set_interactive)(ptr, 0) })?;
        check(unsafe { (bus.api.start)(ptr) })?;
        Ok(bus)
    }
    pub fn retain_on_drop(&mut self) {
        self.retain = true;
    }
    pub fn peer_uid(&self) -> io::Result<u32> {
        let mut creds = ptr::null_mut();
        check(unsafe { (self.api.get_owner_creds)(self.ptr, 1 << 4, &mut creds) })?;
        let mut uid = 0;
        let result = check(unsafe { (self.api.creds_uid)(creds, &mut uid) });
        unsafe { (self.api.creds_unref)(creds) };
        result.map(|_| uid)
    }
    pub fn method(
        &self,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
    ) -> io::Result<Message> {
        let (d, p, i, m) = (
            text(destination)?,
            text(path)?,
            text(interface)?,
            text(member)?,
        );
        let mut ptr = ptr::null_mut();
        check(unsafe {
            (self.api.message_new)(
                self.ptr,
                &mut ptr,
                d.as_ptr(),
                p.as_ptr(),
                i.as_ptr(),
                m.as_ptr(),
            )
        })?;
        let message = Message {
            ptr,
            api: self.api.clone(),
        };
        check(unsafe { (self.api.message_auto_start)(ptr, 0) })?;
        check(unsafe { (self.api.message_interactive)(ptr, 0) })?;
        Ok(message)
    }
    pub fn call(&self, message: &Message, usec: u64) -> io::Result<Rpc> {
        let mut inbox = Box::new(ReplyInbox {
            ptr: ptr::null_mut(),
            reference: self.api.message_ref,
            unreference: self.api.message_unref,
        });
        let mut slot = ptr::null_mut();
        check(unsafe {
            (self.api.call_async)(
                self.ptr,
                &mut slot,
                message.ptr,
                reply_callback,
                (&mut *inbox as *mut ReplyInbox).cast(),
                usec.max(1),
            )
        })?;
        Ok(Rpc {
            _slot: Slot {
                ptr: slot,
                api: self.api.clone(),
            },
            inbox,
        })
    }
    pub fn process(&self) -> io::Result<bool> {
        check(unsafe { (self.api.process)(self.ptr, ptr::null_mut()) }).map(|r| r > 0)
    }
    pub fn poll(&self, millis: c_int) -> io::Result<()> {
        let fd = check(unsafe { (self.api.get_fd)(self.ptr) })?;
        let events = check(unsafe { (self.api.get_events)(self.ptr) })?;
        let mut fds = libc::pollfd {
            fd,
            events: events as i16,
            revents: 0,
        };
        let r = unsafe { libc::poll(&mut fds, 1, millis) };
        if r < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        Ok(())
    }
    pub fn job_signals(&self, owner: &str, usec: u64) -> io::Result<JobSignals> {
        check(unsafe { (self.api.set_timeout)(self.ptr, usec.max(1)) })?;
        let owner = text(owner)?;
        let mut inbox = Box::new(SignalInbox {
            messages: [ptr::null_mut(); 64],
            len: 0,
            overflow: false,
            reference: self.api.message_ref,
            unreference: self.api.message_unref,
        });
        let mut ptr = ptr::null_mut();
        check(unsafe {
            (self.api.match_signal)(
                self.ptr,
                &mut ptr,
                owner.as_ptr(),
                c"/org/freedesktop/systemd1".as_ptr(),
                c"org.freedesktop.systemd1.Manager".as_ptr(),
                c"JobRemoved".as_ptr(),
                signal_callback,
                (&mut *inbox as *mut SignalInbox).cast(),
            )
        })?;
        Ok(JobSignals {
            _slot: Slot {
                ptr,
                api: self.api.clone(),
            },
            inbox,
            api: self.api.clone(),
        })
    }
}
impl Drop for Bus {
    fn drop(&mut self) {
        if self.retain {
            std::mem::forget(self.api.clone());
        } else {
            unsafe { (self.api.close_unref)(self.ptr) };
        }
    }
}
struct Slot {
    ptr: SlotPtr,
    api: Rc<Api>,
}
impl Drop for Slot {
    fn drop(&mut self) {
        unsafe { (self.api.slot_unref)(self.ptr) };
    }
}

struct ReplyInbox {
    ptr: MessagePtr,
    reference: unsafe extern "C" fn(MessagePtr) -> MessagePtr,
    unreference: unsafe extern "C" fn(MessagePtr) -> MessagePtr,
}
impl Drop for ReplyInbox {
    fn drop(&mut self) {
        unsafe { (self.unreference)(self.ptr) };
    }
}
unsafe extern "C" fn reply_callback(
    message: MessagePtr,
    data: *mut c_void,
    _: *mut BusError,
) -> c_int {
    // Callback performs no parsing/allocation/panicking and keeps a native ref.
    let inbox = unsafe { &mut *data.cast::<ReplyInbox>() };
    if inbox.ptr.is_null() {
        inbox.ptr = unsafe { (inbox.reference)(message) };
    }
    1
}
pub(super) struct Rpc {
    _slot: Slot,
    inbox: Box<ReplyInbox>,
}
impl Rpc {
    pub fn reply(&self) -> Option<Message> {
        if self.inbox.ptr.is_null() {
            None
        } else {
            Some(Message {
                ptr: unsafe { (self._slot.api.message_ref)(self.inbox.ptr) },
                api: self._slot.api.clone(),
            })
        }
    }
}
struct SignalInbox {
    messages: [MessagePtr; 64],
    len: usize,
    overflow: bool,
    reference: unsafe extern "C" fn(MessagePtr) -> MessagePtr,
    unreference: unsafe extern "C" fn(MessagePtr) -> MessagePtr,
}
impl Drop for SignalInbox {
    fn drop(&mut self) {
        for &m in &self.messages[..self.len] {
            unsafe { (self.unreference)(m) };
        }
    }
}
unsafe extern "C" fn signal_callback(
    message: MessagePtr,
    data: *mut c_void,
    _: *mut BusError,
) -> c_int {
    let inbox = unsafe { &mut *data.cast::<SignalInbox>() };
    if inbox.len < 64 {
        inbox.messages[inbox.len] = unsafe { (inbox.reference)(message) };
        inbox.len += 1;
    } else {
        inbox.overflow = true;
    }
    1
}
pub(super) struct JobSignals {
    _slot: Slot,
    inbox: Box<SignalInbox>,
    api: Rc<Api>,
}
impl JobSignals {
    pub fn take(&mut self) -> io::Result<Vec<Message>> {
        if self.inbox.overflow {
            return Err(invalid("JobRemoved queue exceeded bound"));
        }
        let mut messages = Vec::with_capacity(self.inbox.len);
        for i in 0..self.inbox.len {
            messages.push(Message {
                ptr: self.inbox.messages[i],
                api: self.api.clone(),
            });
            self.inbox.messages[i] = ptr::null_mut();
        }
        self.inbox.len = 0;
        Ok(messages)
    }
}
pub(super) struct Message {
    pub ptr: MessagePtr,
    pub api: Rc<Api>,
}
impl Drop for Message {
    fn drop(&mut self) {
        unsafe { (self.api.message_unref)(self.ptr) };
    }
}
impl Message {
    pub fn append(&mut self, value: &str) -> io::Result<()> {
        let value = text(value)?;
        check(unsafe {
            (self.api.message_append_basic)(self.ptr, b's' as c_char, value.as_ptr().cast())
        })
        .map(|_| ())
    }
    pub fn append_array(&mut self, values: &[&str]) -> io::Result<()> {
        check(unsafe { (self.api.message_open)(self.ptr, b'a' as c_char, c"s".as_ptr()) })?;
        for value in values {
            self.append(value)?;
        }
        check(unsafe { (self.api.message_close)(self.ptr) }).map(|_| ())
    }
    pub fn error(&self) -> io::Result<Option<String>> {
        let ptr = unsafe { (self.api.message_error)(self.ptr) };
        if ptr.is_null() {
            Ok(None)
        } else {
            copied(unsafe { (*ptr).name }, 1024).map(Some)
        }
    }
    pub fn string(&mut self, kind: u8) -> io::Result<String> {
        let mut ptr = ptr::null();
        let n = check(unsafe {
            (self.api.message_read_basic)(
                self.ptr,
                kind as c_char,
                (&mut ptr as *mut *const c_char).cast(),
            )
        })?;
        if n == 0 {
            return Err(invalid("missing string field"));
        }
        copied(ptr, 65536)
    }
    pub fn u32(&mut self, kind: u8) -> io::Result<u32> {
        let mut value = 0u32;
        let n = check(unsafe {
            (self.api.message_read_basic)(self.ptr, kind as c_char, (&mut value as *mut u32).cast())
        })?;
        if n == 0 {
            Err(invalid("missing integer field"))
        } else {
            Ok(value)
        }
    }
    pub fn enter(&mut self, kind: u8, signature: &CStr) -> io::Result<bool> {
        check(unsafe { (self.api.message_enter)(self.ptr, kind as c_char, signature.as_ptr()) })
            .map(|n| n > 0)
    }
    pub fn require_enter(&mut self, kind: u8, signature: &CStr) -> io::Result<()> {
        if self.enter(kind, signature)? {
            Ok(())
        } else {
            Err(invalid("missing native container"))
        }
    }
    pub fn exit(&mut self) -> io::Result<()> {
        check(unsafe { (self.api.message_exit)(self.ptr) }).map(|_| ())
    }
    pub fn strings(&mut self) -> io::Result<Vec<String>> {
        self.require_enter(b'a', c"s")?;
        let mut out = Vec::new();
        let mut bytes = 0;
        loop {
            let mut ptr = ptr::null();
            if check(unsafe {
                (self.api.message_read_basic)(
                    self.ptr,
                    b's' as c_char,
                    (&mut ptr as *mut *const c_char).cast(),
                )
            })? == 0
            {
                break;
            }
            if out.len() == 64 {
                return Err(invalid("native string array exceeds bound"));
            }
            let value = copied(ptr, 65536)?;
            bytes += value.len();
            if bytes > 65536 {
                return Err(invalid("native string array bytes exceed bound"));
            }
            out.push(value);
        }
        self.exit()?;
        Ok(out)
    }
    pub fn bytes(&mut self) -> io::Result<Vec<u8>> {
        self.require_enter(b'a', c"y")?;
        let mut out = Vec::new();
        loop {
            let mut value = 0u8;
            if check(unsafe {
                (self.api.message_read_basic)(
                    self.ptr,
                    b'y' as c_char,
                    (&mut value as *mut u8).cast(),
                )
            })? == 0
            {
                break;
            }
            if out.len() == 16 {
                return Err(invalid("invocation identity exceeds 16 bytes"));
            }
            out.push(value);
        }
        self.exit()?;
        if out.len() != 16 {
            return Err(invalid("invocation identity must be 16 bytes"));
        }
        Ok(out)
    }
    pub fn exec(&mut self) -> io::Result<Option<(String, Vec<String>, bool)>> {
        self.require_enter(b'a', c"(sasbttttuii)")?;
        if !self.enter(b'r', c"sasbttttuii")? {
            self.exit()?;
            return Ok(None);
        }
        let path = self.string(b's')?;
        let args = self.strings()?;
        let ignore = self.u32(b'b')? != 0;
        check(unsafe { (self.api.message_skip)(self.ptr, c"ttttuii".as_ptr()) })?;
        self.exit()?;
        if self.enter(b'r', c"sasbttttuii")? {
            return Err(invalid("multiple ExecStart commands unsupported"));
        }
        self.exit()?;
        Ok(Some((path, args, ignore)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };
    struct PrivateBus {
        child: Child,
        _dir: tempfile::TempDir,
        address: String,
    }
    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
    impl PrivateBus {
        fn new() -> Self {
            let dir = tempfile::Builder::new()
                .prefix("theseus-sdbus-")
                .tempdir_in("/tmp")
                .unwrap();
            let path = dir.path().join("bus.sock");
            let address = format!("unix:path={}", path.display());
            let child = Command::new("/usr/bin/dbus-daemon")
                .env_clear()
                .args([
                    "--session",
                    "--nofork",
                    "--nopidfile",
                    &format!("--address={address}"),
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let fixture = Self {
                child,
                _dir: dir,
                address,
            };
            let deadline = Instant::now() + Duration::from_secs(2);
            while !path.exists() {
                assert!(
                    Instant::now() < deadline,
                    "private dbus-daemon did not bind"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            fixture
        }
    }
    fn wait(bus: &Bus, rpc: &Rpc) -> Message {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(reply) = rpc.reply() {
                return reply;
            }
            assert!(Instant::now() < deadline, "private bus reply deadline");
            bus.process().unwrap();
            bus.poll(10).unwrap();
        }
    }
    #[test]
    fn native_private_bus_auth_and_async_reply_no_manager_connection() {
        // Actual libsystemd + an owned private bus primitive, no systemd manager.
        let fixture = PrivateBus::new();
        let bus = Bus::new(&fixture.address).unwrap();
        assert_eq!(bus.peer_uid().unwrap(), unsafe { libc::geteuid() });
        let request = bus
            .method(
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "ListNames",
            )
            .unwrap();
        let rpc = bus.call(&request, 1_000_000).unwrap();
        let mut reply = wait(&bus, &rpc);
        assert_eq!(reply.error().unwrap(), None);
        let names = reply.strings().unwrap();
        assert!(names.contains(&"org.freedesktop.DBus".into()));
        assert!(!names.contains(&"org.freedesktop.systemd1".into()));
        let mut request = bus
            .method(
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "GetNameOwner",
            )
            .unwrap();
        request.append("org.freedesktop.systemd1").unwrap();
        let rpc = bus.call(&request, 1_000_000).unwrap();
        let reply = wait(&bus, &rpc);
        assert_eq!(
            reply.error().unwrap().as_deref(),
            Some("org.freedesktop.DBus.Error.NameHasNoOwner")
        );
    }
    #[test]
    fn native_private_bus_signal_slot_owns_callback_storage() {
        let fixture = PrivateBus::new();
        let bus = Bus::new(&fixture.address).unwrap();
        let request = bus
            .method(
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "GetId",
            )
            .unwrap();
        let rpc = bus.call(&request, 1_000_000).unwrap();
        let _ = wait(&bus, &rpc);
        let mut signals = bus.job_signals(":1.99999", 1_000_000).unwrap();
        assert!(signals.take().unwrap().is_empty());
        drop(signals);
        drop(rpc);
        drop(bus);
    }
    // Native wire-container fixtures exercise the exact SDK parser, without
    // implying that a model message is an actual systemd observation.
    fn model_message(bus: &Bus) -> Message {
        type NewSignal = unsafe extern "C" fn(
            BusPtr,
            *mut MessagePtr,
            *const c_char,
            *const c_char,
            *const c_char,
        ) -> c_int;
        let symbol = unsafe { libc::dlsym(bus.api.handle, c"sd_bus_message_new_signal".as_ptr()) };
        assert!(!symbol.is_null());
        let new: NewSignal = unsafe { std::mem::transmute(symbol) };
        let mut ptr = ptr::null_mut();
        check(unsafe {
            new(
                bus.ptr,
                &mut ptr,
                c"/theseus/model".as_ptr(),
                c"org.theseus.Model".as_ptr(),
                c"Value".as_ptr(),
            )
        })
        .unwrap();
        Message {
            ptr,
            api: bus.api.clone(),
        }
    }
    fn seal(message: &mut Message) {
        type Seal = unsafe extern "C" fn(MessagePtr, u64, u64) -> c_int;
        type Rewind = unsafe extern "C" fn(MessagePtr, c_int) -> c_int;
        let a = unsafe { libc::dlsym(message.api.handle, c"sd_bus_message_seal".as_ptr()) };
        let b = unsafe { libc::dlsym(message.api.handle, c"sd_bus_message_rewind".as_ptr()) };
        assert!(!a.is_null() && !b.is_null());
        let seal: Seal = unsafe { std::mem::transmute(a) };
        let rewind: Rewind = unsafe { std::mem::transmute(b) };
        check(unsafe { seal(message.ptr, 1, 0) }).unwrap();
        check(unsafe { rewind(message.ptr, 1) }).unwrap();
    }
    fn open(message: &mut Message, kind: u8, signature: &CStr) {
        check(unsafe {
            (message.api.message_open)(message.ptr, kind as c_char, signature.as_ptr())
        })
        .unwrap();
    }
    fn close(message: &mut Message) {
        check(unsafe { (message.api.message_close)(message.ptr) }).unwrap();
    }
    fn basic<T>(message: &mut Message, kind: u8, value: &T) {
        check(unsafe {
            (message.api.message_append_basic)(
                message.ptr,
                kind as c_char,
                (value as *const T).cast(),
            )
        })
        .unwrap();
    }
    fn exec_entry(message: &mut Message, ignore: bool) {
        open(message, b'r', c"sasbttttuii");
        message.append("/fixture/service").unwrap();
        message
            .append_array(&["/fixture/service", "--foreground"])
            .unwrap();
        basic(message, b'b', &i32::from(ignore));
        for _ in 0..4 {
            basic(message, b't', &0u64);
        }
        basic(message, b'u', &0u32);
        basic(message, b'i', &0i32);
        basic(message, b'i', &0i32);
        close(message);
    }
    #[test]
    fn native_model_exec_projection_preserves_argv_and_refuses_multiple_commands() {
        let fixture = PrivateBus::new();
        let bus = Bus::new(&fixture.address).unwrap();
        let mut message = model_message(&bus);
        open(&mut message, b'a', c"(sasbttttuii)");
        exec_entry(&mut message, false);
        close(&mut message);
        seal(&mut message);
        assert_eq!(
            message.exec().unwrap(),
            Some((
                "/fixture/service".into(),
                vec!["/fixture/service".into(), "--foreground".into()],
                false
            ))
        );
        let mut message = model_message(&bus);
        open(&mut message, b'a', c"(sasbttttuii)");
        exec_entry(&mut message, false);
        exec_entry(&mut message, false);
        close(&mut message);
        seal(&mut message);
        assert!(message.exec().is_err());
    }
    #[test]
    fn native_model_invocation_and_array_bounds_are_enforced() {
        let fixture = PrivateBus::new();
        let bus = Bus::new(&fixture.address).unwrap();
        for count in [0, 15, 16, 17] {
            let mut message = model_message(&bus);
            open(&mut message, b'a', c"y");
            for value in 0..count {
                basic(&mut message, b'y', &(value as u8));
            }
            close(&mut message);
            seal(&mut message);
            assert_eq!(message.bytes().is_ok(), count == 16);
        }
        let mut message = model_message(&bus);
        message.append_array(&["entry"; 65]).unwrap();
        seal(&mut message);
        assert!(message.strings().is_err());
    }
    fn model_manager() -> (PrivateBus, super::super::SystemdManager) {
        use super::super::*;
        let fixture = PrivateBus::new();
        let root_path = fixture._dir.path().join("units");
        std::fs::create_dir(&root_path).unwrap();
        let authority = SystemdAuthority {
            endpoint: UnixPath::new(
                fixture
                    ._dir
                    .path()
                    .join("bus.sock")
                    .to_str()
                    .unwrap()
                    .into(),
            )
            .unwrap(),
            scope: SystemdScope::User,
            bus_uid: unsafe { libc::geteuid() },
            manager_uid: unsafe { libc::geteuid() },
            manager_unique_name: ":1.99999".into(),
            manager_version: "262-model-only".into(),
            unit: "theseus-model.service".into(),
            definition_root: root_path.clone(),
            unit_load_paths: vec![root_path.to_str().unwrap().into()],
            ownership_id: "explicit-model-context".into(),
            context_receipt: "synthetic-model-receipt".into(),
            allowed_start: true,
            allowed_stop: true,
        };
        let root =
            crate::fs::HostDir::open(&root_path, crate::fs::RootPolicy::AllComponentsNoFollow)
                .unwrap();
        let bus = Bus::new(&fixture.address).unwrap();
        let manager = SystemdManager {
            bus,
            authority,
            root,
            instance: [7; 32],
            revoked: false,
            signals: None,
            pending: None,
            retained_errors: Vec::new(),
        };
        (fixture, manager)
    }
    fn model_definition() -> super::super::SystemdDefinition {
        super::super::SystemdDefinition {
            executable: "/fixture/service".into(),
            arguments: vec!["/fixture/service".into()],
            environment: vec![],
            working_directory: "/fixture".into(),
        }
    }
    fn model_loaded(manager: &super::super::SystemdManager) -> super::super::LoadedService {
        use super::super::*;
        LoadedService {
            unit: manager.authority.unit.clone(),
            names: vec![manager.authority.unit.clone()],
            load_state: "loaded".into(),
            active_state: "active".into(),
            sub_state: "running".into(),
            fragment_path: manager
                .authority
                .definition_root
                .join(&manager.authority.unit)
                .to_str()
                .unwrap()
                .into(),
            drop_in_paths: vec![],
            invocation_id: vec![7; 16],
            need_daemon_reload: false,
            service_type: "simple".into(),
            restart: "no".into(),
            definition: Some(model_definition()),
            main_pid: 1,
            result: "success".into(),
        }
    }
    #[test]
    fn model_wrong_manager_owner_is_sticky_before_any_manager_call() {
        use super::super::*;
        use std::sync::atomic::AtomicBool;
        let (_fixture, mut manager) = model_manager();
        let path = manager
            .authority
            .definition_root
            .join(&manager.authority.unit);
        std::fs::write(&path, b"definition").unwrap();
        let file = std::fs::File::open(path).unwrap();
        let fingerprint = crate::fs::fingerprint(&file).unwrap();
        let mut owner = RegistrationOwner {
            _file: file,
            fingerprint,
            definition_bytes: b"definition".to_vec(),
            definition: model_definition(),
            manager_instance: [8; 32],
            install_receipt: "original-sut-action".into(),
            revoked: false,
        };
        assert!(
            manager
                .verify_owner(
                    &mut owner,
                    Instant::now() + Duration::from_secs(1),
                    &AtomicBool::new(false)
                )
                .is_err()
        );
        assert!(owner.is_revoked());
        assert!(manager.is_revoked());
    }
    #[test]
    fn model_registration_configuration_runtime_are_independent() {
        let (_fixture, manager) = model_manager();
        let mut loaded = model_loaded(&manager);
        assert!(
            manager
                .check_definition(&loaded, &model_definition())
                .is_ok()
        );
        loaded.active_state = "inactive".into();
        loaded.sub_state = "dead".into();
        loaded.main_pid = 0;
        assert!(
            manager
                .check_definition(&loaded, &model_definition())
                .is_ok()
        );
        loaded.need_daemon_reload = true;
        assert!(
            manager
                .check_definition(&loaded, &model_definition())
                .is_err()
        );
        loaded.need_daemon_reload = false;
        loaded
            .definition
            .as_mut()
            .unwrap()
            .arguments
            .push("--wrong".into());
        assert!(
            manager
                .check_definition(&loaded, &model_definition())
                .is_err()
        );
        loaded.definition = Some(model_definition());
        loaded.drop_in_paths.push("/foreign/drop-in.conf".into());
        assert!(manager.check_origin(&loaded).is_err());
        loaded.drop_in_paths.clear();
        loaded.names.push("other.service".into());
        assert!(manager.check_origin(&loaded).is_err());
    }
    #[test]
    fn model_unknown_dispatch_is_not_replayed_and_retains_native_owners_on_drop() {
        use super::super::*;
        use std::{os::fd::AsRawFd, sync::atomic::AtomicBool};
        let (_fixture, mut manager) = model_manager();
        let path = manager
            .authority
            .definition_root
            .join(&manager.authority.unit);
        std::fs::write(&path, b"original-definition").unwrap();
        let file = std::fs::File::open(path).unwrap();
        let file_fd = file.as_raw_fd();
        let bus_fd = unsafe { (manager.bus.api.get_fd)(manager.bus.ptr) };
        assert!(bus_fd >= 0);
        let root = crate::fs::HostDir::open(
            &manager.authority.definition_root,
            crate::fs::RootPolicy::AllComponentsNoFollow,
        )
        .unwrap();
        let progress = OwnedActionProgress::Unknown {
            job: None,
            reason: "model lost original send receipt".into(),
        };
        manager.pending = Some(PendingAction {
            rpc: None,
            _definition_file: file,
            _definition_root: root,
            job: None,
            events: Vec::new(),
            progress: progress.clone(),
        });
        assert_eq!(
            manager
                .poll_action(
                    Instant::now() + Duration::from_secs(1),
                    &AtomicBool::new(false)
                )
                .unwrap(),
            progress
        );
        assert!(manager.take_terminal_action().is_err());
        drop(manager);
        // Actual native FDs remain retained; the model never calls a manager.
        assert!(unsafe { libc::fcntl(file_fd, libc::F_GETFD) } >= 0);
        assert!(unsafe { libc::fcntl(bus_fd, libc::F_GETFD) } >= 0);
    }
    #[test]
    fn native_model_empty_exec_is_unavailable_configuration() {
        let fixture = PrivateBus::new();
        let bus = Bus::new(&fixture.address).unwrap();
        let mut message = model_message(&bus);
        open(&mut message, b'a', c"(sasbttttuii)");
        close(&mut message);
        seal(&mut message);
        assert!(
            message.exec().is_ok(),
            "removed cached unit must retain observation with no command"
        );
    }
    fn model_job_signal(
        manager: &mut super::super::SystemdManager,
        path: &str,
        unit: &str,
        result: &str,
    ) {
        let mut message = model_message(&manager.bus);
        basic(&mut message, b'u', &7u32);
        let path = CString::new(path).unwrap();
        check(unsafe {
            (message.api.message_append_basic)(message.ptr, b'o' as c_char, path.as_ptr().cast())
        })
        .unwrap();
        message.append(unit).unwrap();
        message.append(result).unwrap();
        seal(&mut message);
        let inbox = &mut *manager.signals.as_mut().unwrap().inbox;
        unsafe {
            signal_callback(
                message.ptr,
                (inbox as *mut SignalInbox).cast(),
                ptr::null_mut(),
            )
        };
    }
    #[test]
    fn native_model_job_completion_matches_original_job_and_exact_unit() {
        use super::super::*;
        let (_fixture, mut manager) = model_manager();
        let request = manager
            .bus
            .method(
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "GetId",
            )
            .unwrap();
        let rpc = manager.bus.call(&request, 1_000_000).unwrap();
        let _reply = wait(&manager.bus, &rpc);
        manager.signals = Some(manager.bus.job_signals(":1.99999", 1_000_000).unwrap());
        let path = manager
            .authority
            .definition_root
            .join(&manager.authority.unit);
        std::fs::write(&path, b"definition").unwrap();
        let file = std::fs::File::open(path).unwrap();
        let root = crate::fs::HostDir::open(
            &manager.authority.definition_root,
            crate::fs::RootPolicy::AllComponentsNoFollow,
        )
        .unwrap();
        manager.pending = Some(PendingAction {
            rpc: Some(rpc),
            _definition_file: file,
            _definition_root: root,
            job: Some("/org/freedesktop/systemd1/job/7".into()),
            events: Vec::new(),
            progress: OwnedActionProgress::Pending { job: None },
        });
        model_job_signal(
            &mut manager,
            "/org/freedesktop/systemd1/job/8",
            "theseus-model.service",
            "done",
        );
        assert_eq!(manager.advance_action().unwrap(), None);
        model_job_signal(
            &mut manager,
            "/org/freedesktop/systemd1/job/7",
            "other.service",
            "done",
        );
        assert_eq!(manager.advance_action().unwrap(), None);
        model_job_signal(
            &mut manager,
            "/org/freedesktop/systemd1/job/7",
            "theseus-model.service",
            "failed",
        );
        assert_eq!(
            manager.advance_action().unwrap(),
            Some(OwnedActionProgress::Terminal {
                job: "/org/freedesktop/systemd1/job/7".into(),
                result: "failed".into()
            })
        );
        assert!(
            matches!(manager.take_terminal_action().unwrap(),OwnedActionProgress::Terminal{result,..} if result=="failed")
        );
        assert_eq!(manager.pending_action(), None);
    }
    #[test]
    fn model_wrong_bus_peer_and_replaced_root_permanently_revoke_controls() {
        use super::super::*;
        use std::sync::atomic::AtomicBool;
        let cancel = AtomicBool::new(false);
        let (_fixture, mut manager) = model_manager();
        let original_uid = manager.authority.bus_uid;
        manager.authority.bus_uid = original_uid.wrapping_add(1);
        assert!(
            manager
                .verify_context(Instant::now() + Duration::from_secs(1), &cancel)
                .is_err()
        );
        assert!(manager.is_revoked());
        manager.authority.bus_uid = original_uid;
        assert!(
            manager
                .qualify_registration(
                    &model_definition(),
                    "sut-install-receipt".into(),
                    Instant::now() + Duration::from_secs(1),
                    &cancel
                )
                .is_err()
        );
        let (_fixture, mut manager) = model_manager();
        let root = manager.authority.definition_root.clone();
        let retained_path = root.with_file_name("original-units");
        std::fs::rename(&root, &retained_path).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert!(
            manager
                .verify_context(Instant::now() + Duration::from_secs(1), &cancel)
                .is_err()
        );
        assert!(manager.is_revoked());
        std::fs::remove_dir(&root).unwrap();
        std::fs::rename(retained_path, root).unwrap();
        assert!(
            manager
                .qualify_registration(
                    &model_definition(),
                    "sut-install-receipt".into(),
                    Instant::now() + Duration::from_secs(1),
                    &cancel
                )
                .is_err()
        );
    }
}
