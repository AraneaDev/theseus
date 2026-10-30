use super::{identity::query, security::PrivateSecurity};
use crate::fs::{RootPolicy, invalid};
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
    relative_open(parent, name, directory, create)
}
fn relative_open(parent: &File, name: &OsStr, directory: bool, create: bool) -> io::Result<File> {
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
pub(in crate::fs) fn open_dir(parent: &File, name: &OsStr) -> io::Result<File> {
    relative(parent, name, true, false)
}
pub(in crate::fs) fn create_dir(parent: &File, name: &OsStr) -> io::Result<File> {
    relative(parent, name, true, true)
}
pub(in crate::fs) fn open_file(parent: &File, name: &OsStr, create: bool) -> io::Result<File> {
    relative(parent, name, false, create)
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
    // A new relative '.' handle gets an independent enumeration cursor. A duplicated
    // handle shares a cursor and would race another entries() call.
    let directory = enumeration_handle(directory)?;
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
fn enumeration_handle(directory: &File) -> io::Result<File> {
    // Trusted internal self-component: never accepted from an untrusted reference.
    // NtCreateFile uses the pinned root, with an independent enumeration cursor.
    relative_open(directory, OsStr::new("."), true, false)
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
