//! Bounded leaf I/O through retained directory and file authority.
use sha2::{Digest as _, Sha256};
use std::{
    any::Any,
    fs::File,
    io::{self, Read, Seek, Write},
};
use theseus_host::fs::{HostDir, RelativePath, fingerprint};
use theseus_native_protocol::v2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileStage {
    FingerprintBefore,
    Read,
    Write,
    FileSync,
    Readback,
    FingerprintAfter,
    #[cfg(unix)]
    ParentSync,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileFailureKind {
    OpenUnclassified,
    IoAfterOpen,
    Changed,
    ReadbackMismatch,
}
#[derive(Debug)]
pub(crate) enum FileFailure {
    OpenUnclassified(io::Error),
    IoAfterOpen { stage: FileStage, error: io::Error },
    Changed,
    ReadbackMismatch,
}
#[derive(Debug, Default)]
pub(crate) struct FileOwner {
    pub(crate) handle: Option<File>,
    pub(crate) attempted: bool,
    pub(crate) opening: bool,
    pub(crate) primary: Option<FileFailure>,
    pub(crate) panic: Option<Box<dyn Any + Send>>,
    pub(crate) ledger_error: Option<theseus_native_helper_store::StoreError>,
    pub(crate) cleanup_error: Option<io::Error>,
    pub(crate) sticky_open: bool,
    #[cfg(feature = "test-support")]
    pub(crate) fixture: Option<crate::test_support::FixtureControl>,
    // Both acquired handles remain worker-owned through fixture identity checks
    // and the actual failing write, including every unwind/error path.
    #[cfg(feature = "test-support")]
    pub(crate) fixture_reopened: Option<File>,
}
#[derive(Debug)]
pub(crate) struct ObservedFile {
    pub(crate) bytes: v2::FileBytes,
    pub(crate) digest: v2::Digest,
}
#[derive(Debug)]
pub(crate) struct CreatedFile {
    pub(crate) length: u16,
    pub(crate) digest: v2::Digest,
}
impl FileFailure {
    fn kind(&self) -> FileFailureKind {
        match self {
            Self::OpenUnclassified(_) => FileFailureKind::OpenUnclassified,
            Self::IoAfterOpen { .. } => FileFailureKind::IoAfterOpen,
            Self::Changed => FileFailureKind::Changed,
            Self::ReadbackMismatch => FileFailureKind::ReadbackMismatch,
        }
    }
}
impl std::fmt::Display for FileFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenUnclassified(error) => {
                write!(f, "file open completion unclassified: {error}")
            }
            Self::IoAfterOpen { stage, error } => write!(f, "file I/O at {stage:?}: {error}"),
            Self::Changed => f.write_str("file changed during observation"),
            Self::ReadbackMismatch => f.write_str("created file readback differed"),
        }
    }
}
impl std::error::Error for FileFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::OpenUnclassified(error) | Self::IoAfterOpen { error, .. } => Some(error),
            Self::Changed | Self::ReadbackMismatch => None,
        }
    }
}
impl FileOwner {
    pub(crate) fn new() -> Self {
        Self::default()
    }
    fn fail(&mut self, failure: FileFailure) -> FileFailureKind {
        // Every caller stops at its first failure, so no later native call can
        // replace the owned primary source. Re-entry returns that same failure.
        if self.primary.is_none() {
            self.primary = Some(failure);
        }
        self.primary.as_ref().expect("stored primary").kind()
    }
}
fn open(
    owner: &mut FileOwner,
    root: &HostDir,
    path: &v2::LeafName,
    create: bool,
) -> Result<(), FileFailureKind> {
    if let Some(error) = &owner.primary {
        return Err(error.kind());
    }
    if owner.sticky_open || owner.opening {
        return Err(FileFailureKind::OpenUnclassified);
    }
    // A request owner is single-use; never discard a previously acquired handle.
    if owner.attempted {
        return Err(owner.fail(FileFailure::Changed));
    }
    let relative =
        RelativePath::parse(path.as_str()).expect("portable LeafName is one native component");
    owner.attempted = true;
    owner.opening = true;
    #[cfg(feature = "test-support")]
    if create && let Some(control) = &owner.fixture {
        control.mark_file_effect_attempted();
    }
    #[cfg(test)]
    tests::record_open();
    let result = if create {
        root.create_file(&relative)
    } else {
        root.open_file(&relative)
    };
    match result {
        Ok(file) => {
            // Store authority before any metadata, I/O or fixture checkpoint.
            owner.handle = Some(file);
            owner.opening = false;
            Ok(())
        }
        Err(error) => {
            owner.sticky_open = true;
            let kind = owner.fail(FileFailure::OpenUnclassified(error));
            owner.opening = false;
            Err(kind)
        }
    }
}
fn io_step<T>(
    owner: &mut FileOwner,
    stage: FileStage,
    operation: impl FnOnce(&mut File) -> io::Result<T>,
) -> Result<T, FileFailureKind> {
    let file = owner
        .handle
        .as_mut()
        .expect("successful retained file open");
    #[cfg(test)]
    let result = tests::checkpoint(stage, file).and_then(|()| operation(file));
    #[cfg(not(test))]
    let result = operation(file);
    result.map_err(|error| owner.fail(FileFailure::IoAfterOpen { stage, error }))
}
fn content(owner: &mut FileOwner, stage: FileStage) -> Result<ObservedFile, FileFailureKind> {
    let before = io_step(owner, FileStage::FingerprintBefore, |file| {
        fingerprint(file)
    })?;
    let bytes = io_step(owner, stage, |file| {
        file.rewind()?;
        let mut bytes = Vec::with_capacity(v2::MAX_FILE_BYTES + 1);
        file.take((v2::MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > v2::MAX_FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file exceeds bounded leaf size",
            ));
        }
        Ok(bytes)
    })?;
    let after = io_step(owner, FileStage::FingerprintAfter, |file| fingerprint(file))?;
    if before != after || after.len != bytes.len() as u64 {
        return Err(owner.fail(FileFailure::Changed));
    }
    let digest = v2::Digest::from_bytes(Sha256::digest(&bytes).into());
    Ok(ObservedFile {
        bytes: v2::FileBytes::new(&bytes).expect("bounded native bytes"),
        digest,
    })
}
pub(crate) fn observe(
    owner: &mut FileOwner,
    root: &HostDir,
    path: &v2::LeafName,
) -> Result<ObservedFile, FileFailureKind> {
    open(owner, root, path, false)?;
    content(owner, FileStage::Read)
}
#[cfg(feature = "test-support")]
fn fixture_readonly_write_handle(
    owner: &mut FileOwner,
    root: &HostDir,
    path: &v2::LeafName,
) -> Result<(), FileFailureKind> {
    if !owner.fixture.as_ref().is_some_and(|c| c.fail_file_write()) {
        return Ok(());
    }
    let relative = RelativePath::parse(path.as_str()).expect("validated leaf");
    owner.opening = true;
    match root.open_file(&relative) {
        Ok(file) => {
            owner.fixture_reopened = Some(file);
            owner.opening = false;
        }
        Err(error) => {
            owner.sticky_open = true;
            let kind = owner.fail(FileFailure::OpenUnclassified(error));
            owner.opening = false;
            return Err(kind);
        }
    }
    let original = io_step(owner, FileStage::FingerprintBefore, |file| {
        fingerprint(file)
    })?;
    let reopened = fingerprint(
        owner
            .fixture_reopened
            .as_ref()
            .expect("owned reopened file"),
    )
    .map_err(|error| {
        owner.fail(FileFailure::IoAfterOpen {
            stage: FileStage::FingerprintBefore,
            error,
        })
    })?;
    if original != reopened {
        return Err(owner.fail(FileFailure::Changed));
    }
    // Install the read-only file as the ordinary I/O handle. The original RW
    // handle stays in the second owned slot through the real write failure.
    std::mem::swap(&mut owner.handle, &mut owner.fixture_reopened);
    Ok(())
}
pub(crate) fn create(
    owner: &mut FileOwner,
    root: &HostDir,
    path: &v2::LeafName,
    bytes: &v2::FileBytes,
) -> Result<CreatedFile, FileFailureKind> {
    open(owner, root, path, true)?;
    #[cfg(feature = "test-support")]
    fixture_readonly_write_handle(owner, root, path)?;
    #[cfg(feature = "test-support")]
    let fixture = owner.fixture.clone();
    io_step(owner, FileStage::Write, |file| {
        let result = file.write_all(bytes.as_slice());
        #[cfg(feature = "test-support")]
        if result.is_err()
            && let Some(control) = &fixture
        {
            control.mark_file_write_failed();
        }
        result
    })?;
    io_step(owner, FileStage::FileSync, |file| file.sync_all())?;
    let actual = content(owner, FileStage::Readback)?;
    if actual.bytes != *bytes {
        return Err(owner.fail(FileFailure::ReadbackMismatch));
    }
    // Windows only claims the successful file flush above. Its directory
    // persistence is unavailable and is deliberately not represented as done.
    #[cfg(unix)]
    {
        io_step(owner, FileStage::ParentSync, |_file| root.sync_directory())?;
    }
    Ok(CreatedFile {
        length: actual.bytes.as_slice().len() as u16,
        digest: actual.digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use theseus_host::fs::RootPolicy;
    fn leaf() -> v2::LeafName {
        v2::LeafName::new("leaf").unwrap()
    }
    fn digest(bytes: &[u8]) -> v2::Digest {
        v2::Digest::from_bytes(Sha256::digest(bytes).into())
    }
    fn root(temp: &tempfile::TempDir) -> HostDir {
        HostDir::open(temp.path(), RootPolicy::LeafNoFollow).unwrap()
    }
    #[test]
    fn fresh_empty_and_full_create_read_actual_digest() {
        for bytes in [vec![], b"actual contents".to_vec(), vec![0xa5; 4096]] {
            let temp = tempfile::tempdir().unwrap();
            let root = root(&temp);
            let mut owner = FileOwner::new();
            let created = create(
                &mut owner,
                &root,
                &leaf(),
                &v2::FileBytes::new(&bytes).unwrap(),
            )
            .unwrap();
            assert_eq!(created.length as usize, bytes.len());
            assert_eq!(created.digest, digest(&bytes));
            assert!(owner.handle.is_some());
            assert!(owner.attempted && !owner.opening && !owner.sticky_open);
            assert_eq!(std::fs::read(temp.path().join("leaf")).unwrap(), bytes);
            let mut reader = FileOwner::new();
            let observed = observe(&mut reader, &root, &leaf()).unwrap();
            assert_eq!(observed.bytes.as_slice(), bytes);
            assert_eq!(observed.digest, digest(&bytes));
        }
    }
    #[test]
    fn observes_actual_external_bytes_and_rejects_oversize_whole_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        std::fs::write(temp.path().join("leaf"), b"external").unwrap();
        let observed = observe(&mut FileOwner::new(), &root, &leaf()).unwrap();
        assert_eq!(observed.digest, digest(b"external"));
        std::fs::write(temp.path().join("leaf"), vec![1; 4097]).unwrap();
        let mut owner = FileOwner::new();
        assert_eq!(
            observe(&mut owner, &root, &leaf()).unwrap_err(),
            FileFailureKind::IoAfterOpen
        );
        assert!(matches!(
            owner.primary,
            Some(FileFailure::IoAfterOpen {
                stage: FileStage::Read,
                ..
            })
        ));
        assert!(owner.handle.is_some() && !owner.sticky_open);
    }
    #[test]
    fn existing_create_never_overwrites_and_open_error_is_sticky() {
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        std::fs::write(temp.path().join("leaf"), b"original").unwrap();
        let mut owner = FileOwner::new();
        assert_eq!(
            create(
                &mut owner,
                &root,
                &leaf(),
                &v2::FileBytes::new(b"new").unwrap()
            )
            .unwrap_err(),
            FileFailureKind::OpenUnclassified
        );
        assert!(owner.attempted && owner.sticky_open && owner.handle.is_none());
        assert!(matches!(
            owner.primary,
            Some(FileFailure::OpenUnclassified(_))
        ));
        assert_eq!(
            std::fs::read(temp.path().join("leaf")).unwrap(),
            b"original"
        );
    }
    #[test]
    fn missing_and_directory_are_unclassified_open_failures() {
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        for directory in [false, true] {
            if directory {
                std::fs::create_dir(temp.path().join("leaf")).unwrap();
            }
            let mut owner = FileOwner::new();
            assert_eq!(
                observe(&mut owner, &root, &leaf()).unwrap_err(),
                FileFailureKind::OpenUnclassified
            );
            assert!(owner.sticky_open && owner.attempted);
            assert!(matches!(
                owner.primary,
                Some(FileFailure::OpenUnclassified(_))
            ));
        }
    }
    #[cfg(unix)]
    #[test]
    fn link_is_rejected_without_following_target() {
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        std::fs::write(temp.path().join("target"), b"target").unwrap();
        std::os::unix::fs::symlink("target", temp.path().join("leaf")).unwrap();
        let mut owner = FileOwner::new();
        assert_eq!(
            observe(&mut owner, &root, &leaf()).unwrap_err(),
            FileFailureKind::OpenUnclassified
        );
        assert!(owner.sticky_open);
        assert_eq!(
            std::fs::read(temp.path().join("target")).unwrap(),
            b"target"
        );
    }
    #[test]
    fn renamed_root_retains_authority_for_create_and_observe() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("original");
        let moved = temp.path().join("moved");
        std::fs::create_dir(&original).unwrap();
        let root = HostDir::open(&original, RootPolicy::LeafNoFollow).unwrap();
        std::fs::rename(&original, &moved).unwrap();
        std::fs::create_dir(&original).unwrap();
        std::fs::write(original.join("leaf"), b"replacement").unwrap();
        create(
            &mut FileOwner::new(),
            &root,
            &leaf(),
            &v2::FileBytes::new(b"retained").unwrap(),
        )
        .unwrap();
        let observed = observe(&mut FileOwner::new(), &root, &leaf()).unwrap();
        assert_eq!(observed.bytes.as_slice(), b"retained");
        assert_eq!(std::fs::read(moved.join("leaf")).unwrap(), b"retained");
        assert_eq!(
            std::fs::read(original.join("leaf")).unwrap(),
            b"replacement"
        );
    }
    #[test]
    fn staged_failures_retain_typed_source_handle_and_partial_artifact() {
        let stages = [FileStage::Write, FileStage::FileSync, FileStage::Readback];
        #[cfg(unix)]
        let stages = stages.into_iter().chain([FileStage::ParentSync]);
        for stage in stages {
            let temp = tempfile::tempdir().unwrap();
            let root = root(&temp);
            FAULT.set(Some(Fault::Error(stage)));
            let mut owner = FileOwner::new();
            let result = create(
                &mut owner,
                &root,
                &leaf(),
                &v2::FileBytes::new(b"complete").unwrap(),
            );
            FAULT.set(None);
            assert_eq!(result.unwrap_err(), FileFailureKind::IoAfterOpen);
            let Some(FileFailure::IoAfterOpen {
                stage: actual,
                error,
            }) = &owner.primary
            else {
                panic!("missing retained source");
            };
            assert_eq!(*actual, stage);
            assert_eq!(
                error
                    .get_ref()
                    .unwrap()
                    .downcast_ref::<Original>()
                    .unwrap()
                    .0,
                stage
            );
            assert!(owner.handle.is_some() && !owner.sticky_open);
            let expected: &[u8] = if stage == FileStage::Write {
                b"partial"
            } else {
                b"complete"
            };
            assert_eq!(std::fs::read(temp.path().join("leaf")).unwrap(), expected);
        }
    }
    #[test]
    fn changed_fingerprint_and_mismatched_readback_never_succeed() {
        for (fault, expected) in [
            (Fault::Change, FileFailureKind::Changed),
            (Fault::Mismatch, FileFailureKind::ReadbackMismatch),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let root = root(&temp);
            FAULT.set(Some(fault));
            let mut owner = FileOwner::new();
            let result = create(
                &mut owner,
                &root,
                &leaf(),
                &v2::FileBytes::new(b"complete").unwrap(),
            );
            FAULT.set(None);
            assert_eq!(result.unwrap_err(), expected);
            assert_eq!(owner.primary.as_ref().unwrap().kind(), expected);
            assert!(owner.handle.is_some());
            assert!(temp.path().join("leaf").exists());
        }
    }
    #[test]
    fn panic_after_acquisition_leaves_handle_in_external_owner() {
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        let mut owner = FileOwner::new();
        FAULT.set(Some(Fault::Panic));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            create(
                &mut owner,
                &root,
                &leaf(),
                &v2::FileBytes::new(b"complete").unwrap(),
            )
        }));
        FAULT.set(None);
        owner.panic = panic.err();
        assert!(
            owner
                .panic
                .as_ref()
                .unwrap()
                .downcast_ref::<Original>()
                .is_some()
        );
        assert!(owner.handle.is_some() && owner.attempted && !owner.opening);
        assert_eq!(std::fs::read(temp.path().join("leaf")).unwrap(), b"");
    }
    #[test]
    fn retry_on_failed_owner_preserves_source_and_performs_no_new_open() {
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        OPEN_CALLS.set(0);
        let mut owner = FileOwner::new();
        assert_eq!(
            observe(&mut owner, &root, &leaf()).unwrap_err(),
            FileFailureKind::OpenUnclassified
        );
        assert_eq!(OPEN_CALLS.get(), 1);
        let source = match owner.primary.as_ref().unwrap() {
            FileFailure::OpenUnclassified(e) => e as *const io::Error,
            _ => panic!("open source absent"),
        };
        std::fs::write(temp.path().join("leaf"), b"now present").unwrap();
        assert_eq!(
            observe(&mut owner, &root, &leaf()).unwrap_err(),
            FileFailureKind::OpenUnclassified
        );
        let retained = match owner.primary.as_ref().unwrap() {
            FileFailure::OpenUnclassified(e) => e as *const io::Error,
            _ => panic!("open source replaced"),
        };
        assert_eq!(
            OPEN_CALLS.get(),
            1,
            "retry must make zero native open calls"
        );
        assert_eq!(source, retained);
        assert!(owner.handle.is_none() && owner.sticky_open);
    }
    // Test-only hooks perturb real retained-file operations. They do not model
    // native pending completion or establish platform power-loss guarantees.
    #[derive(Clone, Copy)]
    enum Fault {
        Error(FileStage),
        Change,
        Mismatch,
        Panic,
    }
    thread_local! { static OPEN_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
    thread_local! { static FAULT: std::cell::Cell<Option<Fault>> = const { std::cell::Cell::new(None) }; }
    #[derive(Debug)]
    struct Original(FileStage);
    impl std::fmt::Display for Original {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "fixture {:?}", self.0)
        }
    }
    impl std::error::Error for Original {}
    pub(super) fn record_open() {
        OPEN_CALLS.set(OPEN_CALLS.get() + 1);
    }
    pub(super) fn checkpoint(stage: FileStage, file: &mut File) -> io::Result<()> {
        match FAULT.get() {
            Some(Fault::Error(target)) if target == stage => {
                FAULT.set(None);
                if stage == FileStage::Write {
                    file.write_all(b"partial")?;
                }
                Err(io::Error::other(Original(stage)))
            }
            Some(Fault::Panic) if stage == FileStage::Write => {
                FAULT.set(None);
                std::panic::panic_any(Original(stage));
            }
            Some(Fault::Change) if stage == FileStage::FingerprintAfter => {
                FAULT.set(None);
                file.seek(io::SeekFrom::End(0))?;
                file.write_all(b"changed")
            }
            Some(Fault::Mismatch) if stage == FileStage::FingerprintBefore => {
                FAULT.set(None);
                file.rewind()?;
                file.write_all(b"different")
            }
            _ => Ok(()),
        }
    }
}
