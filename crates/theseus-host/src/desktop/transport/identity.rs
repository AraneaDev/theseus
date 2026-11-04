use crate::fs::HostDir;
use std::io;
/// A single no-follow directory open. Only terminal native absence is reported
/// as missing; wrong-kind, access and unresolved native failures remain errors.
pub enum DirectoryOpenResult {
    Opened(HostDir),
    MissingSettled,
    Unproved(io::Error),
}
pub fn open_child_directory_classified(
    parent: &HostDir,
    name: &std::ffi::OsStr,
) -> DirectoryOpenResult {
    match parent.open_child_name(name) {
        Ok(directory) => DirectoryOpenResult::Opened(directory),
        Err(error) => {
            #[cfg(unix)]
            let missing = error.raw_os_error() == Some(libc::ENOENT);
            #[cfg(windows)]
            let missing = matches!(error.raw_os_error(), Some(2 | 3));
            if missing {
                DirectoryOpenResult::MissingSettled
            } else {
                DirectoryOpenResult::Unproved(error)
            }
        }
    }
}
/// Stable observed directory identity for a retained root. Diagnostic binding
/// bytes never recreate its handle or authorize opening a replacement.
pub fn retained_directory_identity(directory: &HostDir) -> io::Result<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = directory.directory.metadata()?;
        if !metadata.is_dir() {
            return Err(io::Error::other("retained root is not a directory"));
        }
        let mut bytes = vec![1];
        bytes.extend_from_slice(&metadata.dev().to_be_bytes());
        bytes.extend_from_slice(&metadata.ino().to_be_bytes());
        Ok(bytes)
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
        };
        let mut info = unsafe { std::mem::zeroed::<FILE_ID_INFO>() };
        if unsafe {
            GetFileInformationByHandleEx(
                directory.directory.as_raw_handle(),
                FileIdInfo,
                (&mut info as *mut FILE_ID_INFO).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut bytes = vec![2];
        bytes.extend_from_slice(&info.VolumeSerialNumber.to_be_bytes());
        bytes.extend_from_slice(&info.FileId.Identifier);
        Ok(bytes)
    }
}
