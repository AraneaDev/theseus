use super::identity::query;
use crate::fs::{RootPolicy, invalid};
use crate::windows_security::PrivateSecurity;
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    mem::{offset_of, size_of, zeroed},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::FileExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path},
    ptr::{null, null_mut},
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem as nt,
        System::SystemServices::{FILE_FS_DEVICE_INFORMATION, FILE_REMOTE_DEVICE},
    },
    Win32::{
        Foundation::*,
        Storage::FileSystem::*,
        System::{
            IO::IO_STATUS_BLOCK,
            SystemServices::FILE_PERSISTENT_ACLS,
            Threading::{INFINITE, WaitForSingleObject},
        },
    },
};

fn nt_error(status: NTSTATUS) -> io::Error {
    // SAFETY: pure NTSTATUS mapping, no borrowed storage.
    io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32)
}
fn wide(text: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<u16> = text.encode_wide().collect();
    if value.contains(&0) {
        return Err(invalid("NUL in Windows path"));
    }
    value.push(0);
    Ok(value)
}
/// Check native code units without replacing unpaired surrogates. Forbidden
/// syntax is ASCII (plus the explicitly reserved superscript device suffixes),
/// so a non-Unicode name cannot hide a separator, stream or alias through decoding.
pub(in crate::fs) fn validate_native_name(name: &OsStr) -> io::Result<()> {
    let units: Vec<u16> = name.encode_wide().collect();
    if units.is_empty()
        || units == [46]
        || units == [46, 46]
        || matches!(units.last(), Some(46 | 32))
        || units
            .iter()
            .any(|unit| *unit <= 31 || matches!(*unit, 34 | 42 | 47 | 58 | 60 | 62 | 63 | 92 | 124))
    {
        return Err(invalid("invalid Windows component"));
    }
    // ASCII-only case folding is exactly the device-alias rule; preserve all
    // other code units, including unpaired surrogates, for the actual native open.
    let stem: Vec<u16> = units
        .iter()
        .take_while(|u| **u != 46)
        .map(|u| if (97..=122).contains(u) { *u - 32 } else { *u })
        .collect();
    let reserved = matches!(
        stem.as_slice(),
        [67, 79, 78]
            | [80, 82, 78]
            | [65, 85, 88]
            | [78, 85, 76]
            | [67, 79, 78, 73, 78, 36]
            | [67, 79, 78, 79, 85, 84, 36]
    ) || (stem.len() == 4
        && (stem[..3] == [67, 79, 77] || stem[..3] == [76, 80, 84])
        && matches!(stem[3], 49..=57 | 185 | 178 | 179));
    if reserved {
        return Err(invalid("reserved Windows device component"));
    }
    Ok(())
}

pub(in crate::fs) fn open_root(path: &Path, policy: RootPolicy) -> io::Result<File> {
    if matches!(policy, RootPolicy::AllComponentsNoFollow)
        && path.components().any(|c| c == Component::ParentDir)
    {
        return Err(invalid("unsafe directory path"));
    }
    // Normalize redundant trailing separators/dots, so a leaf alias cannot become
    // an ancestor alias by spelling it `alias/` or `alias/.`.
    let path: std::path::PathBuf = path.components().collect();
    let absolute = std::path::absolute(&path)?;
    let file = match policy {
        RootPolicy::LeafNoFollow => bootstrap(&absolute, true)?,
        RootPolicy::AllComponentsNoFollow => {
            let mut components = absolute.components();
            let Some(Component::Prefix(prefix)) = components.next() else {
                return Err(invalid("expected local drive root"));
            };
            if !matches!(
                prefix.kind(),
                std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
            ) {
                return Err(invalid("unsupported Windows root"));
            }
            if components.next() != Some(Component::RootDir) {
                return Err(invalid("expected absolute Windows root"));
            }
            let mut base = std::path::PathBuf::from(prefix.as_os_str());
            base.push("\\");
            let mut directory = bootstrap(&base, true)?;
            for component in components {
                match component {
                    Component::Normal(name) => directory = open_dir(&directory, name)?,
                    Component::CurDir => (),
                    _ => return Err(invalid("unsafe directory path")),
                }
            }
            directory
        }
    };
    local_ntfs(&file)?;
    Ok(file)
}
pub(in crate::fs) fn open_creation_parent(path: &Path) -> io::Result<File> {
    let file = bootstrap(&std::path::absolute(path)?, false)?;
    local_ntfs(&file)?;
    Ok(file)
}
fn bootstrap(path: &Path, no_follow: bool) -> io::Result<File> {
    let name = wide(path.as_os_str())?;
    // SAFETY: nul-terminated owned UTF-16 is live for this synchronous call. Only
    // a successful non-sentinel returned handle becomes an owned File.
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS
                | if no_follow {
                    FILE_FLAG_OPEN_REPARSE_POINT
                } else {
                    0
                },
            null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_handle(raw) };
    validate(&file, true)?;
    Ok(file)
}

// Keep every input/output and the borrowed root alive if an exceptional pending
// completion cannot be observed. Normal synchronous calls drop this owner promptly.
struct OpenCall {
    handle: HANDLE,
    status: IO_STATUS_BLOCK,
    name: Vec<u16>,
    unicode: UNICODE_STRING,
    attributes: OBJECT_ATTRIBUTES,
    security: Option<PrivateSecurity>,
    parent: File,
}
fn relative(parent: &File, name: &OsStr, directory: bool, create: bool) -> io::Result<File> {
    crate::fs::validate_name(name)?;
    relative_open(parent, name, directory, create, false)
}
fn relative_open(
    parent: &File,
    name: &OsStr,
    directory: bool,
    create: bool,
    ledger: bool,
) -> io::Result<File> {
    let name = wide(name)?;
    let length = u16::try_from(
        (name.len() - 1)
            .checked_mul(2)
            .ok_or_else(|| invalid("component too long"))?,
    )
    .map_err(|_| invalid("component too long"))?;
    let maximum = length
        .checked_add(2)
        .ok_or_else(|| invalid("component too long"))?;
    // SAFETY: these C records permit zero initialization; all required fields are
    // initialized below before use. Box and Vec allocations keep pointees stable.
    let mut call = Box::new(OpenCall {
        handle: null_mut(),
        status: unsafe { zeroed() },
        name,
        unicode: unsafe { zeroed() },
        attributes: unsafe { zeroed() },
        security: if create {
            Some(PrivateSecurity::new()?)
        } else {
            None
        },
        parent: parent.try_clone()?,
    });
    call.unicode = UNICODE_STRING {
        Length: length,
        MaximumLength: maximum,
        Buffer: call.name.as_mut_ptr(),
    };
    call.attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: call.parent.as_raw_handle(),
        ObjectName: &call.unicode,
        Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        SecurityDescriptor: call.security.as_mut().map_or(null(), |s| s.as_ptr()),
        SecurityQualityOfService: null_mut(),
    };
    let access = if directory {
        FILE_LIST_DIRECTORY
            | FILE_TRAVERSE
            | FILE_READ_ATTRIBUTES
            | READ_CONTROL
            | SYNCHRONIZE
            | if create {
                FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | DELETE
            } else {
                0
            }
    } else {
        FILE_GENERIC_READ
            | if ledger { FILE_GENERIC_WRITE } else { 0 }
            | if create {
                FILE_GENERIC_WRITE | DELETE
            } else {
                0
            }
    };
    let options = nt::FILE_OPEN_REPARSE_POINT
        | nt::FILE_SYNCHRONOUS_IO_NONALERT
        | if directory {
            nt::FILE_DIRECTORY_FILE
        } else {
            nt::FILE_NON_DIRECTORY_FILE
        };
    // SAFETY: root is retained, one validated component only, descriptor and all
    // native records remain owned until terminal completion is observed.
    let mut status = unsafe {
        nt::NtCreateFile(
            &mut call.handle,
            access,
            &call.attributes,
            &mut call.status,
            null(),
            FILE_ATTRIBUTE_NORMAL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            if create {
                nt::FILE_CREATE
            } else {
                nt::FILE_OPEN
            },
            options,
            null(),
            0,
        )
    };
    if status == STATUS_PENDING {
        if call.handle.is_null()
            || unsafe { WaitForSingleObject(call.handle, INFINITE) } != WAIT_OBJECT_0
        {
            // Do not release any possibly pending operation storage/authority.
            std::mem::forget(call);
            return Err(io::Error::other(
                "pending native open could not be settled; operation retained",
            ));
        }
        status = unsafe { call.status.Anonymous.Status };
    }
    if status < 0 {
        return Err(nt_error(status));
    }
    if status == STATUS_PENDING || call.handle.is_null() || call.handle == INVALID_HANDLE_VALUE {
        std::mem::forget(call);
        return Err(io::Error::other(
            "native open returned no terminal handle; operation retained",
        ));
    }
    // SAFETY: NtCreateFile completed successfully and transfers this unique handle.
    let file = unsafe { File::from_raw_handle(call.handle) };
    validate(&file, directory)?;
    // Required file identity must work before the handle is used as evidence.
    let _: FILE_ID_INFO = query(&file, FileIdInfo)?;
    Ok(file)
}
// This owner is never accessed after issuance is classified unresolved. Its
// allocations, native pointers, handle, parent and security are retained together.
// Moving the outer box changes no native pointee; no Rust thread may dereference
// or mutate the inner records. Kernel completion may still write them.
struct RetainedLeafOpen(#[allow(dead_code)] Box<OpenCall>);
unsafe impl Send for RetainedLeafOpen {}
unsafe impl Sync for RetainedLeafOpen {}

pub(in crate::fs) fn open_leaf_classified(
    parent: &File,
    name: &OsStr,
) -> crate::fs::LeafOpenResult {
    open_leaf_classified_shared(
        parent,
        name,
        FILE_GENERIC_READ,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    )
}
/// Read-stable inspection open: shares read and delete only, never write.
/// While the returned handle lives, no other handle can open the file for
/// write (including writable mappings created through a new handle); an
/// existing writer makes this open fail with a settled sharing violation.
/// Rename/replacement remains possible and is detected by file identity.
pub(in crate::fs) fn open_leaf_deny_write_classified(
    parent: &File,
    name: &OsStr,
) -> crate::fs::LeafOpenResult {
    open_leaf_classified_shared(
        parent,
        name,
        FILE_GENERIC_READ,
        FILE_SHARE_READ | FILE_SHARE_DELETE,
    )
}
/// Identity-verified removal open: read plus DELETE access (needed by
/// FileDispositionInfo), sharing like the default classified open. Used only
/// by `remove_created_leaf_classified` after the caller's retained handle and
/// this handle prove the same file identity; no content-write access.
pub(in crate::fs) fn open_leaf_for_delete_classified(
    parent: &File,
    name: &OsStr,
) -> crate::fs::LeafOpenResult {
    open_leaf_classified_shared(
        parent,
        name,
        FILE_GENERIC_READ | DELETE,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    )
}
fn open_leaf_classified_shared(
    parent: &File,
    name: &OsStr,
    access: u32,
    share: u32,
) -> crate::fs::LeafOpenResult {
    use crate::fs::LeafOpenResult;
    let result = (|| -> Result<File, LeafOpenResult> {
        let name = wide(name).map_err(LeafOpenResult::FailedSettled)?;
        let length = u16::try_from(
            (name.len() - 1)
                .checked_mul(2)
                .ok_or_else(|| LeafOpenResult::FailedSettled(invalid("component too long")))?,
        )
        .map_err(|_| LeafOpenResult::FailedSettled(invalid("component too long")))?;
        let maximum = length
            .checked_add(2)
            .ok_or_else(|| LeafOpenResult::FailedSettled(invalid("component too long")))?;
        // SAFETY: these C records permit zero initialization; all required fields are
        // initialized below before use. Box and Vec allocations keep pointees stable.
        let mut call = Box::new(OpenCall {
            handle: null_mut(),
            status: unsafe { zeroed() },
            name,
            unicode: unsafe { zeroed() },
            attributes: unsafe { zeroed() },
            security: None,
            parent: parent.try_clone().map_err(LeafOpenResult::FailedSettled)?,
        });
        call.unicode = UNICODE_STRING {
            Length: length,
            MaximumLength: maximum,
            Buffer: call.name.as_mut_ptr(),
        };
        call.attributes = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: call.parent.as_raw_handle(),
            ObjectName: &call.unicode,
            Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
            SecurityDescriptor: call.security.as_mut().map_or(null(), |s| s.as_ptr()),
            SecurityQualityOfService: null_mut(),
        };
        let options = nt::FILE_OPEN_REPARSE_POINT
            | nt::FILE_SYNCHRONOUS_IO_NONALERT
            | nt::FILE_NON_DIRECTORY_FILE;
        // SAFETY: root is retained, one validated component only, descriptor and all
        // native records remain owned until terminal completion is observed.
        let mut status = unsafe {
            nt::NtCreateFile(
                &mut call.handle,
                access,
                &call.attributes,
                &mut call.status,
                null(),
                FILE_ATTRIBUTE_NORMAL,
                share,
                nt::FILE_OPEN,
                options,
                null(),
                0,
            )
        };
        if status == STATUS_PENDING {
            if call.handle.is_null() || call.handle == INVALID_HANDLE_VALUE {
                return Err(LeafOpenResult::Unsettled(
                    crate::fs::LeafOpenUncertainty::retain(
                        nt_error(status),
                        RetainedLeafOpen(call),
                    ),
                ));
            }
            // A classified leaf lookup must not wait past its caller's
            // cooperative inspection/cancellation budget. An unfinished native
            // call keeps its original storage and handle in Unsettled.
            let waited = unsafe { WaitForSingleObject(call.handle, 0) };
            if waited != WAIT_OBJECT_0 {
                let error = if waited == WAIT_FAILED {
                    io::Error::last_os_error()
                } else {
                    nt_error(status)
                };
                return Err(LeafOpenResult::Unsettled(
                    crate::fs::LeafOpenUncertainty::retain(error, RetainedLeafOpen(call)),
                ));
            }
            status = unsafe { call.status.Anonymous.Status };
        }
        if status == STATUS_PENDING {
            return Err(LeafOpenResult::Unsettled(
                crate::fs::LeafOpenUncertainty::retain(nt_error(status), RetainedLeafOpen(call)),
            ));
        }
        if status < 0 {
            // Raw terminal leaf-name status, never a DOS ErrorKind conversion.
            return Err(if status == STATUS_OBJECT_NAME_NOT_FOUND {
                LeafOpenResult::MissingSettled
            } else {
                LeafOpenResult::FailedSettled(nt_error(status))
            });
        }
        if call.handle.is_null() || call.handle == INVALID_HANDLE_VALUE {
            return Err(LeafOpenResult::Unsettled(
                crate::fs::LeafOpenUncertainty::retain(nt_error(status), RetainedLeafOpen(call)),
            ));
        }
        // SAFETY: NtCreateFile completed successfully and transfers this unique handle.
        let file = unsafe { File::from_raw_handle(call.handle) };
        validate(&file, false).map_err(LeafOpenResult::FailedSettled)?;
        // Required file identity must work before the handle is used as evidence.
        let _: FILE_ID_INFO = query(&file, FileIdInfo).map_err(LeafOpenResult::FailedSettled)?;
        Ok(file)
    })();
    match result {
        Ok(file) => LeafOpenResult::Opened(file),
        Err(result) => result,
    }
}

pub(in crate::fs) fn open_dir(parent: &File, name: &OsStr) -> io::Result<File> {
    relative(parent, name, true, false)
}
pub(in crate::fs) fn create_dir(parent: &File, name: &OsStr) -> io::Result<File> {
    relative(parent, name, true, true)
}
pub(in crate::fs) fn open_file(parent: &File, name: &OsStr, create: bool) -> io::Result<File> {
    relative(parent, name, false, create)
}
pub(in crate::fs) fn open_ledger(parent: &File, name: &OsStr) -> io::Result<File> {
    crate::fs::validate_name(name)?;
    // OBJ_INHERIT is deliberately absent from the shared relative-open attributes.
    relative_open(parent, name, false, false, true)
}
pub(in crate::fs) fn validate(file: &File, directory: bool) -> io::Result<()> {
    let info: FILE_ATTRIBUTE_TAG_INFO = query(file, FileAttributeTagInfo)?;
    if info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid("reparse points are not permitted"));
    }
    let standard: FILE_STANDARD_INFO = query(file, FileStandardInfo)?;
    if standard.Directory != directory {
        return Err(invalid("unexpected filesystem object type"));
    }
    // GetFileType is a type check, NOT our locality proof.
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK {
        return Err(invalid("expected a disk filesystem object"));
    }
    Ok(())
}

struct VolumeCall {
    status: IO_STATUS_BLOCK,
    device: FILE_FS_DEVICE_INFORMATION,
    file: File,
}
fn local_ntfs(file: &File) -> io::Result<()> {
    let mut call = Box::new(VolumeCall {
        status: unsafe { zeroed() },
        device: unsafe { zeroed() },
        file: file.try_clone()?,
    });
    let mut status = unsafe {
        nt::NtQueryVolumeInformationFile(
            call.file.as_raw_handle(),
            &mut call.status,
            (&mut call.device as *mut FILE_FS_DEVICE_INFORMATION).cast(),
            size_of::<FILE_FS_DEVICE_INFORMATION>() as u32,
            nt::FileFsDeviceInformation,
        )
    };
    if status == STATUS_PENDING {
        if unsafe { WaitForSingleObject(call.file.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
            std::mem::forget(call);
            return Err(io::Error::other(
                "pending volume query could not be settled; operation retained",
            ));
        }
        status = unsafe { call.status.Anonymous.Status };
    }
    if status == STATUS_PENDING {
        std::mem::forget(call);
        return Err(io::Error::other(
            "volume query remained pending; operation retained",
        ));
    }
    if status < 0 {
        return Err(nt_error(status));
    }
    let mut filesystem = [0u16; 32];
    let mut flags = 0;
    if unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            &mut flags,
            filesystem.as_mut_ptr(),
            filesystem.len() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let end = filesystem
        .iter()
        .position(|c| *c == 0)
        .ok_or_else(|| io::Error::other("unterminated filesystem name"))?;
    let identity: io::Result<FILE_ID_INFO> = query(file, FileIdInfo);
    supported_profile(
        call.device.Characteristics,
        flags,
        &filesystem[..end],
        identity.map(|_| ()),
    )
}
fn supported_profile(
    characteristics: u32,
    flags: u32,
    filesystem: &[u16],
    identity: io::Result<()>,
) -> io::Result<()> {
    if characteristics & FILE_REMOTE_DEVICE != 0
        || flags & FILE_PERSISTENT_ACLS == 0
        || filesystem != [78, 84, 70, 83]
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "local NTFS with persistent ACLs is required",
        ));
    }
    identity
}

pub(in crate::fs) fn entries(directory: &File) -> io::Result<Vec<OsString>> {
    // HostDir serializes this entire restart/continuation sequence on its retained
    // directory handle. No path or undocumented native self-name is re-opened.
    let mut storage = vec![0u64; 8192];
    let capacity = storage.len() * size_of::<u64>();
    let mut names = Vec::new();
    let mut class = FileIdBothDirectoryRestartInfo;
    loop {
        let ok = unsafe {
            GetFileInformationByHandleEx(
                directory.as_raw_handle(),
                class,
                storage.as_mut_ptr().cast(),
                capacity as u32,
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                break;
            }
            return Err(error);
        }
        class = FileIdBothDirectoryInfo;
        let mut offset = 0usize;
        loop {
            let header = offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
            if offset + size_of::<FILE_ID_BOTH_DIR_INFO>() > capacity {
                return Err(io::Error::other("invalid directory record"));
            }
            // SAFETY: record header is inside owned storage. read_unaligned avoids
            // relying on undocumented record alignment; names are copied as UTF-16.
            let record = unsafe {
                std::ptr::read_unaligned(
                    storage
                        .as_ptr()
                        .cast::<u8>()
                        .add(offset)
                        .cast::<FILE_ID_BOTH_DIR_INFO>(),
                )
            };
            let length = record.FileNameLength as usize;
            let end = offset
                .checked_add(header)
                .and_then(|n| n.checked_add(length))
                .filter(|n| *n <= capacity)
                .ok_or_else(|| io::Error::other("directory name exceeds buffer"))?;
            if !length.is_multiple_of(2) {
                return Err(io::Error::other("invalid UTF-16 name length"));
            }
            let mut name = Vec::with_capacity(length / 2);
            for at in (offset + header..end).step_by(2) {
                name.push(unsafe {
                    std::ptr::read_unaligned(storage.as_ptr().cast::<u8>().add(at).cast::<u16>())
                });
            }
            if name != [46] && name != [46, 46] {
                names.push(OsString::from_wide(&name));
            }
            if record.NextEntryOffset == 0 {
                break;
            }
            let next = record.NextEntryOffset as usize;
            if next < header + length {
                return Err(io::Error::other("overlapping directory records"));
            }
            offset = offset
                .checked_add(next)
                .filter(|n| *n < capacity)
                .ok_or_else(|| io::Error::other("invalid next directory record"))?;
        }
    }
    names.sort();
    Ok(names)
}
pub fn read_exact_at(file: &File, mut bytes: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        match file.seek_read(bytes, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => {
                offset = offset
                    .checked_add(count as u64)
                    .ok_or_else(|| invalid("offset overflow"))?;
                bytes = &mut bytes[count..];
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_open_call_storage_retention_model_not_native_pending_io() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let parent = open_root(temp.path(), RootPolicy::LeafNoFollow)?;
        let mut call = Box::new(OpenCall {
            handle: null_mut(),
            status: unsafe { zeroed() },
            name: wide(OsStr::new("reader"))?,
            unicode: unsafe { zeroed() },
            attributes: unsafe { zeroed() },
            security: None,
            parent,
        });
        call.status.Anonymous.Status = STATUS_PENDING;
        call.unicode = UNICODE_STRING {
            Length: 12,
            MaximumLength: 14,
            Buffer: call.name.as_mut_ptr(),
        };
        call.attributes.RootDirectory = call.parent.as_raw_handle();
        call.attributes.ObjectName = &call.unicode;
        let call_pointer = std::ptr::from_ref(&*call);
        let name_pointer = call.name.as_ptr();
        let parent_handle = call.parent.as_raw_handle();
        let error = nt_error(STATUS_PENDING);
        let original_error = error.raw_os_error();
        let propagated = io::Error::other(crate::fs::LeafOpenUncertainty::retain(
            error,
            RetainedLeafOpen(call),
        ));
        let pending = propagated
            .get_ref()
            .unwrap()
            .downcast_ref::<crate::fs::LeafOpenUncertainty>()
            .unwrap();
        assert_eq!(pending.error().raw_os_error(), original_error);
        let retained = pending
            ._retained
            .downcast_ref::<RetainedLeafOpen>()
            .unwrap();
        assert_eq!(std::ptr::from_ref(&*retained.0), call_pointer);
        assert_eq!(retained.0.name.as_ptr(), name_pointer);
        assert!(std::ptr::eq(
            retained.0.attributes.ObjectName,
            &retained.0.unicode
        ));
        assert_eq!(retained.0.attributes.RootDirectory, parent_handle);
        drop(propagated);
        // The retained original parent handle remains valid after incomplete Drop.
        assert_eq!(unsafe { GetFileType(parent_handle) }, FILE_TYPE_DISK);
        // No NtCreateFile was issued: supplemental ownership model only.
        Ok(())
    }
    #[test]
    fn unsupported_profile_and_identity_errors_fail_closed() {
        for (characteristics, flags, name) in [
            (
                FILE_REMOTE_DEVICE,
                FILE_PERSISTENT_ACLS,
                vec![78, 84, 70, 83],
            ),
            (0, 0, vec![78, 84, 70, 83]),
            (0, FILE_PERSISTENT_ACLS, vec![70, 65, 84]),
        ] {
            assert_eq!(
                supported_profile(characteristics, flags, &name, Ok(()))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::Unsupported
            );
        }
        assert_eq!(
            supported_profile(
                0,
                FILE_PERSISTENT_ACLS,
                &[78, 84, 70, 83],
                Err(io::Error::from_raw_os_error(50))
            )
            .unwrap_err()
            .raw_os_error(),
            Some(50)
        );
    }
}
