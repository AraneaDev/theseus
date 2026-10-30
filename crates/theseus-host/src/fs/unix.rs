use super::RootPolicy;
use rustix::fs::{Dir, Mode, OFlags, mkdirat, open, openat};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    os::unix::{
        ffi::OsStringExt,
        fs::{FileExt, MetadataExt},
    },
    path::{Component, Path},
};
const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
pub(super) fn open_root(path: &Path, policy: RootPolicy) -> io::Result<File> {
    if matches!(policy, RootPolicy::LeafNoFollow) {
        let path: std::path::PathBuf = path.components().collect();
        return Ok(open(path, DIR_FLAGS, Mode::empty())?.into());
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut dir = File::from(open("/", DIR_FLAGS, Mode::empty())?);
    for component in absolute.components() {
        match component {
            Component::RootDir | Component::CurDir => (),
            Component::Normal(name) => dir = open_dir(&dir, name)?,
            _ => return Err(super::invalid("unsafe directory path")),
        }
    }
    Ok(dir)
}
// All components here are ancestors of the exclusively created leaf. Preserve
// the existing creation policy, which permits ancestor aliases.
pub(super) fn open_creation_parent(path: &Path) -> io::Result<File> {
    Ok(open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?
    .into())
}
pub(super) fn open_dir(parent: &File, name: &OsStr) -> io::Result<File> {
    Ok(openat(parent, name, DIR_FLAGS, Mode::empty())?.into())
}
pub(super) fn create_dir(parent: &File, name: &OsStr) -> io::Result<File> {
    mkdirat(parent, name, Mode::RUSR | Mode::WUSR | Mode::XUSR)?;
    parent.sync_all()?;
    open_dir(parent, name)
}
pub(super) fn open_file(parent: &File, name: &OsStr, create: bool) -> io::Result<File> {
    let access = if create {
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL
    } else {
        OFlags::RDONLY
    };
    let file = File::from(openat(
        parent,
        name,
        access | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?);
    if !file.metadata()?.is_file() {
        return Err(super::invalid("expected a regular file"));
    }
    Ok(file)
}
pub(super) fn entries(directory: &File) -> io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in Dir::read_from(directory)? {
        let entry = entry?;
        let bytes = entry.file_name().to_bytes();
        if bytes != b"." && bytes != b".." {
            names.push(OsString::from_vec(bytes.to_vec()));
        }
    }
    names.sort();
    Ok(names)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFingerprint {
    pub len: u64,
    device: u64,
    inode: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
pub fn fingerprint(file: &File) -> io::Result<FileFingerprint> {
    let m = file.metadata()?;
    if !m.is_file() {
        return Err(super::invalid("expected a regular file"));
    }
    Ok(FileFingerprint {
        len: m.len(),
        device: m.dev(),
        inode: m.ino(),
        modified: (m.mtime(), m.mtime_nsec()),
        changed: (m.ctime(), m.ctime_nsec()),
    })
}
pub fn read_exact_at(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<()> {
    file.read_exact_at(bytes, offset)
}

pub(super) fn flush_file(file: &File) -> io::Result<()> {
    file.sync_all()
}
pub(super) fn replace(
    parent: &File,
    temporary: &OsStr,
    _file: &File,
    name: &OsStr,
) -> io::Result<()> {
    Ok(rustix::fs::renameat(parent, temporary, parent, name)?)
}
pub(super) fn remove_temporary(parent: &File, name: &OsStr, _file: &File) -> io::Result<()> {
    Ok(rustix::fs::unlinkat(
        parent,
        name,
        rustix::fs::AtFlags::empty(),
    )?)
}

#[cfg(all(test, target_os = "linux"))]
mod directory_sync_tests {
    use super::*;
    #[test]
    fn child_creation_mkdir_precedes_parent_sync_failure_and_prevents_child_open() {
        let temp = tempfile::tempdir().unwrap();
        // A real O_PATH descriptor permits mkdirat, but fsync returns EBADF.
        // No child capability is returned after the entry barrier fails.
        let parent =
            File::from(open(temp.path(), OFlags::PATH | OFlags::DIRECTORY, Mode::empty()).unwrap());
        for name in ["source", "output-parent"] {
            let result = create_dir(&parent, OsStr::new(name));
            assert!(temp.path().join(name).is_dir(), "mkdir must precede fsync");
            assert_eq!(
                result.unwrap_err().raw_os_error(),
                Some(rustix::io::Errno::BADF.raw_os_error())
            );
        }
    }
}
