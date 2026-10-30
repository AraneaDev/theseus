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

#[cfg(test)]
mod probes {
    use super::*;
    use crate::fs::{HostDir, RelativePath};
    use std::io::{Read, Write};

    // Causal calibration only: production keeps the selected retained-root call.
    // Each case owns a fresh private root, source and target. The absolute path
    // in the working control is test-only and never authorizes publication.
    fn rename_with_documented_buffer(relative: bool) -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root_path = temp.path().join("probe");
        let root = HostDir::create_private(&root_path)?;
        let mut source = root.create_file(&RelativePath::parse("source")?)?;
        source.write_all(b"completed probe bytes")?;
        flush_file(&source)?;
        let absolute_target = root_path.join("target");
        let target = if relative {
            OsStr::new("target")
        } else {
            absolute_target.as_os_str()
        };
        let units: Vec<u16> = target.encode_wide().collect();
        let payload = units
            .len()
            .checked_mul(2)
            .ok_or_else(|| io::Error::other("probe size overflow"))?;
        // Deliberately satisfy the conservative documented sizeof + name size
        // requirement as well as the actual flexible-tail bounds and spare NUL.
        let capacity = size_of::<FILE_RENAME_INFO>()
            .checked_add(payload)
            .and_then(|n| n.checked_add(2))
            .ok_or_else(|| io::Error::other("probe size overflow"))?;
        let layout = Layout::from_size_align(capacity, align_of::<FILE_RENAME_INFO>())
            .map_err(io::Error::other)?;
        let length = u32::try_from(capacity).map_err(io::Error::other)?;
        let buffer = RenameBuffer {
            pointer: NonNull::new(unsafe { alloc_zeroed(layout) })
                .ok_or_else(|| io::Error::other("probe allocation failed"))?,
            layout,
            length,
        };
        // SAFETY: independently checked/aligned owned storage, full initialized
        // header plus native UTF-16 payload and spare NUL; source/root remain live.
        unsafe {
            let record = buffer.pointer.as_ptr().cast::<FILE_RENAME_INFO>();
            (*record).Anonymous.ReplaceIfExists = true;
            (*record).RootDirectory = if relative {
                root.directory.as_raw_handle()
            } else {
                std::ptr::null_mut()
            };
            (*record).FileNameLength = u32::try_from(payload).map_err(io::Error::other)?;
            std::ptr::copy_nonoverlapping(
                units.as_ptr(),
                buffer
                    .pointer
                    .as_ptr()
                    .add(offset_of!(FILE_RENAME_INFO, FileName))
                    .cast::<u16>(),
                units.len(),
            );
        }
        if relative {
            // B varies only allocation/call length. Check every existing header,
            // tail and padding byte against the unchanged production builder.
            let baseline = RenameBuffer::new(&root.directory, target)?;
            let baseline_bytes = unsafe {
                std::slice::from_raw_parts(baseline.pointer.as_ptr(), baseline.length as usize)
            };
            let oversized_prefix = unsafe {
                std::slice::from_raw_parts(buffer.pointer.as_ptr(), baseline.length as usize)
            };
            assert_eq!(baseline_bytes, oversized_prefix);
        }
        let result = checked(unsafe {
            SetFileInformationByHandle(
                source.as_raw_handle(),
                FileRenameInfo,
                buffer.pointer.as_ptr().cast(),
                buffer.length,
            )
        });
        let observation = format!(
            "case={}\nrecord_size={}\nrecord_alignment={}\nfilename_offset={}\nfilename_bytes={}\nbuffer_bytes={}\nclass={}\nroot_nonnull={}\nresult={:?}\n",
            if relative {
                "relative-oversize"
            } else {
                "absolute-working-control"
            },
            size_of::<FILE_RENAME_INFO>(),
            align_of::<FILE_RENAME_INFO>(),
            offset_of!(FILE_RENAME_INFO, FileName),
            payload,
            capacity,
            FileRenameInfo,
            relative,
            result
        );
        eprintln!("{observation}");
        // CI owns this exact evidence directory. Preserve successful controls as
        // raw artifacts too, since the Rust harness hides successful stdout.
        if let Some(evidence) = std::env::var_os("THESEUS_HOST_EVIDENCE") {
            std::fs::write(
                std::path::Path::new(&evidence).join(if relative {
                    "win32-rename-probe-relative.txt"
                } else {
                    "win32-rename-probe-absolute.txt"
                }),
                observation,
            )?;
        }
        result?;
        let mut bytes = Vec::new();
        root.open_file(&RelativePath::parse("target")?)?
            .read_to_end(&mut bytes)?;
        assert_eq!(bytes, b"completed probe bytes");
        assert_eq!(root.entries()?, vec!["target"]);
        Ok(())
    }
    #[test]
    fn win32_relative_rename_with_documented_oversize_buffer() -> io::Result<()> {
        rename_with_documented_buffer(true)
    }
    #[test]
    fn win32_absolute_rename_working_control() -> io::Result<()> {
        rename_with_documented_buffer(false)
    }
    #[test]
    fn native_retained_root_rename_control() -> io::Result<()> {
        use windows_sys::{
            Wdk::Storage::FileSystem::{
                FILE_RENAME_INFORMATION, FileRenameInformation, NtSetInformationFile,
            },
            Win32::{
                Foundation::{RtlNtStatusToDosError, STATUS_PENDING},
                System::IO::IO_STATUS_BLOCK,
            },
        };
        // Retain all storage and the private directory itself if the unexpected
        // pending branch cannot establish completion. No destructor may race it.
        struct NativeCall {
            status: IO_STATUS_BLOCK,
            buffer: RenameBuffer,
            root: HostDir,
            source: File,
            _temporary: tempfile::TempDir,
        }
        let temp = tempfile::tempdir()?;
        let root = HostDir::create_private(&temp.path().join("probe"))?;
        let mut source = root.create_file(&RelativePath::parse("source")?)?;
        source.write_all(b"completed native probe bytes")?;
        flush_file(&source)?;
        let before: FILE_ID_INFO = super::super::identity::query(&source, FileIdInfo)?;
        let units: Vec<u16> = OsStr::new("target").encode_wide().collect();
        let payload = units
            .len()
            .checked_mul(2)
            .ok_or_else(|| io::Error::other("probe size overflow"))?;
        let capacity = size_of::<FILE_RENAME_INFORMATION>()
            .checked_add(payload)
            .and_then(|n| n.checked_add(2))
            .ok_or_else(|| io::Error::other("probe size overflow"))?;
        let layout = Layout::from_size_align(capacity, align_of::<FILE_RENAME_INFORMATION>())
            .map_err(io::Error::other)?;
        let length = u32::try_from(capacity).map_err(io::Error::other)?;
        let payload_length = u32::try_from(payload).map_err(io::Error::other)?;
        let buffer = RenameBuffer {
            pointer: NonNull::new(unsafe { alloc_zeroed(layout) })
                .ok_or_else(|| io::Error::other("probe allocation failed"))?,
            layout,
            length,
        };
        let mut call = Box::new(NativeCall {
            status: unsafe { std::mem::zeroed() },
            buffer,
            root,
            source,
            _temporary: temp,
        });
        // SAFETY: the whole operation owner is boxed before pointers are given to
        // NT. Its aligned allocation contains the native record and complete tail.
        unsafe {
            let record = call
                .buffer
                .pointer
                .as_ptr()
                .cast::<FILE_RENAME_INFORMATION>();
            (*record).Anonymous.ReplaceIfExists = true;
            (*record).RootDirectory = call.root.directory.as_raw_handle();
            (*record).FileNameLength = payload_length;
            std::ptr::copy_nonoverlapping(
                units.as_ptr(),
                call.buffer
                    .pointer
                    .as_ptr()
                    .add(offset_of!(FILE_RENAME_INFORMATION, FileName))
                    .cast::<u16>(),
                units.len(),
            );
        }
        let status = unsafe {
            NtSetInformationFile(
                call.source.as_raw_handle(),
                &mut call.status,
                call.buffer.pointer.as_ptr().cast(),
                call.buffer.length,
                FileRenameInformation,
            )
        };
        // Do not read storage potentially still being written after STATUS_PENDING.
        // The selected synchronous handle contract supplies terminal return status;
        // there is no speculative wait or cancellation protocol in this probe.
        let completed_status = if status == STATUS_PENDING {
            None
        } else {
            Some(unsafe { call.status.Anonymous.Status })
        };
        let unresolved = status == STATUS_PENDING || completed_status == Some(STATUS_PENDING);
        let observation = format!(
            "case=native-retained-root\nrecord_size={}\nrecord_alignment={}\nfilename_offset={}\nfilename_bytes={}\nbuffer_bytes={}\nclass={}\nroot_nonnull=true\nntstatus={:#010x}\ncompleted_iosb_status={:?}\npublication_state={}\n",
            size_of::<FILE_RENAME_INFORMATION>(),
            align_of::<FILE_RENAME_INFORMATION>(),
            offset_of!(FILE_RENAME_INFORMATION, FileName),
            payload,
            capacity,
            FileRenameInformation,
            status as u32,
            completed_status,
            if unresolved {
                "UNKNOWN; entire operation and fixture retained"
            } else {
                "terminal status observed"
            }
        );
        let record_observation = || -> io::Result<()> {
            eprintln!("{observation}");
            if let Some(evidence) = std::env::var_os("THESEUS_HOST_EVIDENCE") {
                std::fs::write(
                    std::path::Path::new(&evidence).join("native-rename-probe-root.txt"),
                    &observation,
                )?;
            }
            Ok(())
        };
        if unresolved {
            std::mem::forget(call);
            let evidence_error = record_observation().err();
            return Err(io::Error::other(format!(
                "UNKNOWN publication state: native probe operation and private fixture retained until process exit; evidence error: {evidence_error:?}"
            )));
        }
        record_observation()?;
        if status < 0 {
            return Err(io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(status) } as i32,
            ));
        }
        if let Some(status) = completed_status.filter(|s| *s < 0) {
            return Err(io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(status) } as i32,
            ));
        }
        let mut target = call.root.open_file(&RelativePath::parse("target")?)?;
        let mut bytes = Vec::new();
        target.read_to_end(&mut bytes)?;
        assert_eq!(bytes, b"completed native probe bytes");
        assert_eq!(call.root.entries()?, vec!["target"]);
        let after: FILE_ID_INFO = super::super::identity::query(&target, FileIdInfo)?;
        assert_eq!(before.VolumeSerialNumber, after.VolumeSerialNumber);
        assert_eq!(before.FileId.Identifier, after.FileId.Identifier);
        Ok(())
    }
}
