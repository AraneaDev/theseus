use std::{fs::File, io, mem::size_of, os::windows::io::AsRawHandle};
use windows_sys::Win32::Storage::FileSystem::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFingerprint {
    pub len: u64,
    volume: u64,
    id: [u8; 16],
    written: i64,
    changed: i64,
}
/// Metadata is queried from the retained object; timestamps never stand in for identity.
pub fn fingerprint(file: &File) -> io::Result<FileFingerprint> {
    super::open::validate(file, false)?;
    let identity: FILE_ID_INFO = query(file, FileIdInfo)?;
    let basic: FILE_BASIC_INFO = query(file, FileBasicInfo)?;
    let standard: FILE_STANDARD_INFO = query(file, FileStandardInfo)?;
    Ok(FileFingerprint {
        len: u64::try_from(standard.EndOfFile)
            .map_err(|_| io::Error::other("negative file size"))?,
        volume: identity.VolumeSerialNumber,
        id: identity.FileId.Identifier,
        written: basic.LastWriteTime,
        changed: basic.ChangeTime,
    })
}
// Sealed call sites select only matching fixed-size Win32 output types.
pub(super) fn query<T: Default>(file: &File, class: FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T> {
    let mut output = T::default();
    // SAFETY: callers use the exact fixed-size type for the class. The synchronous
    // API writes at most the supplied size and does not retain the pointer.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            class,
            (&mut output as *mut T).cast(),
            size_of::<T>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(output)
}
