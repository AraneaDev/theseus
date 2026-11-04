//! Fixture-only installer, never linked into the shipping authority decoder.
//! All native effects require a supplied whole-private-manager context.
#![cfg(target_os = "linux")]
use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    fs::File,
    io::{self, Read, Write},
    ptr,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use theseus_host::{
    fs::{HostDir, RootPolicy},
    services::systemd::{SystemdAuthority, SystemdManager},
};

#[derive(Clone, Debug)]
pub struct SutAuthority {
    pub manager: SystemdAuthority,
    pub whole_private_context_receipt: String,
}
#[derive(Clone, Debug)]
pub struct SutDefinition {
    pub id: String,
    pub bytes: Vec<u8>,
}
struct DefinitionFiles {
    root: HostDir,
    unit: String,
    definitions: Vec<SutDefinition>,
}
impl DefinitionFiles {
    fn open(authority: &SutAuthority, definitions: Vec<SutDefinition>) -> io::Result<Self> {
        authority.manager.validate()?;
        if authority.whole_private_context_receipt.is_empty()
            || authority.whole_private_context_receipt.len() > 4096
            || definitions.is_empty()
            || definitions.len() > 6
        {
            return Err(invalid(
                "missing whole-private-manager authority or definition allowlist",
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        for definition in &definitions {
            if definition.id.is_empty()
                || definition.id.len() > 64
                || !definition
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
                || !ids.insert(&definition.id)
                || definition.bytes.is_empty()
                || definition.bytes.len() > 65536
                || definition.bytes.contains(&0)
                || std::str::from_utf8(&definition.bytes).is_err()
            {
                return Err(invalid("invalid fixture definition allowlist"));
            }
        }
        Ok(Self {
            root: HostDir::open(
                &authority.manager.definition_root,
                RootPolicy::AllComponentsNoFollow,
            )?,
            unit: authority.manager.unit.clone(),
            definitions,
        })
    }
    fn current(&self) -> io::Result<Option<File>> {
        let mut file = match self
            .root
            .open_leaf_classified(std::ffi::OsStr::new(&self.unit))
        {
            theseus_host::fs::LeafOpenResult::Opened(file) => file,
            theseus_host::fs::LeafOpenResult::MissingSettled => return Ok(None),
            theseus_host::fs::LeafOpenResult::FailedSettled(error) => return Err(error),
            theseus_host::fs::LeafOpenResult::Unsettled(owner) => {
                return Err(io::Error::other(owner));
            }
        };
        let before = theseus_host::fs::fingerprint(&file)?;
        if before.len > 65536 {
            return Err(invalid("foreign or oversized fixture unit"));
        }
        let mut bytes = Vec::new();
        (&mut file).take(65537).read_to_end(&mut bytes)?;
        if bytes.len() > 65536
            || theseus_host::fs::fingerprint(&file)? != before
            || !self.definitions.iter().any(|d| d.bytes == bytes)
        {
            return Err(invalid(
                "unit is not an original predeclared fixture definition",
            ));
        }
        Ok(Some(file))
    }
    fn write(&self, id: &str) -> io::Result<File> {
        let definition = self
            .definitions
            .iter()
            .find(|d| d.id == id)
            .ok_or_else(|| invalid("definition ID outside predeclared fixture catalog"))?;
        self.current()?;
        self.root
            .publish(std::ffi::OsStr::new(&self.unit), &mut |file| {
                file.write_all(&definition.bytes)
            })?;
        self.current()?
            .ok_or_else(|| invalid("published fixture definition disappeared"))
    }
    fn remove(&self, file: &File) -> io::Result<()> {
        self.root
            .remove_created_leaf(std::ffi::OsStr::new(&self.unit), file)
    }
}
/// Root's pinned catalog wrapper constructs this on its own SUT process worker.
/// Helper APIs never expose these installer-only manager methods.
pub struct NativeServiceSut {
    authority: SutAuthority,
    files: std::mem::ManuallyDrop<DefinitionFiles>,
    guard: SystemdManager,
    connection: Option<Connection>,
    retained_definition: Option<File>,
    uncertain: bool,
}
impl NativeServiceSut {
    pub fn connect(
        authority: SutAuthority,
        definitions: Vec<SutDefinition>,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        remaining(deadline, cancel)?;
        let files = DefinitionFiles::open(&authority, definitions)?;
        let guard = SystemdManager::connect(authority.manager.clone(), deadline, cancel)?;
        let connection = Connection::connect(&authority.manager, deadline, cancel)?;
        Ok(Self {
            authority,
            files: std::mem::ManuallyDrop::new(files),
            guard,
            connection: Some(connection),
            retained_definition: None,
            uncertain: false,
        })
    }
    pub fn install(
        &mut self,
        definition_id: &str,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if self.uncertain {
            return Err(invalid("original fixture mutation unresolved; no replay"));
        }
        self.guard.inspect(deadline, cancel)?;
        self.uncertain = true;
        self.retained_definition = Some(self.files.write(definition_id)?);
        self.reload(deadline, cancel)?;
        self.guard.inspect(deadline, cancel)?;
        self.connection
            .as_mut()
            .expect("original connection")
            .manager_call(
                "LoadUnit",
                &[&self.authority.manager.unit],
                deadline,
                cancel,
            )?
            .string(b'o')?;
        self.guard.inspect(deadline, cancel)?;
        self.uncertain = false;
        self.retained_definition.take();
        Ok(())
    }
    pub fn configure(
        &mut self,
        definition_id: &str,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.files
            .current()?
            .ok_or_else(|| invalid("configuration requires original fixture registration"))?;
        self.install(definition_id, deadline, cancel)
    }
    pub fn uninstall(&mut self, deadline: Instant, cancel: &AtomicBool) -> io::Result<()> {
        if self.uncertain {
            return Err(invalid("original fixture mutation unresolved; no replay"));
        }
        self.guard.inspect(deadline, cancel)?;
        self.retained_definition = Some(
            self.files
                .current()?
                .ok_or_else(|| invalid("original fixture definition absent before uninstall"))?,
        );
        self.uncertain = true;
        let connection = self.connection.as_mut().expect("original connection");
        connection.discard_events()?;
        let job = connection
            .manager_call(
                "StopUnit",
                &[&self.authority.manager.unit, "fail"],
                deadline,
                cancel,
            )?
            .string(b'o')?;
        connection.wait_job(&job, &self.authority.manager.unit, deadline, cancel)?;
        self.guard.inspect(deadline, cancel)?;
        self.files.remove(
            self.retained_definition
                .as_ref()
                .expect("retained original definition"),
        )?;
        self.reload(deadline, cancel)?;
        self.guard.inspect(deadline, cancel)?;
        self.uncertain = false;
        self.retained_definition.take();
        Ok(())
    }
    fn reload(&mut self, deadline: Instant, cancel: &AtomicBool) -> io::Result<()> {
        self.guard.inspect(deadline, cancel)?;
        let connection = self.connection.as_mut().expect("original connection");
        connection.discard_events()?;
        connection.manager_call("Reload", &[], deadline, cancel)?;
        let mut started = false;
        loop {
            for event in connection.events()? {
                match event {
                    Event::Reloading(true) => started = true,
                    Event::Reloading(false) if started => return Ok(()),
                    _ => {}
                }
            }
            connection.pump(deadline, cancel)?;
        }
    }
}
impl Drop for NativeServiceSut {
    fn drop(&mut self) {
        if self.uncertain {
            if let Some(connection) = self.connection.take() {
                std::mem::forget(connection);
            }
            if let Some(file) = self.retained_definition.take() {
                std::mem::forget(file);
            }
        } else {
            // All fixture mutations and their original native calls settled.
            unsafe { std::mem::ManuallyDrop::drop(&mut self.files) };
        }
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn remaining(deadline: Instant, cancel: &AtomicBool) -> io::Result<u64> {
    if cancel.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "fixture native call cancelled",
        ));
    }
    let left = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "fixture manager deadline"))?;
    if left > Duration::from_secs(30) {
        return Err(invalid("fixture deadline exceeds 30 seconds"));
    }
    Ok(left.as_micros().max(1) as u64)
}

// Private fixture SDK. Fixed soname/symbols, no manager-library linker dependency.
type NativePtr = *mut c_void;
type Handler = unsafe extern "C" fn(NativePtr, *mut c_void, *mut BusError) -> c_int;
#[repr(C)]
struct BusError {
    name: *const c_char,
    message: *const c_char,
    free: c_int,
}
#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}
#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(name: *const c_char, flags: c_int) -> NativePtr;
    fn dlsym(handle: NativePtr, name: *const c_char) -> NativePtr;
    fn dlclose(handle: NativePtr) -> c_int;
    fn poll(fds: *mut PollFd, count: usize, millis: c_int) -> c_int;
}
macro_rules! sdk {
    ($($field:ident:$ty:ty=$name:literal),*$(,)?)=>{
        struct Api{handle:NativePtr,$($field:$ty,)*}
        impl Api{fn load()->io::Result<Rc<Self>>{
            let handle=unsafe{dlopen(c"libsystemd.so.0".as_ptr(),2)};if handle.is_null(){return Err(invalid("fixture libsystemd unavailable"));}
            let loaded=(||{Ok(Self{handle,$($field:{let address=unsafe{dlsym(handle,concat!($name,"\0").as_ptr().cast())};if address.is_null(){return Err(invalid(concat!("missing fixture symbol ",$name)));}unsafe{std::mem::transmute::<NativePtr,$ty>(address)}},)*})})();
            match loaded{Ok(api)=>Ok(Rc::new(api)),Err(error)=>{unsafe{dlclose(handle)};Err(error)}}
        }}
        impl Drop for Api{fn drop(&mut self){unsafe{dlclose(self.handle)};}}
    }
}
sdk! {
    new:unsafe extern "C" fn(*mut NativePtr)->c_int="sd_bus_new",
    set_address:unsafe extern "C" fn(NativePtr,*const c_char)->c_int="sd_bus_set_address",
    set_client:unsafe extern "C" fn(NativePtr,c_int)->c_int="sd_bus_set_bus_client",
    set_interactive:unsafe extern "C" fn(NativePtr,c_int)->c_int="sd_bus_set_allow_interactive_authorization",
    set_timeout:unsafe extern "C" fn(NativePtr,u64)->c_int="sd_bus_set_method_call_timeout",
    start:unsafe extern "C" fn(NativePtr)->c_int="sd_bus_start",
    close:unsafe extern "C" fn(NativePtr)->NativePtr="sd_bus_close_unref",
    process:unsafe extern "C" fn(NativePtr,*mut NativePtr)->c_int="sd_bus_process",
    fd:unsafe extern "C" fn(NativePtr)->c_int="sd_bus_get_fd",
    events:unsafe extern "C" fn(NativePtr)->c_int="sd_bus_get_events",
    owner_creds:unsafe extern "C" fn(NativePtr,u64,*mut NativePtr)->c_int="sd_bus_get_owner_creds",
    creds_uid:unsafe extern "C" fn(NativePtr,*mut u32)->c_int="sd_bus_creds_get_euid",
    creds_unref:unsafe extern "C" fn(NativePtr)->NativePtr="sd_bus_creds_unref",
    method:unsafe extern "C" fn(NativePtr,*mut NativePtr,*const c_char,*const c_char,*const c_char,*const c_char)->c_int="sd_bus_message_new_method_call",
    autostart:unsafe extern "C" fn(NativePtr,c_int)->c_int="sd_bus_message_set_auto_start",
    append:unsafe extern "C" fn(NativePtr,c_char,*const c_void)->c_int="sd_bus_message_append_basic",
    read:unsafe extern "C" fn(NativePtr,c_char,*mut c_void)->c_int="sd_bus_message_read_basic",
    error:unsafe extern "C" fn(NativePtr)->*const BusError="sd_bus_message_get_error",
    reference:unsafe extern "C" fn(NativePtr)->NativePtr="sd_bus_message_ref",
    unreference:unsafe extern "C" fn(NativePtr)->NativePtr="sd_bus_message_unref",
    call:unsafe extern "C" fn(NativePtr,*mut NativePtr,NativePtr,Handler,*mut c_void,u64)->c_int="sd_bus_call_async",
    signal:unsafe extern "C" fn(NativePtr,*mut NativePtr,*const c_char,*const c_char,*const c_char,*const c_char,Handler,*mut c_void)->c_int="sd_bus_match_signal",
    slot_unref:unsafe extern "C" fn(NativePtr)->NativePtr="sd_bus_slot_unref",
}
fn check(code: c_int) -> io::Result<c_int> {
    if code < 0 {
        Err(io::Error::from_raw_os_error(-code))
    } else {
        Ok(code)
    }
}
fn cstring(text: &str) -> io::Result<CString> {
    CString::new(text).map_err(|_| invalid("NUL fixture bus value"))
}
struct Message {
    ptr: NativePtr,
    api: Rc<Api>,
}
impl Drop for Message {
    fn drop(&mut self) {
        unsafe { (self.api.unreference)(self.ptr) };
    }
}
impl Message {
    fn string(&mut self, kind: u8) -> io::Result<String> {
        let mut value: *const c_char = ptr::null();
        if check(unsafe {
            (self.api.read)(
                self.ptr,
                kind as c_char,
                (&mut value as *mut *const c_char).cast(),
            )
        })? == 0
            || value.is_null()
        {
            return Err(invalid("missing native fixture string"));
        }
        let bytes = unsafe { CStr::from_ptr(value) }.to_bytes();
        if bytes.len() > 4096 {
            return Err(invalid("fixture native string too long"));
        }
        String::from_utf8(bytes.to_vec()).map_err(|_| invalid("fixture native string not UTF8"))
    }
    fn number(&mut self, kind: u8) -> io::Result<u32> {
        let mut value = 0;
        if check(unsafe {
            (self.api.read)(self.ptr, kind as c_char, (&mut value as *mut u32).cast())
        })? == 0
        {
            return Err(invalid("missing fixture native number"));
        }
        Ok(value)
    }
    fn success(&self) -> io::Result<()> {
        let error = unsafe { (self.api.error)(self.ptr) };
        if error.is_null() {
            Ok(())
        } else {
            let name = unsafe { CStr::from_ptr((*error).name) };
            if name.to_bytes().len() > 4096 {
                return Err(invalid("fixture error name exceeds bound"));
            }
            Err(io::Error::other(name.to_string_lossy().to_string()))
        }
    }
}
struct Slot {
    ptr: NativePtr,
    api: Rc<Api>,
}
impl Drop for Slot {
    fn drop(&mut self) {
        unsafe { (self.api.slot_unref)(self.ptr) };
    }
}
struct Inbox {
    messages: [NativePtr; 64],
    length: usize,
    overflow: bool,
    reference: unsafe extern "C" fn(NativePtr) -> NativePtr,
    unreference: unsafe extern "C" fn(NativePtr) -> NativePtr,
}
impl Inbox {
    fn new(api: &Api) -> Self {
        Self {
            messages: [ptr::null_mut(); 64],
            length: 0,
            overflow: false,
            reference: api.reference,
            unreference: api.unreference,
        }
    }
}
impl Drop for Inbox {
    fn drop(&mut self) {
        for &message in &self.messages[..self.length] {
            unsafe { (self.unreference)(message) };
        }
    }
}
unsafe extern "C" fn callback(message: NativePtr, data: *mut c_void, _: *mut BusError) -> c_int {
    let inbox = unsafe { &mut *data.cast::<Inbox>() };
    if inbox.length < 64 {
        inbox.messages[inbox.length] = unsafe { (inbox.reference)(message) };
        inbox.length += 1;
    } else {
        inbox.overflow = true;
    }
    1
}
struct Pending {
    _slot: Slot,
    inbox: Box<Inbox>,
}
struct Signals {
    _slot: Slot,
    inbox: Box<Inbox>,
    reloading: bool,
}
enum Event {
    Reloading(bool),
    Job {
        path: String,
        unit: String,
        result: String,
    },
}
struct Connection {
    api: Rc<Api>,
    bus: NativePtr,
    owner: String,
    signals: Vec<Signals>,
    pending: Option<Pending>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        unsafe { (self.api.close)(self.bus) };
    }
}
impl Connection {
    fn connect(
        authority: &SystemdAuthority,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        let api = Api::load()?;
        let mut bus = ptr::null_mut();
        check(unsafe { (api.new)(&mut bus) })?;
        let mut connection = Self {
            api,
            bus,
            owner: authority.manager_unique_name.clone(),
            signals: Vec::new(),
            pending: None,
        };
        let address = cstring(&format!("unix:path={}", authority.endpoint.as_str()))?;
        check(unsafe { (connection.api.set_address)(bus, address.as_ptr()) })?;
        check(unsafe { (connection.api.set_client)(bus, 1) })?;
        check(unsafe { (connection.api.set_interactive)(bus, 0) })?;
        check(unsafe { (connection.api.start)(bus) })?;
        let mut creds = ptr::null_mut();
        check(unsafe { (connection.api.owner_creds)(bus, 1 << 4, &mut creds) })?;
        let mut uid = 0;
        let result = check(unsafe { (connection.api.creds_uid)(creds, &mut uid) });
        unsafe { (connection.api.creds_unref)(creds) };
        result?;
        if uid != authority.bus_uid {
            return Err(invalid("wrong fixture bus peer"));
        }
        let actual = connection
            .call(
                (
                    "org.freedesktop.DBus",
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    "GetNameOwner",
                ),
                &["org.freedesktop.systemd1"],
                deadline,
                cancel,
            )?
            .string(b's')?;
        if actual != authority.manager_unique_name {
            return Err(invalid("fixture manager owner changed"));
        }
        let uid = connection
            .call(
                (
                    "org.freedesktop.DBus",
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    "GetConnectionUnixUser",
                ),
                &[&authority.manager_unique_name],
                deadline,
                cancel,
            )?
            .number(b'u')?;
        if uid != authority.manager_uid {
            return Err(invalid("wrong fixture manager principal"));
        }
        connection.add_signal("Reloading", true, deadline, cancel)?;
        connection.add_signal("JobRemoved", false, deadline, cancel)?;
        connection.manager_call("Subscribe", &[], deadline, cancel)?;
        Ok(connection)
    }
    fn add_signal(
        &mut self,
        member: &str,
        reloading: bool,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let owner = cstring(&self.owner)?;
        let member = cstring(member)?;
        let mut inbox = Box::new(Inbox::new(&self.api));
        let mut slot = ptr::null_mut();
        check(unsafe { (self.api.set_timeout)(self.bus, remaining(deadline, cancel)?) })?;
        check(unsafe {
            (self.api.signal)(
                self.bus,
                &mut slot,
                owner.as_ptr(),
                c"/org/freedesktop/systemd1".as_ptr(),
                c"org.freedesktop.systemd1.Manager".as_ptr(),
                member.as_ptr(),
                callback,
                (&mut *inbox as *mut Inbox).cast(),
            )
        })?;
        self.signals.push(Signals {
            _slot: Slot {
                ptr: slot,
                api: self.api.clone(),
            },
            inbox,
            reloading,
        });
        Ok(())
    }
    fn manager_call(
        &mut self,
        method: &str,
        args: &[&str],
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Message> {
        let owner = self.owner.clone();
        self.call(
            (
                &owner,
                "/org/freedesktop/systemd1",
                "org.freedesktop.systemd1.Manager",
                method,
            ),
            args,
            deadline,
            cancel,
        )
    }
    fn call(
        &mut self,
        target: (&str, &str, &str, &str),
        args: &[&str],
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Message> {
        if self.pending.is_some() {
            return Err(invalid("fixture original native call retained"));
        }
        let (dest, path, interface, member) = target;
        let (dest, path, interface, member) = (
            cstring(dest)?,
            cstring(path)?,
            cstring(interface)?,
            cstring(member)?,
        );
        let mut message = ptr::null_mut();
        check(unsafe {
            (self.api.method)(
                self.bus,
                &mut message,
                dest.as_ptr(),
                path.as_ptr(),
                interface.as_ptr(),
                member.as_ptr(),
            )
        })?;
        let message = Message {
            ptr: message,
            api: self.api.clone(),
        };
        check(unsafe { (self.api.autostart)(message.ptr, 0) })?;
        for arg in args {
            let arg = cstring(arg)?;
            check(unsafe { (self.api.append)(message.ptr, b's' as c_char, arg.as_ptr().cast()) })?;
        }
        let mut inbox = Box::new(Inbox::new(&self.api));
        let mut slot = ptr::null_mut();
        let result = check(unsafe {
            (self.api.call)(
                self.bus,
                &mut slot,
                message.ptr,
                callback,
                (&mut *inbox as *mut Inbox).cast(),
                remaining(deadline, cancel)?,
            )
        });
        self.pending = Some(Pending {
            _slot: Slot {
                ptr: slot,
                api: self.api.clone(),
            },
            inbox,
        });
        result?;
        loop {
            remaining(deadline, cancel)?;
            let pending = self.pending.as_ref().expect("original native call");
            if pending.inbox.overflow {
                return Err(invalid("fixture reply overflow"));
            }
            if pending.inbox.length > 0 {
                let reply = Message {
                    ptr: unsafe { (self.api.reference)(pending.inbox.messages[0]) },
                    api: self.api.clone(),
                };
                reply.success()?;
                self.pending.take();
                return Ok(reply);
            }
            self.pump(deadline, cancel)?;
        }
    }
    fn pump(&mut self, deadline: Instant, cancel: &AtomicBool) -> io::Result<()> {
        remaining(deadline, cancel)?;
        for _ in 0..64 {
            if check(unsafe { (self.api.process)(self.bus, ptr::null_mut()) })? == 0 {
                break;
            }
        }
        let fd = check(unsafe { (self.api.fd)(self.bus) })?;
        let events = check(unsafe { (self.api.events)(self.bus) })?;
        let mut pollfd = PollFd {
            fd,
            events: events as i16,
            revents: 0,
        };
        if unsafe { poll(&mut pollfd, 1, 25) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        Ok(())
    }
    fn events(&mut self) -> io::Result<Vec<Event>> {
        let mut events = Vec::new();
        for signal in &mut self.signals {
            if signal.inbox.overflow {
                return Err(invalid("fixture signal overflow"));
            }
            for index in 0..signal.inbox.length {
                let mut message = Message {
                    ptr: signal.inbox.messages[index],
                    api: self.api.clone(),
                };
                signal.inbox.messages[index] = ptr::null_mut();
                if signal.reloading {
                    events.push(Event::Reloading(message.number(b'b')? != 0));
                } else {
                    let _id = message.number(b'u')?;
                    let path = message.string(b'o')?;
                    let unit = message.string(b's')?;
                    let result = message.string(b's')?;
                    events.push(Event::Job { path, unit, result });
                }
            }
            signal.inbox.length = 0;
        }
        Ok(events)
    }
    fn discard_events(&mut self) -> io::Result<()> {
        self.events().map(|_| ())
    }
    fn wait_job(
        &mut self,
        expected_job: &str,
        expected_unit: &str,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        loop {
            for event in self.events()? {
                if let Event::Job { path, unit, result } = event
                    && path == expected_job
                    && unit == expected_unit
                {
                    return if result == "done" {
                        Ok(())
                    } else {
                        Err(io::Error::other(format!(
                            "original fixture stop job: {result}"
                        )))
                    };
                }
            }
            self.pump(deadline, cancel)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use theseus_host::services::systemd::{SystemdScope, UnixPath};
    fn authority(root: &Path) -> SutAuthority {
        SutAuthority {
            manager: SystemdAuthority {
                endpoint: UnixPath::new("/tmp/explicit-fixture/bus.sock".into()).unwrap(),
                scope: SystemdScope::User,
                bus_uid: 1000,
                manager_uid: 1000,
                manager_unique_name: ":1.42".into(),
                manager_version: "262-model-only".into(),
                unit: "theseus-fixture.service".into(),
                definition_root: root.to_owned(),
                unit_load_paths: vec![root.to_str().unwrap().into()],
                ownership_id: "explicit-model-owner".into(),
                context_receipt: "explicit-model-context".into(),
                allowed_start: true,
                allowed_stop: true,
            },
            whole_private_context_receipt: "explicit-model-whole-private-context".into(),
        }
    }
    fn definitions() -> Vec<SutDefinition> {
        vec![
            SutDefinition {
                id: "base".into(),
                bytes: b"[Service]\nType=simple\nExecStart=/fixture/base\n".to_vec(),
            },
            SutDefinition {
                id: "configured".into(),
                bytes: b"[Service]\nType=simple\nExecStart=/fixture/base --configured\n".to_vec(),
            },
        ]
    }
    #[test]
    fn fixture_file_primitives_publish_only_admitted_definition_and_remove_original() {
        let root = tempfile::tempdir().unwrap();
        let files = DefinitionFiles::open(&authority(root.path()), definitions()).unwrap();
        let original = files.write("base").unwrap();
        assert_eq!(
            std::fs::read(root.path().join("theseus-fixture.service")).unwrap(),
            definitions()[0].bytes
        );
        files.write("configured").unwrap();
        assert!(files.remove(&original).is_err());
        assert_eq!(
            std::fs::read(root.path().join("theseus-fixture.service")).unwrap(),
            definitions()[1].bytes
        );
        let current = files.current().unwrap().unwrap();
        files.remove(&current).unwrap();
        assert!(files.current().unwrap().is_none());
    }
    #[test]
    fn fixture_file_primitives_refuse_foreign_preexisting_definition_without_repair() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("theseus-fixture.service");
        std::fs::write(&path, b"foreign-definition").unwrap();
        let files = DefinitionFiles::open(&authority(root.path()), definitions()).unwrap();
        assert!(files.current().is_err());
        assert!(files.write("base").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"foreign-definition");
    }
    #[test]
    fn model_missing_whole_private_authority_and_undeclared_definition_refuse_offline() {
        let root = tempfile::tempdir().unwrap();
        let mut bad = authority(root.path());
        bad.whole_private_context_receipt.clear();
        assert!(DefinitionFiles::open(&bad, definitions()).is_err());
        let files = DefinitionFiles::open(&authority(root.path()), definitions()).unwrap();
        assert!(files.write("../../other.service").is_err());
        assert!(!root.path().join("theseus-fixture.service").exists());
        let mut duplicate = definitions();
        duplicate.push(duplicate[0].clone());
        assert!(DefinitionFiles::open(&authority(root.path()), duplicate).is_err());
    }
    #[test]
    fn fixture_sdk_symbol_set_loads_without_connecting_any_manager() {
        let api = Api::load().unwrap();
        let mut bus = ptr::null_mut();
        check(unsafe { (api.new)(&mut bus) }).unwrap();
        assert!(!bus.is_null());
        unsafe { (api.close)(bus) };
    }
}
