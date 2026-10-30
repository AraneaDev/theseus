//! Completed regular-file flush, handle-relative replacement and owned-handle cleanup.
use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    ffi::OsStr,
    fs::File,
    io,
    mem::{align_of, offset_of, size_of},
    os::windows::{ffi::OsStrExt, io::AsRawHandle},
    ptr::NonNull,
};
use windows_sys::Win32::Storage::FileSystem::*;

struct RenameBuffer {
    pointer: NonNull<u8>,
    layout: Layout,
    length: u32,
}
impl RenameBuffer {
    fn new(parent: &File, name: &OsStr) -> io::Result<Self> {
        crate::fs::validate_name(name)?;
        let units: Vec<u16> = name.encode_wide().collect();
        let invalid = || crate::fs::invalid("rename component too long");
        let payload = units.len().checked_mul(2).ok_or_else(invalid)?;
        let payload_u32 = u32::try_from(payload).map_err(|_| invalid())?;
        let offset = offset_of!(FILE_RENAME_INFO, FileName);
        let capacity = offset
            .checked_add(payload)
            .and_then(|n| n.checked_add(2))
            .ok_or_else(invalid)?
            .max(size_of::<FILE_RENAME_INFO>());
        let length = u32::try_from(capacity).map_err(|_| invalid())?;
        let layout = Layout::from_size_align(capacity, align_of::<FILE_RENAME_INFO>())
            .map_err(|_| invalid())?;
        // SAFETY: nonzero checked layout, correctly aligned for the actual FFI record.
        let pointer = NonNull::new(unsafe { alloc_zeroed(layout) })
            .ok_or_else(|| io::Error::other("cannot allocate rename information"))?;
        let buffer = Self {
            pointer,
            layout,
            length,
        };
        // SAFETY: the allocation covers the header plus every UTF-16 code unit and
        // spare NUL. Tail copies use allocation-aware raw pointers, not [u16; 1].
        unsafe {
            let record = buffer.pointer.as_ptr().cast::<FILE_RENAME_INFO>();
            (*record).Anonymous.ReplaceIfExists = true;
            (*record).RootDirectory = parent.as_raw_handle();
            (*record).FileNameLength = payload_u32;
            std::ptr::copy_nonoverlapping(
                units.as_ptr(),
                buffer.pointer.as_ptr().add(offset).cast::<u16>(),
                units.len(),
            );
        }
        Ok(buffer)
    }
}
impl Drop for RenameBuffer {
    fn drop(&mut self) {
        // SAFETY: exact allocation layout, freed once after the synchronous call.
        unsafe { dealloc(self.pointer.as_ptr(), self.layout) };
    }
}
fn checked(ok: windows_sys::core::BOOL) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
pub(in crate::fs) fn flush_file(file: &File) -> io::Result<()> {
    // SAFETY: live owned regular-file handle, synchronous completed writes only.
    checked(unsafe { FlushFileBuffers(file.as_raw_handle()) })
}
pub(in crate::fs) fn replace(
    parent: &File,
    _temporary: &OsStr,
    file: &File,
    name: &OsStr,
) -> io::Result<()> {
    let buffer = RenameBuffer::new(parent, name)?;
    // SAFETY: parent, source and aligned native buffer remain live for this
    // synchronous Win32 call. Source was exclusively created with DELETE access.
    checked(unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileRenameInfo,
            buffer.pointer.as_ptr().cast(),
            buffer.length,
        )
    })
}
pub(in crate::fs) fn remove_temporary(
    _parent: &File,
    _name: &OsStr,
    file: &File,
) -> io::Result<()> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: only the owned exclusive temporary is marked, and File closes it
    // after this call. No pathname lookup or destination deletion is performed.
    checked(unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    })
}
