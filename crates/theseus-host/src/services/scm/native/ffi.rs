//! Narrow advapi32/kernel32 ABI subset: declarations, constants and repr(C) layouts.
use super::*;

// Narrow ABI subset of winsvc.h. These declarations avoid adding a Services SDK
// feature to the shared manifest; all signatures/layouts match the Windows SDK.
pub(super) type Handle = *mut c_void;
#[link(name = "advapi32")]
unsafe extern "system" {
    pub(super) fn OpenSCManagerW(machine: *const u16, database: *const u16, access: u32) -> Handle;
    pub(super) fn OpenServiceW(manager: Handle, name: *const u16, access: u32) -> Handle;
    pub(super) fn CloseServiceHandle(handle: Handle) -> i32;
    pub(super) fn QueryServiceConfigW(
        service: Handle,
        buffer: *mut QueryConfig,
        size: u32,
        needed: *mut u32,
    ) -> i32;
    pub(super) fn QueryServiceConfig2W(
        service: Handle,
        level: u32,
        buffer: *mut u8,
        size: u32,
        needed: *mut u32,
    ) -> i32;
    pub(super) fn QueryServiceStatusEx(
        service: Handle,
        level: i32,
        buffer: *mut u8,
        size: u32,
        needed: *mut u32,
    ) -> i32;
    pub(super) fn StartServiceW(service: Handle, argc: u32, argv: *const *const u16) -> i32;
    pub(super) fn ControlService(service: Handle, control: u32, status: *mut ControlStatus) -> i32;
}
// Read-only registry subset (winreg.h) for the services.exe DeleteFlag.
#[link(name = "advapi32")]
unsafe extern "system" {
    pub(super) fn RegOpenKeyExW(
        key: Handle,
        subkey: *const u16,
        options: u32,
        sam: u32,
        out: *mut Handle,
    ) -> i32;
    pub(super) fn RegQueryValueExW(
        key: Handle,
        name: *const u16,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        size: *mut u32,
    ) -> i32;
    pub(super) fn RegCloseKey(key: Handle) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    pub(super) fn GetLastError() -> u32;
}
pub(super) const HKEY_LOCAL_MACHINE: isize = 0x8000_0002u32 as i32 as isize;
pub(super) const KEY_QUERY_VALUE: u32 = 0x0001;
pub(super) const REG_DWORD: u32 = 4;
pub(super) const ERROR_FILE_NOT_FOUND: i32 = 2;
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct QueryConfig {
    pub(super) service_type: u32,
    pub(super) start_type: u32,
    pub(super) error_control: u32,
    pub(super) binary: *const u16,
    pub(super) group: *const u16,
    pub(super) tag: u32,
    pub(super) dependencies: *const u16,
    pub(super) account: *const u16,
    pub(super) display: *const u16,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct NativeFailureAction {
    pub(super) kind: u32,
    pub(super) delay: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct FailureActions {
    pub(super) reset: u32,
    pub(super) reboot: *const u16,
    pub(super) command: *const u16,
    pub(super) count: u32,
    pub(super) actions: *const NativeFailureAction,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct WidePointer {
    pub(super) value: *const u16,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct TriggerInfo {
    pub(super) count: u32,
    pub(super) triggers: *const c_void,
    pub(super) reserved: *const c_void,
}
#[repr(C)]
#[derive(Default)]
pub(super) struct ControlStatus {
    pub(super) service_type: u32,
    pub(super) state: u32,
    pub(super) controls: u32,
    pub(super) exit: u32,
    pub(super) specific_exit: u32,
    pub(super) checkpoint: u32,
    pub(super) wait_hint: u32,
}
#[repr(C)]
#[derive(Default)]
pub(super) struct ProcessStatus {
    pub(super) service_type: u32,
    pub(super) state: u32,
    pub(super) controls: u32,
    pub(super) exit: u32,
    pub(super) specific_exit: u32,
    pub(super) checkpoint: u32,
    pub(super) wait_hint: u32,
    pub(super) pid: u32,
    pub(super) flags: u32,
}
// SAFETY: these repr(C) records contain only integers and raw pointers; all
// initialized bit patterns are valid. Every returned interior pointer is bounded
// by NativeBuffer before it is read.
unsafe impl super::buffer::NativeValue for QueryConfig {}
unsafe impl super::buffer::NativeValue for NativeFailureAction {}
unsafe impl super::buffer::NativeValue for FailureActions {}
unsafe impl super::buffer::NativeValue for WidePointer {}
unsafe impl super::buffer::NativeValue for TriggerInfo {}

pub(super) const INSUFFICIENT_BUFFER: u32 = 122;
pub(super) const DOES_NOT_EXIST: u32 = 1060;
pub(super) const MARKED_FOR_DELETE: u32 = 1072;
