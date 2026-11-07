//! Pinned directory authority. Paths retained here are diagnostic only.
mod ledger;
pub use ledger::LockedLedgerFile;
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

/// Cleanup through the original retained leaf, classified before errors erase phase.
#[derive(Debug)]
pub enum CreatedLeafRemovalResult {
    Removed,
    /// Only a terminal failure of the final synchronous deletion call.
    RetryableSettledDelete(io::Error),
    /// Name, mapping, open or identity authority was not proven; never auto-retry.
    StickyAuthorityFailure(io::Error),
    Unsettled(LeafOpenUncertainty),
}

/// Native single-leaf completion, classified before native status conversion.
#[derive(Debug)]
pub enum LeafOpenResult {
    Opened(File),
    MissingSettled,
    FailedSettled(io::Error),
    Unsettled(LeafOpenUncertainty),
}

/// Opaque original unresolved operation. Dropping it preserves both native
/// storage and the original error until process exit; completion is unproved.
pub struct LeafOpenUncertainty {
    error: std::mem::ManuallyDrop<io::Error>,
    _retained: std::mem::ManuallyDrop<Box<dyn std::any::Any + Send + Sync>>,
}
impl LeafOpenUncertainty {
    pub fn error(&self) -> &io::Error {
        &self.error
    }
    #[cfg(any(windows, test))]
    fn retain(error: io::Error, retained: impl std::any::Any + Send + Sync) -> Self {
        Self {
            error: std::mem::ManuallyDrop::new(error),
            _retained: std::mem::ManuallyDrop::new(Box::new(retained)),
        }
    }
}
impl std::fmt::Debug for LeafOpenUncertainty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeafOpenUncertainty")
            .field("error", self.error())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Display for LeafOpenUncertainty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error().fmt(f)
    }
}
impl std::error::Error for LeafOpenUncertainty {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error())
    }
}

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
    /// Compare retained native directory identities, independent of their paths.
    /// Failure to query or validate either directory is returned as an error.
    pub fn same_object(&self, other: &Self) -> io::Result<bool> {
        platform::same_directory(&self.directory, &other.directory)
    }
    /// Compare stable native directory identity, excluding mutable metadata.
    pub fn same_directory_object(&self, other: &Self) -> io::Result<bool> {
        platform::same_directory(&self.directory, &other.directory)
    }
    /// Open exactly one native file component through this retained directory.
    pub fn open_leaf_classified(&self, name: &OsStr) -> LeafOpenResult {
        if let Err(error) = validate_name(name) {
            return LeafOpenResult::FailedSettled(error);
        }
        platform::open_leaf_classified(&self.directory, name)
    }
    /// Classified open for read-stable inspection. On Windows the handle
    /// shares read and delete only, so while it is held no other handle can
    /// gain write access, and an existing writer makes the open fail settled
    /// (`FailedSettled`, sharing violation). Rename/replacement stays possible
    /// and must be detected through file identity. POSIX has no mandatory
    /// deny-write primitive; there this is exactly `open_leaf_classified`.
    pub fn open_leaf_deny_write_classified(&self, name: &OsStr) -> LeafOpenResult {
        if let Err(error) = validate_name(name) {
            return LeafOpenResult::FailedSettled(error);
        }
        #[cfg(windows)]
        {
            platform::open_leaf_deny_write_classified(&self.directory, name)
        }
        #[cfg(not(windows))]
        {
            platform::open_leaf_classified(&self.directory, name)
        }
    }
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
    /// Remove an exclusively created regular leaf using its retained directory
    /// and original file handle. The caller must own this private namespace and
    /// keep the handle until removal returns; this is not an atomic conditional
    /// unlink against a concurrent namespace writer. Never follows a replacement.
    pub fn remove_created_leaf(&self, name: &OsStr, file: &File) -> io::Result<()> {
        match self.remove_created_leaf_classified(name, file) {
            CreatedLeafRemovalResult::Removed => Ok(()),
            CreatedLeafRemovalResult::RetryableSettledDelete(error)
            | CreatedLeafRemovalResult::StickyAuthorityFailure(error) => Err(error),
            CreatedLeafRemovalResult::Unsettled(owner) => Err(io::Error::other(owner)),
        }
    }
    /// A settled final deletion error may retry this exact original authority.
    /// Missing/replaced/unproved mappings and unresolved native calls remain sticky.
    pub fn remove_created_leaf_classified(
        &self,
        name: &OsStr,
        file: &File,
    ) -> CreatedLeafRemovalResult {
        use CreatedLeafRemovalResult as R;
        if let Err(error) = validate_name(name) {
            return R::StickyAuthorityFailure(error);
        }
        // Windows FileDispositionInfo requires DELETE access, which a retained
        // read-only (e.g. classified-open) handle lacks. Delete through this
        // fresh name-bound handle only after it proves the retained identity.
        #[cfg(windows)]
        let current = platform::open_leaf_for_delete_classified(&self.directory, name);
        #[cfg(not(windows))]
        let current = self.open_leaf_classified(name);
        let current = match current {
            LeafOpenResult::Opened(current) => current,
            LeafOpenResult::MissingSettled => {
                return R::StickyAuthorityFailure(io::Error::new(
                    io::ErrorKind::NotFound,
                    "created leaf mapping is absent; retained object was not removed",
                ));
            }
            LeafOpenResult::FailedSettled(error) => return R::StickyAuthorityFailure(error),
            LeafOpenResult::Unsettled(owner) => return R::Unsettled(owner),
        };
        let original = match fingerprint(file) {
            Ok(f) => f,
            Err(error) => return R::StickyAuthorityFailure(error),
        };
        let observed = match fingerprint(&current) {
            Ok(f) => f,
            Err(error) => return R::StickyAuthorityFailure(error),
        };
        if original != observed {
            return R::StickyAuthorityFailure(io::Error::new(
                io::ErrorKind::InvalidInput,
                "created leaf no longer identifies the retained file",
            ));
        }
        // Both platform calls are synchronous and retain no operation storage:
        // Unix unlinkat; Windows POSIX-semantics FileDispositionInfoEx (legacy
        // FileDispositionInfo only where the file system lacks it). An
        // image-mapped (running) executable stays a settled retryable refusal.
        // on the identity-proven DELETE handle.
        #[cfg(windows)]
        let target = &current;
        #[cfg(not(windows))]
        let target = file;
        match platform::remove_temporary(&self.directory, name, target) {
            Ok(()) => R::Removed,
            Err(error) => R::RetryableSettledDelete(error),
        }
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
/// Synthetic typed publication classification fixture; starts no native operation.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub fn publication_unknown_fixture_error() -> io::Error {
    publication_error(
        PublishStage::Replace,
        io::Error::other(UnresolvedReplacement),
        None,
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

#[cfg(test)]
mod directory_identity_tests {
    use super::*;

    #[test]
    fn comparison_rejects_regular_file_authority_in_either_position() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let directory = HostDir::create_private(&temp.path().join("directory"))?;
        let file = HostDir {
            directory: File::create(temp.path().join("file"))?,
            display: temp.path().join("directory"),
            #[cfg(windows)]
            enumeration: std::sync::Mutex::new(()),
        };
        for (left, right) in [(&directory, &file), (&file, &directory)] {
            assert_eq!(
                left.same_object(right).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod leaf_completion_tests {
    use super::*;
    #[test]
    fn native_missing_leaf_is_settled_absence() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = HostDir::open(temp.path(), RootPolicy::LeafNoFollow)?;
        assert!(matches!(
            root.open_leaf_classified(OsStr::new("missing")),
            LeafOpenResult::MissingSettled
        ));
        Ok(())
    }
    #[test]
    fn native_present_nonregular_and_invalid_names() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        std::fs::write(temp.path().join("reader"), b"native reader")?;
        std::fs::create_dir(temp.path().join("directory"))?;
        let root = HostDir::open(temp.path(), RootPolicy::LeafNoFollow)?;
        match root.open_leaf_classified(OsStr::new("reader")) {
            LeafOpenResult::Opened(mut file) => {
                use std::io::Read;
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                assert_eq!(bytes, b"native reader");
            }
            other => panic!("expected present regular file: {other:?}"),
        }
        for name in [
            "directory",
            "",
            ".",
            "..",
            "a/b",
            "a/../reader",
            "reader/",
            "a\0b",
        ] {
            assert!(
                matches!(
                    root.open_leaf_classified(OsStr::new(name)),
                    LeafOpenResult::FailedSettled(_)
                ),
                "{name:?}"
            );
        }
        Ok(())
    }
    #[test]
    fn native_directory_identity_survives_changes_and_rename_but_not_replacement() -> io::Result<()>
    {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("prefix");
        std::fs::create_dir(&path)?;
        let held = HostDir::open(&path, RootPolicy::LeafNoFollow)?;
        let other = HostDir::open(&path, RootPolicy::LeafNoFollow)?;
        std::fs::write(path.join("new-entry"), b"metadata changes")?;
        assert!(held.same_directory_object(&other)?);
        let renamed = temp.path().join("renamed");
        std::fs::rename(&path, &renamed)?;
        assert!(held.same_directory_object(&HostDir::open(&renamed, RootPolicy::LeafNoFollow)?)?);
        std::fs::create_dir(&path)?;
        assert!(!held.same_directory_object(&HostDir::open(&path, RootPolicy::LeafNoFollow)?)?);
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn native_symlink_and_permission_failure_are_never_absence() -> io::Result<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir()?;
        std::fs::write(temp.path().join("denied"), b"private")?;
        symlink("absent-target", temp.path().join("link"))?;
        let root = HostDir::open(temp.path(), RootPolicy::LeafNoFollow)?;
        assert!(matches!(
            root.open_leaf_classified(OsStr::new("link")),
            LeafOpenResult::FailedSettled(_)
        ));
        std::fs::set_permissions(
            temp.path().join("denied"),
            std::fs::Permissions::from_mode(0o0),
        )?;
        if unsafe { libc::geteuid() } != 0 {
            match root.open_leaf_classified(OsStr::new("denied")) {
                LeafOpenResult::FailedSettled(error) => {
                    assert_eq!(error.raw_os_error(), Some(libc::EACCES))
                }
                other => panic!("permission failure must be settled failure: {other:?}"),
            }
        }
        Ok(())
    }
    #[test]
    fn pending_retention_model_preserves_original_call_root_storage_and_error_on_propagation_and_drop()
    -> io::Result<()> {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        #[derive(Debug)]
        struct TrackedError(Arc<AtomicUsize>);
        impl std::fmt::Display for TrackedError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("original model error")
            }
        }
        impl std::error::Error for TrackedError {}
        impl Drop for TrackedError {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        struct ModelCall {
            _root: File,
            storage: Vec<u8>,
            drops: Arc<AtomicUsize>,
        }
        impl Drop for ModelCall {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::SeqCst);
            }
        }
        let temp = tempfile::tempdir()?;
        let root = HostDir::open(temp.path(), RootPolicy::LeafNoFollow)?;
        let call_drops = Arc::new(AtomicUsize::new(0));
        let error_drops = Arc::new(AtomicUsize::new(0));
        let call = ModelCall {
            _root: root.directory.try_clone()?,
            storage: vec![17; 32],
            drops: call_drops.clone(),
        };
        let original_storage = call.storage.as_ptr();
        let error = io::Error::other(TrackedError(error_drops.clone()));
        let original_error = error.get_ref().unwrap() as *const dyn std::error::Error;
        let pending = LeafOpenUncertainty::retain(error, call);
        let propagated = io::Error::other(pending);
        let uncertainty = propagated
            .get_ref()
            .unwrap()
            .downcast_ref::<LeafOpenUncertainty>()
            .unwrap();
        assert!(std::ptr::addr_eq(
            uncertainty.error().get_ref().unwrap(),
            original_error
        ));
        let retained = uncertainty._retained.downcast_ref::<ModelCall>().unwrap();
        assert_eq!(retained.storage.as_ptr(), original_storage);
        assert!(retained._root.metadata()?.is_dir());
        drop(root);
        drop(propagated);
        assert_eq!(call_drops.load(Ordering::SeqCst), 0);
        assert_eq!(error_drops.load(Ordering::SeqCst), 0);
        // Synthetic ownership model only: no Windows operation was issued.
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn native_parent_failure_is_not_leaf_absence_and_identity_errors_propagate() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("regular");
        std::fs::write(&path, b"not a directory")?;
        // Deliberately invalid authority fixture: exercise the actual ENOTDIR
        // syscall, rather than manufacturing an io::ErrorKind::NotFound.
        let invalid_parent = HostDir {
            directory: File::open(&path)?,
            display: path,
        };
        match invalid_parent.open_leaf_classified(OsStr::new("missing")) {
            LeafOpenResult::FailedSettled(error) => {
                assert_eq!(error.raw_os_error(), Some(libc::ENOTDIR))
            }
            other => panic!("parent failure cannot prove leaf absence: {other:?}"),
        }
        let valid = HostDir::open(temp.path(), RootPolicy::LeafNoFollow)?;
        assert!(invalid_parent.same_directory_object(&valid).is_err());
        Ok(())
    }
}

#[cfg(test)]
mod owned_leaf_cleanup_tests {
    use super::*;
    use std::io::Write;

    // POSIX permits renaming a directory while a created leaf beneath it is
    // open, so cleanup must follow the retained identity, not the pathname.
    // Windows refuses that rename (see the cfg(windows) counterpart below).
    #[cfg(unix)]
    #[test]
    fn retained_leaf_cleanup_survives_root_rename_without_touching_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("auth");
        let moved = temp.path().join("moved-auth");
        let root = HostDir::create_private(&original).unwrap();
        let mut file = root
            .create_file(&RelativePath::parse("identity").unwrap())
            .unwrap();
        file.write_all(b"inert test material").unwrap();
        file.sync_all().unwrap();
        std::fs::rename(&original, &moved).unwrap();
        let replacement = HostDir::create_private(&original).unwrap();
        let mut other = replacement
            .create_file(&RelativePath::parse("identity").unwrap())
            .unwrap();
        other.write_all(b"unrelated").unwrap();
        other.sync_all().unwrap();
        root.remove_created_leaf(OsStr::new("identity"), &file)
            .unwrap();
        drop(file);
        assert!(!moved.join("identity").exists());
        assert_eq!(
            std::fs::read(original.join("identity")).unwrap(),
            b"unrelated"
        );
    }

    // Windows (NTFS) refuses to rename a directory while any handle to an
    // object beneath it is open, even with FILE_SHARE_DELETE, so a retained
    // created leaf pins its root's pathname. Cleanup then removes exactly the
    // retained leaf under the unmoved root.
    #[cfg(windows)]
    #[test]
    fn retained_leaf_pins_root_against_rename_and_cleanup_removes_only_it() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("auth");
        let moved = temp.path().join("moved-auth");
        let root = HostDir::create_private(&original).unwrap();
        let mut file = root
            .create_file(&RelativePath::parse("identity").unwrap())
            .unwrap();
        file.write_all(b"inert test material").unwrap();
        file.sync_all().unwrap();
        let mut sibling = root
            .create_file(&RelativePath::parse("sibling").unwrap())
            .unwrap();
        sibling.write_all(b"unrelated").unwrap();
        sibling.sync_all().unwrap();
        drop(sibling);
        let refused = std::fs::rename(&original, &moved).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(!moved.exists());
        root.remove_created_leaf(OsStr::new("identity"), &file)
            .unwrap();
        drop(file);
        assert!(!original.join("identity").exists());
        assert_eq!(
            std::fs::read(original.join("sibling")).unwrap(),
            b"unrelated"
        );
    }

    #[test]
    fn retained_leaf_cleanup_refuses_another_object_even_with_equal_content() {
        let temp = tempfile::tempdir().unwrap();
        let root = HostDir::create_private(&temp.path().join("auth")).unwrap();
        let first = root
            .create_file(&RelativePath::parse("first").unwrap())
            .unwrap();
        let second = root
            .create_file(&RelativePath::parse("second").unwrap())
            .unwrap();
        assert!(
            root.remove_created_leaf(OsStr::new("second"), &first)
                .is_err()
        );
        assert!(temp.path().join("auth/first").exists());
        assert!(temp.path().join("auth/second").exists());
        root.remove_created_leaf(OsStr::new("second"), &second)
            .unwrap();
        drop(second);
        root.remove_created_leaf(OsStr::new("first"), &first)
            .unwrap();
        drop(first);
        assert!(root.entries().unwrap().is_empty());
    }
}

#[cfg(all(test, unix))]
mod classified_created_leaf_cleanup_tests {
    use super::*;
    use std::{io::Write, os::unix::fs::PermissionsExt};
    #[test]
    fn only_final_synchronous_delete_error_is_retryable_after_original_identity_proof() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("auth");
        let root = HostDir::create_private(&path).unwrap();
        let mut file = root
            .create_file(&RelativePath::parse("identity").unwrap())
            .unwrap();
        file.write_all(b"synthetic").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = root.remove_created_leaf_classified(OsStr::new("identity"), &file);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        match result {
            CreatedLeafRemovalResult::RetryableSettledDelete(error) => {
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied)
            }
            other => panic!("expected actual settled unlink denial, got {other:?}"),
        }
        assert!(matches!(
            root.remove_created_leaf_classified(OsStr::new("identity"), &file),
            CreatedLeafRemovalResult::Removed
        ));
        assert!(
            matches!(
                root.remove_created_leaf_classified(OsStr::new("identity"), &file),
                CreatedLeafRemovalResult::StickyAuthorityFailure(_)
            ),
            "missing mapping cannot become a deletion receipt"
        );
    }
}

#[cfg(all(test, windows))]
mod deny_write_inspection_tests {
    use super::*;
    #[test]
    fn removal_unlinks_the_name_while_another_owner_keeps_the_object_open() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("reader.exe");
        std::fs::write(&path, b"original").unwrap();
        let root = HostDir::open(temp.path(), RootPolicy::LeafNoFollow).unwrap();
        let LeafOpenResult::Opened(owner) = root.open_leaf_classified(OsStr::new("reader.exe"))
        else {
            panic!("owner open failed")
        };
        // Another owner (like an original broker's retained definition)
        // still holds the object while the SUT removes and reinstalls it.
        let LeafOpenResult::Opened(other) = root.open_leaf_classified(OsStr::new("reader.exe"))
        else {
            panic!("second owner open failed")
        };
        root.remove_created_leaf(OsStr::new("reader.exe"), &owner)
            .unwrap();
        assert!(matches!(
            root.open_leaf_classified(OsStr::new("reader.exe")),
            LeafOpenResult::MissingSettled
        ));
        std::fs::write(&path, b"replacement").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
        drop((owner, other));
    }
    #[test]
    fn retained_read_handle_removal_deletes_only_the_identity_proven_leaf() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("leaf"), b"owned").unwrap();
        std::fs::write(temp.path().join("other"), b"owned").unwrap();
        let root = HostDir::open(temp.path(), RootPolicy::LeafNoFollow).unwrap();
        // A classified open has no DELETE access; removal must still work
        // through the identity-proven DELETE handle, and only for that object.
        let LeafOpenResult::Opened(leaf) = root.open_leaf_classified(OsStr::new("leaf")) else {
            panic!("leaf open failed")
        };
        assert!(
            root.remove_created_leaf(OsStr::new("other"), &leaf)
                .is_err()
        );
        assert!(temp.path().join("other").exists());
        root.remove_created_leaf(OsStr::new("leaf"), &leaf).unwrap();
        drop(leaf);
        assert!(!temp.path().join("leaf").exists());
        assert!(temp.path().join("other").exists());
    }
    const ERROR_SHARING_VIOLATION: i32 = 32;
    #[test]
    fn deny_write_open_refuses_writers_settled_and_keeps_default_open_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("leaf");
        std::fs::write(&path, b"original").unwrap();
        let root = HostDir::open(temp.path(), RootPolicy::LeafNoFollow).unwrap();
        // An existing writer makes the inspection open a settled refusal.
        let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        match root.open_leaf_deny_write_classified(OsStr::new("leaf")) {
            LeafOpenResult::FailedSettled(error) => {
                assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION))
            }
            other => panic!("existing writer must refuse settled, got {other:?}"),
        }
        // The default classified open still shares write with that writer.
        assert!(matches!(
            root.open_leaf_classified(OsStr::new("leaf")),
            LeafOpenResult::Opened(_)
        ));
        drop(writer);
        let held = match root.open_leaf_deny_write_classified(OsStr::new("leaf")) {
            LeafOpenResult::Opened(file) => file,
            other => panic!("inspection open failed: {other:?}"),
        };
        let refused = std::fs::write(&path, b"modified").unwrap_err();
        assert_eq!(refused.raw_os_error(), Some(ERROR_SHARING_VIOLATION));
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        // A second deny-write reader is compatible; rename stays possible.
        assert!(matches!(
            root.open_leaf_deny_write_classified(OsStr::new("leaf")),
            LeafOpenResult::Opened(_)
        ));
        std::fs::rename(&path, temp.path().join("moved")).unwrap();
        drop(held);
        std::fs::write(temp.path().join("moved"), b"modified").unwrap();
    }
}
