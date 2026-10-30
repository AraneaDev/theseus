//! Completed file flush, retained-root native replacement and owned-handle cleanup.
use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    ffi::OsStr,
    fs::File,
    io,
    mem::{align_of, offset_of, size_of},
    os::windows::{ffi::OsStrExt, io::AsRawHandle},
    ptr::NonNull,
};
use windows_sys::{
    Wdk::Storage::FileSystem::{
        FILE_RENAME_INFORMATION, FILE_RENAME_POSIX_SEMANTICS, FILE_RENAME_REPLACE_IF_EXISTS,
        FileRenameInformationEx, NtSetInformationFile,
    },
    Win32::{
        Foundation::{NTSTATUS, RtlNtStatusToDosError, STATUS_PENDING},
        Storage::FileSystem::*,
        System::IO::IO_STATUS_BLOCK,
    },
};

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
        let payload_length = u32::try_from(payload).map_err(|_| invalid())?;
        // Meet the documented native sizeof + filename requirement as well as
        // the allocation-aware flexible tail bounds; include one spare NUL.
        let capacity = size_of::<FILE_RENAME_INFORMATION>()
            .checked_add(payload)
            .and_then(|n| n.checked_add(2))
            .ok_or_else(invalid)?;
        let length = u32::try_from(capacity).map_err(|_| invalid())?;
        let layout = Layout::from_size_align(capacity, align_of::<FILE_RENAME_INFORMATION>())
            .map_err(|_| invalid())?;
        let pointer = NonNull::new(unsafe { alloc_zeroed(layout) })
            .ok_or_else(|| io::Error::other("cannot allocate rename information"))?;
        let buffer = Self {
            pointer,
            layout,
            length,
        };
        // SAFETY: aligned zeroed allocation covers the complete native record,
        // all UTF-16 units and spare NUL. No [u16; 1] out-of-bounds indexing.
        unsafe {
            let record = buffer.pointer.as_ptr().cast::<FILE_RENAME_INFORMATION>();
            (*record).Anonymous.Flags = FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS;
            (*record).RootDirectory = parent.as_raw_handle();
            (*record).FileNameLength = payload_length;
            std::ptr::copy_nonoverlapping(
                units.as_ptr(),
                buffer
                    .pointer
                    .as_ptr()
                    .add(offset_of!(FILE_RENAME_INFORMATION, FileName))
                    .cast::<u16>(),
                units.len(),
            );
        }
        Ok(buffer)
    }
}
impl Drop for RenameBuffer {
    fn drop(&mut self) {
        // SAFETY: exact original allocation layout, freed once after completion.
        unsafe { dealloc(self.pointer.as_ptr(), self.layout) };
    }
}

// All storage/authority exists before the native call, with stable owned addresses.
// The source is the original completed object, not the serializer's mutable slot.
struct RenameCall {
    status: IO_STATUS_BLOCK,
    buffer: RenameBuffer,
    _parent: File,
    source: File,
    #[cfg(test)]
    dropped: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}
impl RenameCall {
    fn new(parent: &File, source: &File, name: &OsStr) -> io::Result<Box<Self>> {
        let parent = parent.try_clone()?;
        let source = source.try_clone()?;
        let buffer = RenameBuffer::new(&parent, name)?;
        Ok(Box::new(Self {
            status: unsafe { std::mem::zeroed() },
            buffer,
            _parent: parent,
            source,
            #[cfg(test)]
            dropped: None,
        }))
    }
}
impl Drop for RenameCall {
    fn drop(&mut self) {
        #[cfg(test)]
        if let Some(dropped) = &self.dropped {
            dropped.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}
fn finish_rename(call: Box<RenameCall>, returned: NTSTATUS) -> io::Result<()> {
    if returned == STATUS_PENDING {
        // No completion was observed. Do not read concurrent IOSB storage or
        // release source/root/buffer. The wrapper must disarm temporary cleanup.
        std::mem::forget(call);
        return Err(io::Error::other(crate::fs::UnresolvedReplacement));
    }
    // Official IO_STATUS_BLOCK contract: every terminal return is authoritative.
    // A different/stale IOSB cannot leak this owner or change publication state.
    if returned < 0 {
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(returned) } as i32,
        ));
    }
    Ok(())
}
fn checked(ok: windows_sys::core::BOOL) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
pub(in crate::fs) fn flush_file(file: &File) -> io::Result<()> {
    // SAFETY: retained regular-file handle, completed synchronous writes only.
    checked(unsafe { FlushFileBuffers(file.as_raw_handle()) })
}
pub(in crate::fs) fn replace(
    parent: &File,
    _temporary: &OsStr,
    file: &File,
    name: &OsStr,
) -> io::Result<()> {
    let mut call = RenameCall::new(parent, file, name)?;
    // SAFETY: synchronous source acquired with DELETE access; boxed IOSB,
    // aligned native buffer, and both retained handles outlive the operation.
    let returned = unsafe {
        NtSetInformationFile(
            call.source.as_raw_handle(),
            &mut call.status,
            call.buffer.pointer.as_ptr().cast(),
            call.buffer.length,
            FileRenameInformationEx,
        )
    };
    finish_rename(call, returned)
}
pub(in crate::fs) fn remove_temporary(
    _parent: &File,
    _name: &OsStr,
    file: &File,
) -> io::Result<()> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: only the owned exclusive temporary is marked, then File closes it.
    // Unknown replacement never reaches this path. No destination deletion.
    checked(unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{CommitScope, HostDir, RelativePath};
    use std::{
        io::{Read, Write},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    use windows_sys::Win32::Foundation::STATUS_INVALID_PARAMETER;

    #[test]
    fn native_rename_abi_and_flags_match_x64_msvc() {
        assert_eq!(size_of::<FILE_RENAME_INFORMATION>(), 24);
        assert_eq!(align_of::<FILE_RENAME_INFORMATION>(), 8);
        assert_eq!(offset_of!(FILE_RENAME_INFORMATION, RootDirectory), 8);
        assert_eq!(offset_of!(FILE_RENAME_INFORMATION, FileNameLength), 16);
        assert_eq!(offset_of!(FILE_RENAME_INFORMATION, FileName), 20);
        assert_eq!(FileRenameInformationEx, 65);
        assert_eq!(
            FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS,
            3
        );
    }
    #[test]
    fn terminal_return_ignores_stale_pending_iosb_and_releases_owned_call() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = HostDir::create_private(&temp.path().join("root"))?;
        let source = root.create_file(&RelativePath::parse("source")?)?;
        for status in [0, STATUS_INVALID_PARAMETER] {
            let dropped = Arc::new(AtomicBool::new(false));
            let mut call = RenameCall::new(&root.directory, &source, OsStr::new("target"))?;
            call.status.Anonymous.Status = STATUS_PENDING;
            call.dropped = Some(dropped.clone());
            // No kernel call is started: this seam checks returned-status
            // classification and terminal-owner release, not pending completion.
            let result = finish_rename(call, status);
            assert!(dropped.load(Ordering::SeqCst));
            if status == 0 {
                result?;
            } else {
                assert_eq!(result.unwrap_err().raw_os_error(), Some(87));
            }
        }
        Ok(())
    }
    #[test]
    fn selected_native_replacement_preserves_source_identity_and_retained_reader() -> io::Result<()>
    {
        let temp = tempfile::tempdir()?;
        let root = HostDir::create_private(&temp.path().join("root"))?;
        root.publish(OsStr::new("report"), &mut |file| file.write_all(b"old"))?;
        let mut old = root.open_file(&RelativePath::parse("report")?)?;
        let mut source_id = None;
        let scope = root.publish(OsStr::new("report"), &mut |file| {
            file.write_all(b"new completed bytes")?;
            source_id = Some(super::super::identity::query::<FILE_ID_INFO>(
                file, FileIdInfo,
            )?);
            Ok(())
        })?;
        assert_eq!(scope, CommitScope::FileFlushedAtomicReplace);
        let mut target = root.open_file(&RelativePath::parse("report")?)?;
        let target_id: FILE_ID_INFO = super::super::identity::query(&target, FileIdInfo)?;
        let source_id = source_id.expect("writer completed identity query");
        assert_eq!(source_id.VolumeSerialNumber, target_id.VolumeSerialNumber);
        assert_eq!(source_id.FileId.Identifier, target_id.FileId.Identifier);
        let mut old_bytes = Vec::new();
        old.read_to_end(&mut old_bytes)?;
        assert_eq!(old_bytes, b"old");
        let mut bytes = Vec::new();
        target.read_to_end(&mut bytes)?;
        assert_eq!(bytes, b"new completed bytes");
        assert_eq!(root.entries()?, vec!["report"]);
        Ok(())
    }
}
