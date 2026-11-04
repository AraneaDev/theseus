//! Native no-follow artifact owners plus deny-write/delete pins for Shell's path
//! consumption. Every ancestor is held against rename before native parsing.
use super::{ArtifactCommitment, ShortcutProfile, invalid};
use crate::fs::{HostDir, LeafOpenResult, RootPolicy, fingerprint, read_exact_at};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs::File,
    io,
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle},
    path::{Path, PathBuf},
    ptr,
};
use windows_sys::Win32::{
    Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE},
    Storage::FileSystem::*,
};
#[derive(Debug)]
pub struct RetainedShortcut {
    pub(super) profile: ShortcutProfile,
    root: HostDir,
    entry: File,
    executable: File,
    _directories: Vec<File>,
}
fn wide(v: &str) -> Vec<u16> {
    v.encode_utf16().chain(Some(0)).collect()
}
fn query<T: Default>(file: &File, class: FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T> {
    let mut value = T::default();
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            class,
            (&mut value as *mut T).cast(),
            size_of::<T>() as u32,
        )
    } == 0
    {
        return Err(super::com::win32("GetFileInformationByHandleEx"));
    }
    Ok(value)
}
fn lock_directory(path: &str) -> io::Result<File> {
    let path = wide(path);
    let raw = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(super::com::win32("CreateFileW(directory pin)"));
    }
    let file = unsafe { File::from_raw_handle(raw) };
    let info: FILE_ATTRIBUTE_TAG_INFO = query(&file, FileAttributeTagInfo)?;
    if info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || info.FileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
    {
        return Err(invalid("directory pin is not original regular directory"));
    }
    Ok(file)
}
fn open_file(root: &HostDir, leaf: &str) -> io::Result<File> {
    match root.open_leaf_classified(OsStr::new(leaf)) {
        LeafOpenResult::Opened(file) => Ok(file),
        LeafOpenResult::MissingSettled => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "shortcut artifact absent",
        )),
        LeafOpenResult::FailedSettled(e) => Err(e),
        LeafOpenResult::Unsettled(owner) => Err(io::Error::other(owner)),
    }
}
fn pin(file: File) -> io::Result<File> {
    let original = fingerprint(&file)?;
    let raw = unsafe {
        ReOpenFile(
            file.as_raw_handle(),
            GENERIC_READ,
            FILE_SHARE_READ,
            FILE_FLAG_OPEN_REPARSE_POINT,
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(super::com::win32("ReOpenFile(deny write/delete)"));
    }
    let pinned = unsafe { File::from_raw_handle(raw) };
    if fingerprint(&pinned)? != original {
        return Err(invalid("artifact identity changed while pinning"));
    }
    Ok(pinned)
}
fn verify(file: &File, expected: &ArtifactCommitment) -> io::Result<()> {
    let before = fingerprint(file)?;
    if before.len != expected.length {
        return Err(invalid("shortcut artifact length differs"));
    }
    let mut sha = Sha256::new();
    let mut buffer = [0u8; 16384];
    let mut offset = 0;
    while offset < before.len {
        let n = (before.len - offset).min(buffer.len() as u64) as usize;
        read_exact_at(file, &mut buffer[..n], offset)?;
        sha.update(&buffer[..n]);
        offset += n as u64;
    }
    let actual: [u8; 32] = sha.finalize().into();
    if fingerprint(file)? != before || actual != expected.sha256 {
        return Err(invalid("shortcut artifact changed or digest differs"));
    }
    Ok(())
}
impl RetainedShortcut {
    pub fn open(profile: ShortcutProfile) -> io::Result<Self> {
        profile.validate()?;
        let root = HostDir::open(Path::new(&profile.root), RootPolicy::AllComponentsNoFollow)?;
        let mut directories = Vec::new();
        let mut path = profile.root[..3].to_owned();
        directories.push(lock_directory(&path)?);
        for component in profile.root[3..].split('\\') {
            if !path.ends_with('\\') {
                path.push('\\');
            }
            path.push_str(component);
            directories.push(lock_directory(&path)?);
        }
        let entry = pin(open_file(&root, &profile.entry)?)?;
        let executable = pin(open_file(&root, &profile.executable)?)?;
        let held = Self {
            profile,
            root,
            entry,
            executable,
            _directories: directories,
        };
        held.revalidate()?;
        Ok(held)
    }
    pub fn profile(&self) -> &ShortcutProfile {
        &self.profile
    }
    pub fn entry_path(&self) -> PathBuf {
        Path::new(&self.profile.root).join(&self.profile.entry)
    }
    pub fn executable_path(&self) -> PathBuf {
        Path::new(&self.profile.root).join(&self.profile.executable)
    }
    pub fn entry_bytes(&self) -> io::Result<Vec<u8>> {
        self.revalidate()?;
        let mut data = vec![0; self.profile.entry_artifact.length as usize];
        read_exact_at(&self.entry, &mut data, 0)?;
        Ok(data)
    }
    pub fn revalidate(&self) -> io::Result<()> {
        let now = HostDir::open(
            Path::new(&self.profile.root),
            RootPolicy::AllComponentsNoFollow,
        )?;
        if !self.root.same_directory_object(&now)? {
            return Err(invalid("original shortcut root replaced"));
        }
        for (name, held, commitment) in [
            (
                &self.profile.entry,
                &self.entry,
                &self.profile.entry_artifact,
            ),
            (
                &self.profile.executable,
                &self.executable,
                &self.profile.executable_artifact,
            ),
        ] {
            if fingerprint(&open_file(&now, name)?)? != fingerprint(held)? {
                return Err(invalid("original shortcut artifact replaced"));
            }
            verify(held, commitment)?;
        }
        Ok(())
    }
}
