//! Retained native error plumbing and the owned SC_HANDLE.
use super::*;

pub(super) type Errors = Rc<RefCell<Vec<NativeError>>>;
pub(super) fn last(api: &'static str) -> NativeError {
    NativeError {
        api,
        code: unsafe { GetLastError() },
    }
}
fn io_native(error: &NativeError) -> io::Error {
    io::Error::from_raw_os_error(error.code as i32)
}
pub(super) fn keep_error(errors: &Errors, error: NativeError) -> io::Error {
    let result = io_native(&error);
    errors.borrow_mut().push(error);
    result
}
pub(super) struct OwnedHandle {
    pub(super) raw: Handle,
    pub(super) errors: Errors,
}
impl OwnedHandle {
    pub(super) fn open(raw: Handle, api: &'static str, errors: &Errors) -> io::Result<Self> {
        if raw.is_null() {
            let e = last(api);
            return Err(keep_error(errors, e));
        }
        Ok(Self {
            raw,
            errors: errors.clone(),
        })
    }
    pub(super) fn close(&mut self) -> io::Result<()> {
        if self.raw.is_null() {
            return Ok(());
        }
        if unsafe { CloseServiceHandle(self.raw) } == 0 {
            let e = last("CloseServiceHandle");
            return Err(keep_error(&self.errors, e));
        }
        self.raw = ptr::null_mut();
        Ok(())
    }
}
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if self.close().is_err() {
            // Failed close is not handle-closure evidence. Preserve diagnostics
            // and the still-open native owner until supplied-context disposal.
            forget(self.errors.clone());
        }
    }
}
