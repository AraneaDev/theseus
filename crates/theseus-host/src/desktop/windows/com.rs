//! Original STA only. No Resolve, search, URL, elevation or COM automation.
use super::{RetainedShortcut, ShortcutMetadata, admit_run_level, invalid};
use std::{ffi::c_void, io, marker::PhantomData, mem::size_of, ptr, rc::Rc};
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeCode {
    HResult(i32),
    Win32(u32),
}
#[derive(Debug)]
pub struct NativeError {
    pub api: &'static str,
    pub code: NativeCode,
}
impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {:?}", self.api, self.code)
    }
}
impl std::error::Error for NativeError {}
pub(super) fn win32(api: &'static str) -> io::Error {
    let code = io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
    io::Error::other(NativeError {
        api,
        code: NativeCode::Win32(code),
    })
}
fn hr(api: &'static str, value: i32) -> io::Result<()> {
    if value < 0 {
        Err(io::Error::other(NativeError {
            api,
            code: NativeCode::HResult(value),
        }))
    } else {
        Ok(())
    }
}
#[repr(C)]
struct Guid {
    a: u32,
    b: u16,
    c: u16,
    d: [u8; 8],
}
const CLSID_SHELL_LINK: Guid = Guid {
    a: 0x00021401,
    b: 0,
    c: 0,
    d: [0xc0, 0, 0, 0, 0, 0, 0, 0x46],
};
const IID_SHELL_LINK: Guid = Guid {
    a: 0x000214f9,
    b: 0,
    c: 0,
    d: [0xc0, 0, 0, 0, 0, 0, 0, 0x46],
};
const IID_PERSIST_FILE: Guid = Guid {
    a: 0x0000010b,
    b: 0,
    c: 0,
    d: [0xc0, 0, 0, 0, 0, 0, 0, 0x46],
};
#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, flags: u32) -> i32;
    fn CoUninitialize();
    fn CoCreateInstance(
        class: *const Guid,
        outer: *mut c_void,
        context: u32,
        iid: *const Guid,
        result: *mut *mut c_void,
    ) -> i32;
}
#[repr(C)]
struct UnknownTable {
    query: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}
#[repr(C)]
struct LinkTable {
    unknown: UnknownTable,
    get_path: unsafe extern "system" fn(*mut c_void, *mut u16, i32, *mut c_void, u32) -> i32,
    get_id: usize,
    set_id: usize,
    get_description: usize,
    set_description: usize,
    get_cwd: unsafe extern "system" fn(*mut c_void, *mut u16, i32) -> i32,
    set_cwd: unsafe extern "system" fn(*mut c_void, *const u16) -> i32,
    get_arguments: unsafe extern "system" fn(*mut c_void, *mut u16, i32) -> i32,
    set_arguments: unsafe extern "system" fn(*mut c_void, *const u16) -> i32,
    get_hotkey: usize,
    set_hotkey: usize,
    get_show: usize,
    set_show: unsafe extern "system" fn(*mut c_void, i32) -> i32,
    get_icon: usize,
    set_icon: usize,
    set_relative: usize,
    resolve: usize,
    set_path: unsafe extern "system" fn(*mut c_void, *const u16) -> i32,
}
#[repr(C)]
struct PersistTable {
    unknown: UnknownTable,
    get_class: usize,
    is_dirty: usize,
    load: unsafe extern "system" fn(*mut c_void, *const u16, u32) -> i32,
    save: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32,
    save_completed: usize,
    get_current: usize,
}
struct Object(*mut c_void);
impl Object {
    fn table<T>(&self) -> &T {
        unsafe { &**self.0.cast::<*const T>() }
    }
}
impl Drop for Object {
    fn drop(&mut self) {
        unsafe {
            (self.table::<UnknownTable>().release)(self.0);
        }
    }
}
struct ApartmentInner {
    thread: std::thread::ThreadId,
}
impl Drop for ApartmentInner {
    fn drop(&mut self) {
        debug_assert_eq!(self.thread, std::thread::current().id());
        unsafe {
            CoUninitialize();
        }
    }
}
#[derive(Clone)]
pub struct ComApartment {
    inner: Rc<ApartmentInner>,
}
#[repr(C)]
#[derive(Default)]
struct Message {
    window: HANDLE,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    x: i32,
    y: i32,
    private: u32,
}
#[link(name = "user32")]
unsafe extern "system" {
    fn PeekMessageW(message: *mut Message, window: HANDLE, min: u32, max: u32, remove: u32) -> i32;
    fn TranslateMessage(message: *const Message) -> i32;
    fn DispatchMessageW(message: *const Message) -> isize;
    fn PostQuitMessage(code: i32);
}
impl ComApartment {
    /// One bounded STA turn on the original thread. No new apartment, wait,
    /// executor or alternate session. Native callbacks remain synchronous.
    pub fn pump_once(&self) -> io::Result<()> {
        self.check()?;
        super::probe::pump_turn(|| {
            let mut message = Message::default();
            if unsafe { PeekMessageW(&mut message, ptr::null_mut(), 0, 0, 1) } == 0 {
                return super::probe::MessageTurn::Idle;
            }
            if message.message == 0x12 {
                // Preserve the calling thread's quit request and exit code.
                unsafe { PostQuitMessage(message.wparam as i32) };
                return super::probe::MessageTurn::Quit;
            }
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            super::probe::MessageTurn::Dispatched
        })
    }
    pub fn initialize_original_thread() -> io::Result<Self> {
        hr("CoInitializeEx(STA)", unsafe {
            CoInitializeEx(ptr::null_mut(), 6)
        })?;
        Ok(Self {
            inner: Rc::new(ApartmentInner {
                thread: std::thread::current().id(),
            }),
        })
    }
    pub(super) fn check(&self) -> io::Result<()> {
        if self.inner.thread != std::thread::current().id() {
            return Err(invalid("original COM apartment thread changed"));
        }
        Ok(())
    }
}
pub struct AdmittedShortcut {
    pub(super) retained: RetainedShortcut,
    _link: Object,
    pub(super) apartment: ComApartment,
    _thread: PhantomData<Rc<()>>,
}
fn wide(v: &str) -> Vec<u16> {
    v.encode_utf16().chain(Some(0)).collect()
}
fn output(buffer: &[u16]) -> io::Result<String> {
    let len = buffer
        .iter()
        .position(|c| *c == 0)
        .ok_or_else(|| invalid("unbounded native shortcut string"))?;
    if len == buffer.len() - 1 {
        return Err(invalid("possibly truncated shortcut string"));
    }
    String::from_utf16(&buffer[..len]).map_err(|_| invalid("non Unicode native shortcut string"))
}
const SLGP_RAWPATH: u32 = 4;
fn create_link() -> io::Result<(Object, Object)> {
    let mut raw = ptr::null_mut();
    hr("CoCreateInstance(IShellLinkW)", unsafe {
        CoCreateInstance(
            &CLSID_SHELL_LINK,
            ptr::null_mut(),
            1,
            &IID_SHELL_LINK,
            &mut raw,
        )
    })?;
    if raw.is_null() {
        return Err(invalid("COM returned no shell link"));
    }
    let link = Object(raw);
    let mut persist = ptr::null_mut();
    hr("IShellLinkW::QueryInterface(IPersistFile)", unsafe {
        (link.table::<LinkTable>().unknown.query)(link.0, &IID_PERSIST_FILE, &mut persist)
    })?;
    if persist.is_null() {
        return Err(invalid("COM returned no IPersistFile"));
    }
    Ok((link, Object(persist)))
}
/// Create an in-process IShellLinkW and load (never resolve) one pathname.
fn load_link(entry: &str) -> io::Result<Object> {
    let (link, persist) = create_link()?;
    let path = wide(entry);
    hr("IPersistFile::Load", unsafe {
        (persist.table::<PersistTable>().load)(persist.0, path.as_ptr(), 0)
    })?;
    Ok(link)
}
/// Returns the success HRESULT too: S_FALSE (1) means no path was produced.
fn get_path(link: &Object, flags: u32) -> io::Result<(i32, String)> {
    let mut target = [0u16; 2048];
    let result = unsafe {
        (link.table::<LinkTable>().get_path)(
            link.0,
            target.as_mut_ptr(),
            target.len() as i32,
            ptr::null_mut(),
            flags,
        )
    };
    hr("IShellLinkW::GetPath", result)?;
    Ok((result, output(&target)?))
}
fn get_arguments(link: &Object) -> io::Result<String> {
    let mut arguments = [0u16; 16385];
    hr("IShellLinkW::GetArguments", unsafe {
        (link.table::<LinkTable>().get_arguments)(
            link.0,
            arguments.as_mut_ptr(),
            arguments.len() as i32,
        )
    })?;
    output(&arguments)
}
fn get_working_directory(link: &Object) -> io::Result<String> {
    let mut cwd = [0u16; 2048];
    hr("IShellLinkW::GetWorkingDirectory", unsafe {
        (link.table::<LinkTable>().get_cwd)(link.0, cwd.as_mut_ptr(), cwd.len() as i32)
    })?;
    output(&cwd)
}
/// Test-only installer fixture: author a shortcut exactly as real installers
/// do, through IShellLinkW setters and IPersistFile::Save on the original STA.
/// It never resolves or activates the shortcut.
#[cfg(test)]
pub(super) fn author_shortcut_for_test(
    apartment: &ComApartment,
    entry: &str,
    target: &str,
    arguments: &str,
    working_directory: &str,
) -> io::Result<()> {
    apartment.check()?;
    let (link, persist) = create_link()?;
    let table = link.table::<LinkTable>();
    let (target, arguments, cwd, entry) = (
        wide(target),
        wide(arguments),
        wide(working_directory),
        wide(entry),
    );
    hr("IShellLinkW::SetPath", unsafe {
        (table.set_path)(link.0, target.as_ptr())
    })?;
    hr("IShellLinkW::SetArguments", unsafe {
        (table.set_arguments)(link.0, arguments.as_ptr())
    })?;
    hr("IShellLinkW::SetWorkingDirectory", unsafe {
        (table.set_cwd)(link.0, cwd.as_ptr())
    })?;
    // SW_SHOWNORMAL, the only show command the closed profile admits.
    hr("IShellLinkW::SetShowCmd", unsafe {
        (table.set_show)(link.0, 1)
    })?;
    hr("IPersistFile::Save", unsafe {
        (persist.table::<PersistTable>().save)(persist.0, entry.as_ptr(), 1)
    })
}
/// Test-only bounded diagnostic of what native Shell COM reports for one
/// shortcut. It never admits anything and never resolves or activates.
#[cfg(test)]
pub(super) fn diagnose_native_metadata(apartment: &ComApartment, entry: &str) -> String {
    fn bounded<T: std::fmt::Debug>(value: io::Result<T>) -> String {
        let text = format!("{value:?}");
        text.chars().take(600).collect()
    }
    if let Err(error) = apartment.check() {
        return format!("apartment: {error}");
    }
    let link = match load_link(entry) {
        Ok(link) => link,
        Err(error) => return format!("load: {error}"),
    };
    format!(
        "GetPath(RAWPATH)={} GetPath(0)={} GetArguments={} GetWorkingDirectory={}",
        bounded(get_path(&link, SLGP_RAWPATH)),
        bounded(get_path(&link, 0)),
        bounded(get_arguments(&link)),
        bounded(get_working_directory(&link)),
    )
}
pub struct ShortcutRetireFailure {
    pub error: io::Error,
    pub owner: AdmittedShortcut,
}
impl AdmittedShortcut {
    /// Consume only this original read-only parser proof on its original thread.
    /// The caller must establish its original no-effect/retired process barrier.
    pub fn retire_original(self) -> Result<ComApartment, Box<ShortcutRetireFailure>> {
        if let Err(error) = self.apartment.check() {
            return Err(Box::new(ShortcutRetireFailure { error, owner: self }));
        }
        let Self {
            retained,
            _link,
            apartment,
            _thread,
        } = self;
        drop(_link);
        drop(retained);
        Ok(apartment)
    }
    pub fn pump_once(&self) -> io::Result<()> {
        self.apartment.pump_once()
    }
    pub fn parse(apartment: ComApartment, retained: RetainedShortcut) -> io::Result<Self> {
        apartment.check()?;
        retained.revalidate()?;
        let bytes = retained.entry_bytes()?;
        let layout = super::validate_link_bytes(&bytes)?;
        let link = load_link(
            retained
                .entry_path()
                .to_str()
                .ok_or_else(|| invalid("non Unicode entry path"))?,
        )?;
        // RAWPATH only. Resolve is deliberately never called.
        let (_, target) = get_path(&link, SLGP_RAWPATH)?;
        retained.profile().admit_metadata(&ShortcutMetadata {
            target,
            arguments: get_arguments(&link)?,
            working_directory: get_working_directory(&link)?,
            layout,
        })?;
        validate_executable_run_level(&retained)?;
        retained.revalidate()?;
        Ok(Self {
            retained,
            _link: link,
            apartment,
            _thread: PhantomData,
        })
    }
    pub fn profile(&self) -> &super::ShortcutProfile {
        self.retained.profile()
    }
    pub fn revalidate(&self) -> io::Result<()> {
        self.apartment.check()?;
        self.retained.revalidate()?;
        validate_executable_run_level(&self.retained)
    }
}
#[repr(C)]
struct ActCtx {
    size: u32,
    flags: u32,
    source: *const u16,
    processor: u16,
    language: u16,
    assembly: *const u16,
    resource: *const u16,
    application: *const u16,
    module: HANDLE,
}
#[repr(C)]
#[derive(Default)]
struct RunLevel {
    flags: u32,
    level: u32,
    ui_access: u32,
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateActCtxW(context: *const ActCtx) -> HANDLE;
    fn ReleaseActCtx(context: HANDLE);
    fn QueryActCtxW(
        flags: u32,
        context: HANDLE,
        sub: *const c_void,
        class: u32,
        buffer: *mut c_void,
        bytes: usize,
        needed: *mut usize,
    ) -> i32;
}
struct Context(HANDLE);
impl Drop for Context {
    fn drop(&mut self) {
        unsafe {
            ReleaseActCtx(self.0);
        }
    }
}
fn validate_executable_run_level(retained: &RetainedShortcut) -> io::Result<()> {
    if std::env::var_os("__COMPAT_LAYER").is_some() {
        return Err(invalid("inherited app-compat policy is unsupported"));
    }
    reject_compatibility_layers(retained)?;
    let path = wide(
        retained
            .executable_path()
            .to_str()
            .ok_or_else(|| invalid("non Unicode executable path"))?,
    );
    // Explicit embedded RT_MANIFEST #1 only; missing/unknown manifest refuses.
    let input = ActCtx {
        size: size_of::<ActCtx>() as u32,
        flags: 8,
        source: path.as_ptr(),
        processor: 0,
        language: 0,
        assembly: ptr::null(),
        resource: std::ptr::without_provenance::<u16>(1),
        application: ptr::null(),
        module: ptr::null_mut(),
    };
    let raw = unsafe { CreateActCtxW(&input) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(win32("CreateActCtxW(embedded manifest)"));
    }
    let context = Context(raw);
    let mut run = RunLevel::default();
    let mut needed = 0;
    // RunlevelInformationInActivationContext = 5 (winnt.h).
    if unsafe {
        QueryActCtxW(
            0,
            context.0,
            ptr::null(),
            5,
            (&mut run as *mut RunLevel).cast(),
            size_of::<RunLevel>(),
            &mut needed,
        )
    } == 0
    {
        return Err(win32("QueryActCtxW(RunLevel)"));
    }
    if needed > size_of::<RunLevel>() || run.flags != 0 {
        return Err(invalid("unknown executable elevation metadata"));
    }
    admit_run_level(run.level, run.ui_access != 0)
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegGetValueW(
        key: *mut c_void,
        subkey: *const u16,
        value: *const u16,
        flags: u32,
        kind: *mut u32,
        data: *mut c_void,
        bytes: *mut u32,
    ) -> i32;
}
fn reject_compatibility_layers(retained: &RetainedShortcut) -> io::Result<()> {
    let subkey = wide(r"Software\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Layers");
    let value = wide(
        retained
            .executable_path()
            .to_str()
            .ok_or_else(|| invalid("non Unicode executable path"))?,
    );
    for root in [0x80000001u32, 0x80000002u32] {
        for view in [0x00010000, 0x00020000] {
            let mut bytes = 0;
            // Read only, both registry views. Any explicit compatibility layer,
            // including RUNASADMIN, refuses rather than altering user policy.
            let result = unsafe {
                RegGetValueW(
                    root as i32 as isize as *mut c_void,
                    subkey.as_ptr(),
                    value.as_ptr(),
                    0x0000ffff | 0x10000000 | view,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut bytes,
                )
            };
            if result == 0 {
                return Err(invalid("executable has unsupported app-compat policy"));
            }
            if result != 2 && result != 3 {
                return Err(io::Error::other(NativeError {
                    api: "RegGetValueW(AppCompatFlags/Layers)",
                    code: NativeCode::Win32(result as u32),
                }));
            }
        }
    }
    Ok(())
}
