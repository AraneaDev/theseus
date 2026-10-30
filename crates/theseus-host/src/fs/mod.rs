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
    #[cfg(windows)]
    enumeration: std::sync::Mutex<()>,
}
impl HostDir {
    pub fn open(path: &Path, policy: RootPolicy) -> io::Result<Self> {
        Ok(Self {
            directory: platform::open_root(path, policy)?,
            display: path.to_owned(),
            #[cfg(windows)]
            enumeration: std::sync::Mutex::new(()),
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
            #[cfg(windows)]
            enumeration: std::sync::Mutex::new(()),
        })
    }
    pub fn open_child(&self, path: &RelativePath) -> io::Result<Self> {
        self.walk(path.components(), false)
    }
    /// Open one native directory component through retained authority.
    pub fn open_child_name(&self, name: &OsStr) -> io::Result<Self> {
        validate_name(name)?;
        Ok(Self {
            directory: platform::open_dir(&self.directory, name)?,
            display: self.display.join(name),
            #[cfg(windows)]
            enumeration: std::sync::Mutex::new(()),
        })
    }
    /// Sync this retained directory on Unix. Windows directory power-loss
    /// persistence is unsupported; file flush and atomic replace are separate.
    pub fn sync_directory(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.directory.sync_all()
        }
        #[cfg(windows)]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "directory persistence is unsupported on Windows",
            ))
        }
    }
    pub fn create_child(&self, name: &OsStr) -> io::Result<Self> {
        validate_name(name)?;
        Ok(Self {
            directory: platform::create_dir(&self.directory, name)?,
            display: self.display.join(name),
            #[cfg(windows)]
            enumeration: std::sync::Mutex::new(()),
        })
    }
    fn walk(&self, components: &[String], create: bool) -> io::Result<Self> {
        let mut dir = Self {
            directory: self.directory.try_clone()?,
            display: self.display.clone(),
            #[cfg(windows)]
            enumeration: std::sync::Mutex::new(()),
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
                #[cfg(windows)]
                enumeration: std::sync::Mutex::new(()),
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
        // Windows enumeration advances this retained handle's cursor. HostDir is
        // not Clone, and temporary internal clones are used only for child opens.
        #[cfg(windows)]
        let _cursor = self
            .enumeration
            .lock()
            .map_err(|_| io::Error::other("directory enumeration lock poisoned"))?;
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
        platform::validate_native_name(name)?;
    }
    Ok(())
}

/// The completed persistence barrier. Windows makes no directory-entry power-loss claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitScope {
    FileAndDirectorySynced,
    FileFlushedAtomicReplace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishStage {
    Reserve,
    Write,
    Flush,
    Replace,
    ParentSync,
}

/// Observed namespace outcome, independent of the operation's error stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicationState {
    NotReplaced,
    Replaced,
    /// Completion is unobserved. Replacement may still occur; rollback and
    /// completed temporary cleanup must not be inferred.
    Unknown,
}

/// Payload of publication's `io::Error`, available through `get_ref().downcast_ref()`.
/// A ParentSync failure means the new entry is already published; retry is not rollback.
#[derive(Debug)]
pub struct PublicationError {
    stage: PublishStage,
    state: PublicationState,
    source: io::Error,
    cleanup: Option<io::Error>,
}
impl PublicationError {
    pub fn stage(&self) -> PublishStage {
        self.stage
    }
    pub fn state(&self) -> PublicationState {
        self.state
    }
    /// Failure to remove the owned temporary, in addition to the primary failure.
    pub fn cleanup_error(&self) -> Option<&io::Error> {
        self.cleanup.as_ref()
    }
}
impl std::fmt::Display for PublicationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.state {
            PublicationState::Replaced => write!(
                f,
                "publication occurred but directory durability failed: {}",
                self.source
            )?,
            PublicationState::NotReplaced => write!(
                f,
                "publication failed before replacement at {:?}: {}",
                self.stage, self.source
            )?,
            PublicationState::Unknown => write!(
                f,
                "publication state UNKNOWN; replacement and cleanup are unconfirmed: {}",
                self.source
            )?,
        }
        if let Some(error) = &self.cleanup {
            write!(f, "; owned temporary cleanup failed: {error}")?;
        }
        Ok(())
    }
}
impl std::error::Error for PublicationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
// Only a platform operation that retains its complete unresolved owner may
// return this marker. It must never be treated as an ordinary rollback error.
#[derive(Debug)]
struct UnresolvedReplacement;
impl std::fmt::Display for UnresolvedReplacement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("native rename operation retained until process exit")
    }
}
impl std::error::Error for UnresolvedReplacement {}
fn publication_state(stage: PublishStage, source: &io::Error) -> PublicationState {
    if stage == PublishStage::Replace
        && source
            .get_ref()
            .is_some_and(|e| e.is::<UnresolvedReplacement>())
    {
        PublicationState::Unknown
    } else if stage == PublishStage::ParentSync {
        PublicationState::Replaced
    } else {
        PublicationState::NotReplaced
    }
}

fn publication_error(
    stage: PublishStage,
    source: io::Error,
    cleanup: Option<io::Error>,
) -> io::Error {
    io::Error::new(
        source.kind(),
        PublicationError {
            stage,
            state: publication_state(stage, &source),
            source,
            cleanup,
        },
    )
}
fn publication_step<T>(
    stage: PublishStage,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    #[cfg(test)]
    publication_tests::checkpoint(stage)?;
    #[cfg(not(test))]
    let _ = stage;
    operation()
}

struct PendingPublication<'a> {
    directory: &'a File,
    name: OsString,
    file: File,
    temporary: bool,
}
impl PendingPublication<'_> {
    fn cleanup(&mut self) -> io::Result<()> {
        if self.temporary {
            self.temporary = false;
            platform::remove_temporary(self.directory, &self.name, &self.file)?;
        }
        Ok(())
    }
}
impl Drop for PendingPublication<'_> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

impl HostDir {
    /// Reserve an exclusive private sibling, call the caller's bounded serializer,
    /// flush the completed file and atomically replace this single native basename.
    /// All errors carry `PublicationError`; inspect it before claiming rollback.
    pub fn publish(
        &self,
        name: &OsStr,
        write: &mut dyn FnMut(&mut File) -> io::Result<()>,
    ) -> io::Result<CommitScope> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let names = std::iter::repeat_with(|| {
            let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Numeric-only, valid 8.3 names avoid case aliases and a distinct
            // generated NTFS short name. Collisions remain exclusive-create errors.
            let value =
                serial.wrapping_add(u64::from(std::process::id()) * 1_000_000) % 100_000_000_000;
            OsString::from(format!("{:08}.{:03}", value / 1_000, value % 1_000))
        })
        .take(128);
        self.publish_with_names(name, write, names)
    }
    fn publish_with_names(
        &self,
        name: &OsStr,
        write: &mut dyn FnMut(&mut File) -> io::Result<()>,
        names: impl Iterator<Item = OsString>,
    ) -> io::Result<CommitScope> {
        let mut pending = publication_step(PublishStage::Reserve, || {
            validate_name(name)?;
            self.reserve_publication_names(name, names)
        })
        .map_err(|e| publication_error(PublishStage::Reserve, e, None))?;
        let result = (|| {
            publication_step(PublishStage::Write, || {
                // Keep the exclusive owned object as rename/flush/cleanup authority.
                // A callback may replace its mutable File slot; only its duplicate
                // is exposed. Cloning failure is still before replacement.
                let mut writer = pending.file.try_clone()?;
                write(&mut writer)
            })
            .map_err(|e| (PublishStage::Write, e))?;
            publication_step(PublishStage::Flush, || platform::flush_file(&pending.file))
                .map_err(|e| (PublishStage::Flush, e))?;
            publication_step(PublishStage::Replace, || {
                platform::replace(&self.directory, &pending.name, &pending.file, name)
            })
            .map_err(|e| (PublishStage::Replace, e))?;
            // Ownership of the temporary name ends exactly at successful replacement.
            pending.temporary = false;
            #[cfg(unix)]
            {
                publication_step(PublishStage::ParentSync, || self.directory.sync_all())
                    .map_err(|e| (PublishStage::ParentSync, e))?;
                Ok(CommitScope::FileAndDirectorySynced)
            }
            #[cfg(windows)]
            {
                Ok(CommitScope::FileFlushedAtomicReplace)
            }
        })();
        result.map_err(|(stage, source)| {
            if publication_state(stage, &source) == PublicationState::Unknown {
                // The native owner holds the source and directory. That source
                // may already have its final name: never dispose/unlink it here.
                pending.temporary = false;
            }
            let cleanup = pending.cleanup().err();
            publication_error(stage, source, cleanup)
        })
    }
    fn reserve_publication_names(
        &self,
        destination: &OsStr,
        names: impl Iterator<Item = OsString>,
    ) -> io::Result<PendingPublication<'_>> {
        // The candidate alphabet is ASCII digits and one dot. Normal NTFS/APFS
        // canonical/case rules cannot turn non-ASCII into those characters.
        // Dropping non-ASCII conservatively excludes aliases with ignorable
        // characters; this key never changes validation or the real destination.
        let destination_ascii: Vec<u8> = destination
            .as_encoded_bytes()
            .iter()
            .copied()
            .filter(u8::is_ascii)
            .collect();
        for name in names {
            if name.as_encoded_bytes() == destination_ascii {
                continue;
            }
            match platform::open_file(&self.directory, &name, true) {
                Ok(file) => {
                    return Ok(PendingPublication {
                        directory: &self.directory,
                        name,
                        file,
                        temporary: true,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not reserve private publication temporary",
        ))
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;
    use std::{
        cell::Cell,
        io::{Read, Write},
    };
    thread_local! {
        static FAIL: Cell<Option<PublishStage>> = const { Cell::new(None) };
        static UNRESOLVED: Cell<bool> = const { Cell::new(false) };
    }
    pub(super) fn checkpoint(stage: PublishStage) -> io::Result<()> {
        if stage == PublishStage::Replace && UNRESOLVED.get() {
            return Err(io::Error::other(UnresolvedReplacement));
        }
        if FAIL.get() == Some(stage) {
            Err(io::Error::other("injected publication stage failure"))
        } else {
            Ok(())
        }
    }
    fn content(root: &HostDir) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        root.open_file(&RelativePath::parse("report")?)?
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    fn failure(root: &HostDir, stage: PublishStage) -> io::Result<io::Error> {
        FAIL.set(Some(stage));
        let result = root.publish(OsStr::new("report"), &mut |f| {
            f.write_all(b"complete new bytes")
        });
        FAIL.set(None);
        Ok(result.unwrap_err())
    }
    #[test]
    fn unresolved_replacement_is_unknown_and_never_cleans_possible_renamed_source() -> io::Result<()>
    {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("run");
        let root = HostDir::create_private(&path)?;
        root.publish(OsStr::new("report"), &mut |f| f.write_all(b"old"))?;
        UNRESOLVED.set(true);
        let result = root.publish_with_names(
            OsStr::new("report"),
            &mut |f| f.write_all(b"complete new bytes"),
            [OsString::from("91234567.890")].into_iter(),
        );
        UNRESOLVED.set(false);
        let error = result.unwrap_err();
        let detail = error
            .get_ref()
            .unwrap()
            .downcast_ref::<PublicationError>()
            .unwrap();
        assert_eq!(detail.stage(), PublishStage::Replace);
        assert_eq!(detail.state(), PublicationState::Unknown);
        assert!(detail.cleanup_error().is_none());
        assert!(error.to_string().contains("UNKNOWN"));
        assert_eq!(root.entries()?, vec!["91234567.890", "report"]);
        assert_eq!(
            std::fs::read(path.join("91234567.890"))?,
            b"complete new bytes"
        );
        // This seam starts no kernel operation. It proves the wrapper does not
        // delete/dispose possible renamed authority; actual pending is unexercised.
        Ok(())
    }
    #[test]
    fn absent_destination_is_never_reserved_as_its_own_temporary() -> io::Result<()> {
        let destinations = [
            OsString::from("01234567.890"),
            OsString::from("0123\u{200d}4567.890"),
        ]
        .into_iter();
        #[cfg(all(unix, not(target_os = "macos")))]
        let destinations = {
            use std::os::unix::ffi::OsStringExt;
            destinations.chain([OsString::from_vec(b"0123\xff4567.890".to_vec())])
        };
        #[cfg(windows)]
        let destinations = {
            use std::os::windows::ffi::OsStringExt;
            destinations.chain([OsString::from_wide(&[
                48, 49, 50, 51, 0xd800, 52, 53, 54, 55, 46, 56, 57, 48,
            ])])
        };
        for destination in destinations {
            let temp = tempfile::tempdir()?;
            let path = temp.path().join("run");
            let root = HostDir::create_private(&path)?;
            let candidates = || {
                [
                    OsString::from("01234567.890"),
                    OsString::from("01234567.891"),
                ]
                .into_iter()
            };
            let error = root
                .publish_with_names(
                    &destination,
                    &mut |file| {
                        file.write_all(b"partial")?;
                        assert_eq!(
                            std::fs::metadata(path.join(&destination))
                                .unwrap_err()
                                .kind(),
                            io::ErrorKind::NotFound,
                            "destination became visible during writer callback"
                        );
                        assert_eq!(root.entries()?, vec!["01234567.891"]);
                        Err(io::Error::other("writer failed before commit"))
                    },
                    candidates(),
                )
                .unwrap_err();
            let detail = error
                .get_ref()
                .unwrap()
                .downcast_ref::<PublicationError>()
                .unwrap();
            assert_eq!(detail.stage(), PublishStage::Write);
            assert_eq!(detail.state(), PublicationState::NotReplaced);
            assert!(detail.cleanup_error().is_none());
            assert_eq!(
                std::fs::metadata(path.join(&destination))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::NotFound
            );
            assert!(root.entries()?.is_empty());
            root.publish_with_names(
                &destination,
                &mut |file| {
                    file.write_all(b"complete")?;
                    assert_eq!(
                        std::fs::metadata(path.join(&destination))
                            .unwrap_err()
                            .kind(),
                        io::ErrorKind::NotFound
                    );
                    Ok(())
                },
                candidates(),
            )?;
            assert_eq!(std::fs::read(path.join(&destination))?, b"complete");
            assert_eq!(root.entries()?.len(), 1);
        }
        Ok(())
    }
    #[test]
    fn reservation_collisions_never_delete_unowned_entries() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = HostDir::create_private(&temp.path().join("run"))?;
        root.create_file(&RelativePath::parse("collision")?)?
            .write_all(b"unowned")?;
        let error = root
            .reserve_publication_names(
                OsStr::new("report"),
                [OsString::from("collision")].into_iter(),
            )
            .err()
            .expect("collision must fail");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(temp.path().join("run/collision"))?,
            b"unowned"
        );
        {
            let mut reserved = root.reserve_publication_names(
                OsStr::new("report"),
                [OsString::from("collision"), OsString::from("owned")].into_iter(),
            )?;
            reserved.file.write_all(b"temporary")?;
            assert_eq!(root.entries()?, vec!["collision", "owned"]);
        }
        assert_eq!(root.entries()?, vec!["collision"]);
        assert_eq!(
            std::fs::read(temp.path().join("run/collision"))?,
            b"unowned"
        );
        Ok(())
    }
    #[test]
    fn injected_pre_replace_failures_preserve_old_and_remove_only_owned_temporary() -> io::Result<()>
    {
        let temp = tempfile::tempdir()?;
        let root = HostDir::create_private(&temp.path().join("run"))?;
        root.publish(OsStr::new("report"), &mut |f| f.write_all(b"old"))?;
        root.create_file(&RelativePath::parse("unrelated")?)?
            .write_all(b"leave alone")?;
        for stage in [
            PublishStage::Reserve,
            PublishStage::Write,
            PublishStage::Flush,
            PublishStage::Replace,
        ] {
            let error = failure(&root, stage)?;
            let detail = error
                .get_ref()
                .unwrap()
                .downcast_ref::<PublicationError>()
                .unwrap();
            assert_eq!(detail.stage(), stage);
            assert_eq!(detail.state(), PublicationState::NotReplaced);
            assert!(detail.cleanup_error().is_none());
            assert_eq!(content(&root)?, b"old");
            assert_eq!(root.entries()?, vec!["report", "unrelated"]);
            assert_eq!(
                std::fs::read(temp.path().join("run/unrelated"))?,
                b"leave alone"
            );
        }
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn parent_sync_failure_reports_publication_occurred_without_rollback() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = HostDir::create_private(&temp.path().join("run"))?;
        root.publish(OsStr::new("report"), &mut |f| f.write_all(b"old"))?;
        let error = failure(&root, PublishStage::ParentSync)?;
        let detail = error
            .get_ref()
            .unwrap()
            .downcast_ref::<PublicationError>()
            .unwrap();
        assert_eq!(detail.stage(), PublishStage::ParentSync);
        assert_eq!(detail.state(), PublicationState::Replaced);
        assert!(error.to_string().contains("publication occurred"));
        assert_eq!(content(&root)?, b"complete new bytes");
        assert_eq!(root.entries()?, vec!["report"]);
        Ok(())
    }
}
