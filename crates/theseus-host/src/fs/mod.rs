//! Pinned directory authority. Paths retained here are diagnostic only.
mod path;
pub use path::RelativePath;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;

pub use platform::{FileFingerprint, fingerprint, read_exact_at};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    path::{Path, PathBuf},
};

/// Controls only initial root acquisition. Descendants always reject links.
#[derive(Clone, Copy, Debug)]
pub enum RootPolicy {
    /// Permit aliases in ancestors, but never follow the root leaf.
    LeafNoFollow,
    /// Walk each component from the filesystem root without following aliases.
    AllComponentsNoFollow,
}

#[derive(Debug)]
pub struct HostDir {
    pub(super) directory: File,
    display: PathBuf,
}
impl HostDir {
    pub fn open(path: &Path, policy: RootPolicy) -> io::Result<Self> {
        Ok(Self {
            directory: platform::open_root(path, policy)?,
            display: path.to_owned(),
        })
    }
    /// Create missing private parents, accepting existing ancestor aliases, and
    /// exclusively create the leaf. Existing directories are never re-permissioned.
    pub fn create_private(path: &Path) -> io::Result<Self> {
        let name = path
            .file_name()
            .ok_or_else(|| invalid("expected directory leaf"))?;
        validate_name(name)?;
        let parent_path = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = match Self::open_creation_parent(parent_path) {
            Ok(dir) => dir,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match Self::create_private(parent_path) {
                    Ok(dir) => dir,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        Self::open_creation_parent(parent_path)?
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        parent.create_child(name)
    }
    fn open_creation_parent(path: &Path) -> io::Result<Self> {
        Ok(Self {
            directory: platform::open_creation_parent(path)?,
            display: path.to_owned(),
        })
    }
    pub fn open_child(&self, path: &RelativePath) -> io::Result<Self> {
        self.walk(path.components(), false)
    }
    pub fn create_child(&self, name: &OsStr) -> io::Result<Self> {
        validate_name(name)?;
        Ok(Self {
            directory: platform::create_dir(&self.directory, name)?,
            display: self.display.join(name),
        })
    }
    fn walk(&self, components: &[String], create: bool) -> io::Result<Self> {
        let mut dir = Self {
            directory: self.directory.try_clone()?,
            display: self.display.clone(),
        };
        for component in components {
            let name = OsStr::new(component);
            let directory = if create {
                match platform::create_dir(&dir.directory, name) {
                    Ok(file) => file,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        platform::open_dir(&dir.directory, name)?
                    }
                    Err(error) => return Err(error),
                }
            } else {
                platform::open_dir(&dir.directory, name)?
            };
            dir = Self {
                directory,
                display: dir.display.join(name),
            };
        }
        Ok(dir)
    }
    pub fn open_file(&self, path: &RelativePath) -> io::Result<File> {
        self.file(path, false)
    }
    pub fn create_file(&self, path: &RelativePath) -> io::Result<File> {
        self.file(path, true)
    }
    fn file(&self, path: &RelativePath, create: bool) -> io::Result<File> {
        let (name, parents) = path
            .components()
            .split_last()
            .expect("validated nonempty path");
        let parent = self.walk(parents, create)?;
        platform::open_file(&parent.directory, OsStr::new(name), create)
    }
    /// Enumerate the retained object, independent of its former pathname.
    pub fn entries(&self) -> io::Result<Vec<OsString>> {
        platform::entries(&self.directory)
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn validate_name(name: &OsStr) -> io::Result<()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(part)) if part == name)
        || components.next().is_some()
    {
        return Err(invalid("expected one native component"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        if name.as_bytes().contains(&0) {
            return Err(invalid("NUL in component"));
        }
    }
    #[cfg(windows)]
    {
        let text = name
            .to_str()
            .ok_or_else(|| invalid("non-Unicode Windows component"))?;
        RelativePath::parse(text)?;
    }
    Ok(())
}
