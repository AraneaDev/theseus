//! Lazy fixed-ABI GIO bridge, on the original dedicated broker worker only.
//! Signatures: GLib/GIO 2.80+ public C headers. No build-time native linkage.
pub(super) mod images;
use super::{
    LiteralCommand, NativeCallbacks, NativeLaunchReceipt, NativePreparationFailure, invalid,
};
use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    io, ptr,
    rc::Rc,
};
type Object = *mut c_void;
type PidCallback = unsafe extern "C" fn(Object, libc::pid_t, *mut c_void);
type ChildSetup = unsafe extern "C" fn(*mut c_void);
type AsyncCallback = unsafe extern "C" fn(Object, Object, *mut c_void);
#[repr(C)]
struct GError {
    domain: u32,
    code: c_int,
    message: *mut c_char,
}

macro_rules! api {
    ($($field:ident: $ty:ty = $name:literal),* $(,)?) => {
        struct Api { _handle: *mut c_void, _images: Option<Rc<images::PinnedImages>>, $( $field: $ty, )* }
        impl Api {
            fn load(version: [u32; 3], images: Option<Rc<images::PinnedImages>>) -> io::Result<Rc<Self>> {
                let retain=images.clone();
                let result=(|| {
                // The qualified stock path loads exact retained images. Legacy unsafe
                // callers keep their independently discharged fixed-SDK contract.
                let handle = match &images { Some(original)=>original.load()?, None=>unsafe { libc::dlopen(c"libgio-2.0.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) } };
                let resolve=|name:&CStr| -> io::Result<*mut c_void> {match &images {Some(original)=>original.symbol(handle,name),None=>Ok(unsafe{libc::dlsym(handle,name.as_ptr())})}};
                if handle.is_null() { return Err(loader_failure("load fixed GIO SDK")); }
                // GIO/GLib register process-global types and may retain worker
                // threads. Once loaded the SDK mapping stays until process exit,
                // including symbol/version failure; never unload live type code.
                let mut actual = [0; 3];
                for (index, name) in ["glib_major_version", "glib_minor_version", "glib_micro_version"].into_iter().enumerate() {
                    let name = CString::new(name).expect("static SDK symbol");
                    let pointer = resolve(&name)?;
                    if pointer.is_null() { return Err(loader_failure("resolve GLib version symbol")); }
                    actual[index] = unsafe { *pointer.cast::<u32>() };
                }
                if actual != version { return Err(io::Error::new(io::ErrorKind::Unsupported, "GIO SDK differs from exact supplied GLib version")); }
                let sdk_api=Rc::new(Self { _handle: handle, _images: images.clone(), $( $field: {
                    let pointer = resolve(CStr::from_bytes_with_nul(concat!($name, "\0").as_bytes()).expect("static SDK symbol"))?;
                    if pointer.is_null() { return Err(loader_failure(concat!("resolve SDK symbol ", $name))); }
                    unsafe { std::mem::transmute::<*mut c_void, $ty>(pointer) }
                }, )* });
                if let Some(original)=&images {original.revalidate()?;}
                Ok(sdk_api)
                })();
                if result.is_err() { if let Some(original)=retain {std::mem::forget(original);} }
                result
            }
        }
    }
}
/// Immediate copied native loader diagnostic, before another loader call can
/// invalidate its thread-local storage. Raw bytes remain available even when
/// the diagnostic is not UTF-8.
#[derive(Debug)]
pub struct DynamicLoaderFailure {
    operation: &'static str,
    diagnostic: Vec<u8>,
}
impl DynamicLoaderFailure {
    pub fn operation(&self) -> &'static str {
        self.operation
    }
}
impl std::fmt::Display for DynamicLoaderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {}",
            self.operation,
            String::from_utf8_lossy(&self.diagnostic)
        )
    }
}
impl std::error::Error for DynamicLoaderFailure {}
fn loader_failure(operation: &'static str) -> io::Error {
    let pointer = unsafe { libc::dlerror() };
    let diagnostic = if pointer.is_null() {
        Vec::new()
    } else {
        unsafe { CStr::from_ptr(pointer) }.to_bytes().to_vec()
    };
    io::Error::new(
        io::ErrorKind::Unsupported,
        DynamicLoaderFailure {
            operation,
            diagnostic,
        },
    )
}
api! {
    key_new: unsafe extern "C" fn()->Object = "g_key_file_new",
    key_load: unsafe extern "C" fn(Object,*const c_char,usize,c_int,*mut *mut GError)->c_int = "g_key_file_load_from_data",
    key_unref: unsafe extern "C" fn(Object) = "g_key_file_unref",
    desktop_new: unsafe extern "C" fn(Object)->Object = "g_desktop_app_info_new_from_keyfile",
    get_executable: unsafe extern "C" fn(Object)->*const c_char = "g_app_info_get_executable",
    get_commandline: unsafe extern "C" fn(Object)->*const c_char = "g_app_info_get_commandline",
    get_boolean: unsafe extern "C" fn(Object,*const c_char)->c_int = "g_desktop_app_info_get_boolean",
    context_new: unsafe extern "C" fn()->Object = "g_app_launch_context_new",
    context_get_env: unsafe extern "C" fn(Object)->*mut *mut c_char = "g_app_launch_context_get_environment",
    context_set_env: unsafe extern "C" fn(Object,*const c_char,*const c_char) = "g_app_launch_context_setenv",
    context_unset_env: unsafe extern "C" fn(Object,*const c_char) = "g_app_launch_context_unsetenv",
    strfreev: unsafe extern "C" fn(*mut *mut c_char) = "g_strfreev",
    object_unref: unsafe extern "C" fn(Object) = "g_object_unref",
    main_new: unsafe extern "C" fn()->Object = "g_main_context_new",
    main_unref: unsafe extern "C" fn(Object) = "g_main_context_unref",
    main_push: unsafe extern "C" fn(Object) = "g_main_context_push_thread_default",
    main_pop: unsafe extern "C" fn(Object) = "g_main_context_pop_thread_default",
    launch: unsafe extern "C" fn(Object,Object,Object,c_int,Option<ChildSetup>,*mut c_void,Option<PidCallback>,*mut c_void,*mut *mut GError)->c_int = "g_desktop_app_info_launch_uris_as_manager",
    main_iteration: unsafe extern "C" fn(Object,c_int)->c_int = "g_main_context_iteration",
    main_pending: unsafe extern "C" fn(Object)->c_int = "g_main_context_pending",
    bus_get: unsafe extern "C" fn(c_int,Object,*mut *mut GError)->Object = "g_bus_get_sync",
    bus_closed: unsafe extern "C" fn(Object)->c_int = "g_dbus_connection_is_closed",
    bus_set_exit_on_close: unsafe extern "C" fn(Object,c_int) = "g_dbus_connection_set_exit_on_close",
    bus_get_exit_on_close: unsafe extern "C" fn(Object)->c_int = "g_dbus_connection_get_exit_on_close",
    bus_flush: unsafe extern "C" fn(Object,Object,Option<AsyncCallback>,*mut c_void) = "g_dbus_connection_flush",
    bus_flush_finish: unsafe extern "C" fn(Object,Object,*mut *mut GError)->c_int = "g_dbus_connection_flush_finish",
    bus_close: unsafe extern "C" fn(Object,Object,Option<AsyncCallback>,*mut c_void) = "g_dbus_connection_close",
    bus_close_finish: unsafe extern "C" fn(Object,Object,*mut *mut GError)->c_int = "g_dbus_connection_close_finish",
    object_ref: unsafe extern "C" fn(Object)->Object = "g_object_ref",
    error_free: unsafe extern "C" fn(*mut GError) = "g_error_free",
}

/// Exact original GError allocation and SDK mapping. This intentionally stays
/// non-Send with the native worker and is not flattened into a diagnostic string.
pub struct NativeErrorOwner {
    pointer: *mut GError,
    api: Rc<Api>,
}
impl NativeErrorOwner {
    pub fn domain(&self) -> u32 {
        unsafe { (*self.pointer).domain }
    }
    pub fn code(&self) -> i32 {
        unsafe { (*self.pointer).code }
    }
    pub fn message_bytes(&self) -> &[u8] {
        let message = unsafe { (*self.pointer).message };
        if message.is_null() {
            &[]
        } else {
            unsafe { CStr::from_ptr(message) }.to_bytes()
        }
    }
}
impl std::fmt::Debug for NativeErrorOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeErrorOwner")
            .field("domain", &self.domain())
            .field("code", &self.code())
            .field(
                "message",
                &String::from_utf8_lossy(
                    &self.message_bytes()[..self.message_bytes().len().min(4096)],
                ),
            )
            .finish()
    }
}
impl Drop for NativeErrorOwner {
    fn drop(&mut self) {
        unsafe { (self.api.error_free)(self.pointer) };
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum RetirementProgress {
    #[default]
    Idle,
    Flushing,
    Flushed,
    Closing,
    Closed,
    Retired,
    Unknown,
}
impl RetirementProgress {
    pub(super) fn flush_complete(&mut self, success: bool) {
        *self = if *self == Self::Flushing && success {
            Self::Flushed
        } else {
            Self::Unknown
        };
    }
    pub(super) fn close_complete(&mut self, success: bool) {
        *self = if *self == Self::Closing && success {
            Self::Closed
        } else {
            Self::Unknown
        };
    }
}
struct RetirementState {
    api: Rc<Api>,
    connection: Object,
    progress: RetirementProgress,
    errors: Vec<NativeErrorOwner>,
    unexpected: Vec<Object>,
    truncated: bool,
}
impl RetirementState {
    fn retain_error(&mut self, pointer: *mut GError) {
        if pointer.is_null() {
            return;
        }
        let error = NativeErrorOwner {
            pointer,
            api: self.api.clone(),
        };
        if self.errors.len() < 4 {
            self.errors.push(error);
        } else {
            self.truncated = true;
            std::mem::forget(error);
        }
    }
    fn unexpected(&mut self, result: Object) {
        self.progress = RetirementProgress::Unknown;
        if !result.is_null() {
            let held = unsafe { (self.api.object_ref)(result) };
            if self.unexpected.len() < 4 {
                self.unexpected.push(held);
            } else {
                self.truncated = true;
            }
        }
    }
}
unsafe extern "C" fn flush_callback(source: Object, result: Object, data: *mut c_void) {
    let Some(state) = (unsafe { data.cast::<RetirementState>().as_mut() }) else {
        return;
    };
    if state.progress != RetirementProgress::Flushing
        || source != state.connection
        || result.is_null()
    {
        state.unexpected(result);
        return;
    }
    let mut error = ptr::null_mut();
    let success = unsafe { (state.api.bus_flush_finish)(source, result, &mut error) } != 0;
    state.retain_error(error);
    state.progress.flush_complete(success && error.is_null());
}
unsafe extern "C" fn close_callback(source: Object, result: Object, data: *mut c_void) {
    let Some(state) = (unsafe { data.cast::<RetirementState>().as_mut() }) else {
        return;
    };
    if state.progress != RetirementProgress::Closing
        || source != state.connection
        || result.is_null()
    {
        state.unexpected(result);
        return;
    }
    let mut error = ptr::null_mut();
    let success = unsafe { (state.api.bus_close_finish)(source, result, &mut error) } != 0;
    state.retain_error(error);
    state.progress.close_complete(success && error.is_null());
}

pub(super) struct Objects {
    api: Rc<Api>,
    key: Object,
    app: Object,
    context: Object,
    main: Object,
    connection: Object,
    retirement: Option<Box<RetirementState>>,
    invoked: bool,
    retired: bool,
    callbacks: Box<NativeCallbacks>,
    pub error: Option<NativeErrorOwner>,
}
impl Objects {
    pub fn prepare(
        bytes: &[u8],
        expected: &LiteralCommand,
        environment: &[(String, String)],
        version: [u32; 3],
        images: Option<Rc<images::PinnedImages>>,
    ) -> Result<Self, NativePreparationFailure> {
        let api = Api::load(version, images).map_err(failure)?;
        let retirement = Box::new(RetirementState {
            api: api.clone(),
            connection: ptr::null_mut(),
            progress: RetirementProgress::Idle,
            errors: Vec::with_capacity(4),
            unexpected: Vec::with_capacity(4),
            truncated: false,
        });
        let mut owner = Self {
            api,
            key: ptr::null_mut(),
            app: ptr::null_mut(),
            context: ptr::null_mut(),
            main: ptr::null_mut(),
            connection: ptr::null_mut(),
            retirement: Some(retirement),
            invoked: false,
            retired: false,
            callbacks: Box::default(),
            error: None,
        };
        let setup = (|| -> Result<(), NativePreparationFailure> {
            owner.main = unsafe { (owner.api.main_new)() };
            owner.key = unsafe { (owner.api.key_new)() };
            if owner.main.is_null() || owner.key.is_null() {
                return Err(failure(io::Error::other(
                    "native context/keyfile allocation failed",
                )));
            }
            let mut error = ptr::null_mut();
            let loaded = unsafe {
                (owner.api.key_load)(owner.key, bytes.as_ptr().cast(), bytes.len(), 0, &mut error)
            };
            if !error.is_null() {
                owner.error = Some(NativeErrorOwner {
                    pointer: error,
                    api: owner.api.clone(),
                });
            }
            if loaded == 0 || owner.error.is_some() {
                return Err(NativePreparationFailure {
                    error: invalid("native desktop keyfile parser refused supplied bytes"),
                    native_error: owner.error.take(),
                    retained_broker: None,
                    retained_entry: None,
                    retained_objects: None,
                });
            }
            owner.app = unsafe { (owner.api.desktop_new)(owner.key) };
            if owner.app.is_null() {
                return Err(failure(invalid(
                    "native GIO DesktopAppInfo refused exact entry",
                )));
            }
            let commandline = std::iter::once(expected.executable.as_str())
                .chain(expected.arguments.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            let executable = unsafe { (owner.api.get_executable)(owner.app) };
            let native_command = unsafe { (owner.api.get_commandline)(owner.app) };
            if !native_equals(executable, &expected.executable)
                || !native_equals(native_command, &commandline)
                || unsafe { (owner.api.get_boolean)(owner.app, c"Terminal".as_ptr()) } != 0
                || unsafe { (owner.api.get_boolean)(owner.app, c"DBusActivatable".as_ptr()) } != 0
            {
                return Err(failure(invalid(
                    "native GIO projection differs from admitted literal command/policy",
                )));
            }
            owner.context = unsafe { (owner.api.context_new)() };
            if owner.context.is_null() {
                return Err(failure(io::Error::other(
                    "native launch context allocation failed",
                )));
            }
            // Explicitly erase the new context's inherited snapshot, then supply
            // only approved material. Never mutate the process environment.
            for entry in owner.environment()? {
                let (key, _) = entry
                    .split_once('=')
                    .ok_or_else(|| failure(invalid("invalid native environment snapshot")))?;
                let key = CString::new(key)
                    .map_err(|_| failure(invalid("invalid native environment key")))?;
                unsafe { (owner.api.context_unset_env)(owner.context, key.as_ptr()) };
            }
            for (key, value) in environment {
                let key = CString::new(key.as_str())
                    .map_err(|_| failure(invalid("invalid supplied environment key")))?;
                let value = CString::new(value.as_str())
                    .map_err(|_| failure(invalid("invalid supplied environment value")))?;
                unsafe { (owner.api.context_set_env)(owner.context, key.as_ptr(), value.as_ptr()) };
            }
            let mut actual = owner.environment()?;
            let mut supplied = environment
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>();
            actual.sort();
            supplied.sort();
            if actual != supplied {
                return Err(failure(invalid(
                    "native launch context differs from supplied replacement environment",
                )));
            }
            // This exact shared connection is acquired once under the exclusive
            // pre-GIO process contract. The manager's internal cache lookup returns
            // this same held object; retirement never reacquires a connection.
            let mut error = ptr::null_mut();
            unsafe { (owner.api.main_push)(owner.main) };
            owner.connection = unsafe { (owner.api.bus_get)(2, ptr::null_mut(), &mut error) };
            unsafe { (owner.api.main_pop)(owner.main) };
            owner
                .retirement
                .as_mut()
                .expect("original retirement owner")
                .connection = owner.connection;
            if !error.is_null() {
                owner.error = Some(NativeErrorOwner {
                    pointer: error,
                    api: owner.api.clone(),
                });
            }
            if owner.connection.is_null()
                || owner.error.is_some()
                || unsafe { (owner.api.bus_closed)(owner.connection) } != 0
            {
                return Err(NativePreparationFailure {
                    error: invalid("original supplied GIO session connection unavailable"),
                    native_error: owner.error.take(),
                    retained_broker: None,
                    retained_entry: None,
                    retained_objects: None,
                });
            }
            // This connection has no unrelated SDK users under the admission
            // contract. Unexpected bus loss must remain an observed error, not
            // implicitly terminate the broker and erase original child authority.
            unsafe { (owner.api.bus_set_exit_on_close)(owner.connection, 0) };
            if unsafe { (owner.api.bus_get_exit_on_close)(owner.connection) } != 0 {
                return Err(failure(invalid(
                    "original GIO bus exit-on-close policy unproved",
                )));
            }
            Ok(())
        })();
        match setup {
            Ok(()) => Ok(owner),
            Err(mut failure) => {
                failure.retained_objects = Some(Box::new(owner));
                Err(failure)
            }
        }
    }
    fn environment(&self) -> Result<Vec<String>, NativePreparationFailure> {
        let entries = unsafe { (self.api.context_get_env)(self.context) };
        if entries.is_null() {
            return Err(failure(invalid(
                "native launch context environment unavailable",
            )));
        }
        let result = (|| {
            let mut copied = Vec::new();
            let mut total = 0;
            for index in 0..=64 {
                let entry = unsafe { *entries.add(index) };
                if entry.is_null() {
                    return Ok(copied);
                }
                let bytes = unsafe { CStr::from_ptr(entry) }.to_bytes();
                total += bytes.len();
                if index == 64 || bytes.len() > 8192 || total > 16_384 {
                    return Err(failure(invalid("native launch environment exceeds bound")));
                }
                copied.push(
                    std::str::from_utf8(bytes)
                        .map_err(|_| failure(invalid("native launch environment is not UTF-8")))?
                        .to_owned(),
                );
            }
            Err(failure(invalid("unterminated native environment")))
        })();
        unsafe { (self.api.strfreev)(entries) };
        result
    }
    pub fn callback_pids(&self) -> &[i32] {
        &self.callbacks.pids[..self.callbacks.stored]
    }
    pub fn callback_overflow(&self) -> bool {
        self.callbacks.overflow
    }
    pub fn retirement_errors(&self) -> &[NativeErrorOwner] {
        self.retirement
            .as_ref()
            .map(|state| state.errors.as_slice())
            .unwrap_or(&[])
    }
    pub fn retirement_errors_truncated(&self) -> bool {
        self.retirement
            .as_ref()
            .is_some_and(|state| state.truncated)
    }
    pub fn retirement_unknown(&self) -> bool {
        self.retirement.as_ref().is_some_and(|state| {
            state.progress == RetirementProgress::Unknown
                || state.truncated
                || !state.unexpected.is_empty()
        })
    }
    pub fn poll_retirement(&mut self) -> io::Result<bool> {
        if self.retirement_unknown() {
            return Err(invalid(
                "original native retirement unknown; owners retained",
            ));
        }
        if self.retired {
            return Ok(true);
        }
        let phase = self
            .retirement
            .as_ref()
            .expect("original retirement owner")
            .progress;
        if phase == RetirementProgress::Unknown {
            return Err(invalid(
                "original native retirement unknown; storage retained",
            ));
        }
        if matches!(
            phase,
            RetirementProgress::Idle | RetirementProgress::Flushed
        ) {
            let state = self.retirement.as_mut().expect("original retirement owner");
            let pointer = (&mut **state as *mut RetirementState).cast();
            unsafe { (self.api.main_push)(self.main) };
            if phase == RetirementProgress::Idle {
                state.progress = RetirementProgress::Flushing;
                unsafe {
                    (self.api.bus_flush)(
                        self.connection,
                        ptr::null_mut(),
                        Some(flush_callback),
                        pointer,
                    )
                };
            } else {
                state.progress = RetirementProgress::Closing;
                unsafe {
                    (self.api.bus_close)(
                        self.connection,
                        ptr::null_mut(),
                        Some(close_callback),
                        pointer,
                    )
                };
            }
            unsafe { (self.api.main_pop)(self.main) };
        }
        // One existing broker-loop step; never block or create an executor.
        for _ in 0..8 {
            if unsafe { (self.api.main_pending)(self.main) } == 0 {
                break;
            }
            unsafe { (self.api.main_iteration)(self.main, 0) };
        }
        let state = self.retirement.as_mut().expect("original retirement owner");
        if state.progress == RetirementProgress::Unknown {
            return Err(invalid(
                "original native retirement failed; owners retained",
            ));
        }
        if state.progress != RetirementProgress::Closed
            || unsafe { (self.api.main_pending)(self.main) } != 0
        {
            return Ok(false);
        }
        state.progress = RetirementProgress::Retired;
        self.release_objects();
        self.retired = true;
        Ok(true)
    }
    pub fn launch(&mut self, owned_primary: bool) -> NativeLaunchReceipt {
        self.invoked = true;
        // A private thread-default context, never the caller's default display
        // or event loop. The owner remains retained after synchronous return.
        unsafe { (self.api.main_push)(self.main) };
        let mut error = ptr::null_mut();
        let acknowledged = unsafe {
            (self.api.launch)(
                self.app,
                ptr::null_mut(),
                self.context,
                // Qualified primary branch reserves the actual GSpawn child.
                // No PATH/inherited descriptors/output claims. Legacy ack-only
                // keeps automatic reaping and cannot mint child authority.
                spawn_flags(owned_primary),
                None,
                ptr::null_mut(),
                Some(pid_callback),
                (&mut *self.callbacks as *mut NativeCallbacks).cast(),
                &mut error,
            )
        };
        unsafe { (self.api.main_pop)(self.main) };
        if !error.is_null() {
            self.error = Some(NativeErrorOwner {
                pointer: error,
                api: self.api.clone(),
            });
        }
        self.callbacks
            .receipt(acknowledged != 0 && self.error.is_none())
    }
}
impl Objects {
    fn release_objects(&mut self) {
        unsafe {
            if !self.connection.is_null() {
                (self.api.object_unref)(self.connection);
                self.connection = ptr::null_mut();
            }
            if !self.context.is_null() {
                (self.api.object_unref)(self.context);
                self.context = ptr::null_mut();
            }
            if !self.app.is_null() {
                (self.api.object_unref)(self.app);
                self.app = ptr::null_mut();
            }
            if !self.key.is_null() {
                (self.api.key_unref)(self.key);
                self.key = ptr::null_mut();
            }
            if !self.main.is_null() {
                (self.api.main_unref)(self.main);
                self.main = ptr::null_mut();
            }
        }
    }
}
impl Drop for Objects {
    fn drop(&mut self) {
        if self.invoked && !self.retired {
            // A late native callback may still use the stable boxed state.
            std::mem::forget(self.retirement.take());
            return;
        }
        self.release_objects();
    }
}
unsafe extern "C" fn pid_callback(_app: Object, pid: libc::pid_t, data: *mut c_void) {
    // Scope-call callback, no allocation, panic, PID adoption or side effects.
    if let Some(callbacks) = unsafe { data.cast::<NativeCallbacks>().as_mut() } {
        callbacks.record(pid);
    }
}
pub(super) fn spawn_flags(owned_primary: bool) -> c_int {
    8 | 16 | if owned_primary { 2 } else { 0 }
}
fn native_equals(pointer: *const c_char, expected: &str) -> bool {
    !pointer.is_null() && unsafe { CStr::from_ptr(pointer) }.to_bytes() == expected.as_bytes()
}
fn failure(error: io::Error) -> NativePreparationFailure {
    NativePreparationFailure {
        error,
        native_error: None,
        retained_broker: None,
        retained_entry: None,
        retained_objects: None,
    }
}
