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
impl FileFingerprint {
    /// Fixed native change-fingerprint encoding; integer fields are big-endian.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(49);
        bytes.push(2);
        bytes.extend_from_slice(&self.len.to_be_bytes());
        bytes.extend_from_slice(&self.volume.to_be_bytes());
        bytes.extend_from_slice(&self.id);
        bytes.extend_from_slice(&self.written.to_be_bytes());
        bytes.extend_from_slice(&self.changed.to_be_bytes());
        bytes
    }
}
pub(in crate::fs) fn same_directory(left: &File, right: &File) -> io::Result<bool> {
    super::open::validate(left, true)?;
    super::open::validate(right, true)?;
    let left: FILE_ID_INFO = query(left, FileIdInfo)?;
    let right: FILE_ID_INFO = query(right, FileIdInfo)?;
    Ok(left.VolumeSerialNumber == right.VolumeSerialNumber
        && left.FileId.Identifier == right.FileId.Identifier)
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

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn canonical_fingerprint_uses_fixed_big_endian_native_fields() {
        let value = FileFingerprint {
            len: 0x0102_0304_0506_0708,
            volume: 0x1112_1314_1516_1718,
            id: [
                0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d,
                0x2e, 0x2f,
            ],
            written: -2,
            changed: i64::MAX,
        };
        assert_eq!(
            value.canonical_bytes(),
            [
                2, 1, 2, 3, 4, 5, 6, 7, 8, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x20,
                0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e,
                0x2f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe, 0x7f, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0xff,
            ]
        );
    }

    #[test]
    fn regular_file_fingerprint_rejects_a_directory() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let directory = super::super::open_root(temp.path(), crate::fs::RootPolicy::LeafNoFollow)?;
        assert_eq!(
            fingerprint(&directory).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        Ok(())
    }
}
