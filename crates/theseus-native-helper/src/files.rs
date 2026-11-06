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
    NativeUnsettled(theseus_host::fs::LeafOpenUncertainty),
    OpenUnclassified(io::Error),
    IoAfterOpen { stage: FileStage, error: io::Error },
    Changed,
    ReadbackMismatch,
}
#[derive(Debug, Default)]
pub(crate) struct FileOwner {
    pub(crate) handle: Option<File>,
    pub(crate) native_reopened: Option<File>,
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
            Self::NativeUnsettled(_) | Self::OpenUnclassified(_) => {
                FileFailureKind::OpenUnclassified
            }
            Self::IoAfterOpen { .. } => FileFailureKind::IoAfterOpen,
            Self::Changed => FileFailureKind::Changed,
            Self::ReadbackMismatch => FileFailureKind::ReadbackMismatch,
        }
    }
}
impl std::fmt::Display for FileFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NativeUnsettled(error) => write!(f, "native leaf completion unproved: {error}"),
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
            Self::NativeUnsettled(error) => Some(error),
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

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NativeObservation {
    Present { length: u64, sha256: v2::Digest },
    Absent,
}
pub(crate) fn inspect_native(
    owner: &mut FileOwner,
    root: &HostDir,
    path: &v2::LeafName,
    deadline: std::time::Instant,
    cancelled: impl Fn() -> bool,
) -> Result<NativeObservation, FileFailureKind> {
    use theseus_host::fs::LeafOpenResult;
    if let Some(e) = &owner.primary {
        return Err(e.kind());
    }
    if owner.attempted {
        return Err(owner.fail(FileFailure::Changed));
    }
    let check = || !cancelled() && std::time::Instant::now() < deadline;
    if !check() {
        return Err(owner.fail(FileFailure::Changed));
    }
    owner.attempted = true;
    owner.opening = true;
    // Windows: this handle shares read/delete only and stays owned from the
    // first fingerprint through the reopen check, so no writer can coexist
    // (an existing writer is a settled refusal). Same-size in-place rewrites
    // within one NTFS timestamp tick are otherwise invisible to fingerprints.
    // Rename/replacement is still caught by file identity at reopen.
    match root.open_leaf_deny_write_classified(std::ffi::OsStr::new(path.as_str())) {
        LeafOpenResult::Opened(file) => {
            owner.handle = Some(file);
            owner.opening = false;
        }
        LeafOpenResult::MissingSettled => {
            owner.opening = false;
            return Ok(NativeObservation::Absent);
        }
        LeafOpenResult::FailedSettled(e) => {
            owner.opening = false;
            return Err(owner.fail(FileFailure::OpenUnclassified(e)));
        }
        LeafOpenResult::Unsettled(e) => {
            owner.opening = false;
            owner.sticky_open = true;
            return Err(owner.fail(FileFailure::NativeUnsettled(e)));
        }
    }
    let before = io_step(owner, FileStage::FingerprintBefore, |f| fingerprint(f))?;
    if before.len > v2::MAX_NATIVE_FILE_BYTES {
        return Err(owner.fail(FileFailure::Changed));
    }
    let mut hash = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0u8; v2::NATIVE_INSPECT_BUFFER_BYTES];
    loop {
        if !check() {
            return Err(owner.fail(FileFailure::Changed));
        }
        // One extra bounded read proves EOF, even for the exact 64MiB boundary.
        let n = io_step(owner, FileStage::Read, |f| f.read(&mut buffer))?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .ok_or_else(|| owner.fail(FileFailure::Changed))?;
        if count > v2::MAX_NATIVE_FILE_BYTES {
            return Err(owner.fail(FileFailure::Changed));
        }
        hash.update(&buffer[..n]);
    }
    let after = io_step(owner, FileStage::FingerprintAfter, |f| fingerprint(f))?;
    if !check() || before != after || after.len != count {
        return Err(owner.fail(FileFailure::Changed));
    }
    #[cfg(test)]
    tests::native_reopen_checkpoint();
    owner.opening = true;
    match root.open_leaf_deny_write_classified(std::ffi::OsStr::new(path.as_str())) {
        LeafOpenResult::Opened(file) => {
            owner.native_reopened = Some(file);
            owner.opening = false;
        }
        LeafOpenResult::MissingSettled => {
            owner.opening = false;
            return Err(owner.fail(FileFailure::Changed));
        }
        LeafOpenResult::FailedSettled(e) => {
            owner.opening = false;
            return Err(owner.fail(FileFailure::OpenUnclassified(e)));
        }
        LeafOpenResult::Unsettled(e) => {
            owner.opening = false;
            owner.sticky_open = true;
            return Err(owner.fail(FileFailure::NativeUnsettled(e)));
        }
    }
    let reopened = fingerprint(owner.native_reopened.as_ref().unwrap()).map_err(|error| {
        owner.fail(FileFailure::IoAfterOpen {
            stage: FileStage::FingerprintAfter,
            error,
        })
    })?;
    if !check() || reopened != after {
        return Err(owner.fail(FileFailure::Changed));
    }
    Ok(NativeObservation::Present {
        length: count,
        sha256: v2::Digest::from_bytes(hash.finalize().into()),
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
    fn native_exact_limit_eof_overlimit_and_cancel() {
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        let path = temp.path().join("leaf");
        let file = File::create(&path).unwrap();
        file.set_len(v2::MAX_NATIVE_FILE_BYTES).unwrap();
        drop(file);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        assert_eq!(
            inspect_native(&mut FileOwner::new(), &root, &leaf(), deadline, || false).unwrap(),
            NativeObservation::Present {
                length: v2::MAX_NATIVE_FILE_BYTES,
                sha256: v2::Digest::parse(
                    "3b6a07d0d404fab4e23b6d34bc6696a6a312dd92821332385e5af7c01c421351"
                )
                .unwrap()
            }
        );
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(v2::MAX_NATIVE_FILE_BYTES + 1)
            .unwrap();
        assert!(inspect_native(&mut FileOwner::new(), &root, &leaf(), deadline, || false).is_err());
        assert!(
            inspect_native(
                &mut FileOwner::new(),
                &root,
                &leaf(),
                std::time::Instant::now(),
                || false
            )
            .is_err()
        );
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(1024 * 1024)
            .unwrap();
        let calls = std::cell::Cell::new(0);
        let mut owner = FileOwner::new();
        assert!(
            inspect_native(&mut owner, &root, &leaf(), deadline, || {
                calls.set(calls.get() + 1);
                calls.get() > 2
            })
            .is_err()
        );
        assert!(owner.handle.is_some());
    }
    #[test]
    fn native_after_read_change_and_same_size_replacement_refuse() {
        // What the mutation hook actually achieved: Ok(()) means the
        // "modified" bytes landed; Err(code) means the OS refused the write.
        type Landed = std::rc::Rc<std::cell::Cell<Option<Result<(), Option<i32>>>>>;
        for replace in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = root(&temp);
            let path = temp.path().join("leaf");
            std::fs::write(&path, b"original").unwrap();
            let landed = Landed::default();
            let record = landed.clone();
            let target = path.clone();
            let mutation = Box::new(move || {
                if replace {
                    // FILE_SHARE_DELETE keeps rename possible on Windows too.
                    std::fs::rename(&target, target.with_extension("moved")).unwrap();
                }
                let write = std::fs::write(&target, b"modified");
                record.set(Some(write.map_err(|e| e.raw_os_error())));
            });
            if replace {
                NATIVE_REOPEN_HOOK.with(|hook| *hook.borrow_mut() = Some(mutation));
            } else {
                NATIVE_HOOK.with(|hook| *hook.borrow_mut() = Some(mutation));
            }
            let mut owner = FileOwner::new();
            let result = inspect_native(
                &mut owner,
                &root,
                &leaf(),
                std::time::Instant::now() + std::time::Duration::from_secs(30),
                || false,
            );
            let write = landed.take().expect("mutation hook never ran");
            assert!(owner.handle.is_some());
            match (result, write) {
                // A mutation that landed must always be refused.
                (result, Ok(())) => {
                    assert!(result.is_err(), "landed mutation went unnoticed");
                    assert_eq!(owner.native_reopened.is_some(), replace);
                }
                // Windows deny-write inspection: the in-place rewrite could not
                // land, so the observation is exactly the original bytes.
                #[cfg(windows)]
                (Ok(observed), Err(Some(32))) if !replace => {
                    assert_eq!(
                        observed,
                        NativeObservation::Present {
                            length: 8,
                            sha256: digest(b"original")
                        }
                    );
                    assert_eq!(std::fs::read(&path).unwrap(), b"original");
                    assert!(owner.native_reopened.is_some());
                }
                (result, write) => {
                    panic!("unexpected outcome: inspect {result:?}, mutation {write:?}")
                }
            }
        }
    }
    #[cfg(unix)]
    #[test]
    fn native_permission_denied_is_settled_error_never_absence() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        let path = temp.path().join("leaf");
        std::fs::write(&path, b"private").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root is permitted to bypass mode bits; this test requires a real denial.
        if File::open(&path).is_ok() {
            return;
        }
        let mut owner = FileOwner::new();
        assert!(
            inspect_native(
                &mut owner,
                &root,
                &leaf(),
                std::time::Instant::now() + std::time::Duration::from_secs(30),
                || false
            )
            .is_err()
        );
        assert!(!owner.sticky_open);
        assert!(matches!(
            owner.primary,
            Some(FileFailure::OpenUnclassified(_))
        ));
    }
    #[test]
    fn native_nonregular_and_symlink_are_never_absent() {
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        std::fs::create_dir(temp.path().join("leaf")).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        assert!(inspect_native(&mut FileOwner::new(), &root, &leaf(), deadline, || false).is_err());
        #[cfg(unix)]
        {
            std::fs::remove_dir(temp.path().join("leaf")).unwrap();
            std::os::unix::fs::symlink("missing", temp.path().join("leaf")).unwrap();
            assert!(
                inspect_native(&mut FileOwner::new(), &root, &leaf(), deadline, || false).is_err()
            );
        }
    }
    pub(super) fn native_reopen_checkpoint() {
        NATIVE_REOPEN_HOOK.with(|h| {
            if let Some(hook) = h.borrow_mut().take() {
                hook();
            }
        });
    }
    thread_local! {static NATIVE_REOPEN_HOOK:std::cell::RefCell<Option<Box<dyn FnOnce()>>>=const{std::cell::RefCell::new(None)};}
    thread_local! {static NATIVE_HOOK:std::cell::RefCell<Option<Box<dyn FnOnce()>>>=const{std::cell::RefCell::new(None)};}
    #[test]
    fn native_streams_large_reader_and_settled_missing() {
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        let bytes = vec![0x5a; 16385];
        std::fs::write(temp.path().join("leaf"), &bytes).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        assert_eq!(
            inspect_native(&mut FileOwner::new(), &root, &leaf(), deadline, || false).unwrap(),
            NativeObservation::Present {
                length: bytes.len() as u64,
                sha256: digest(&bytes)
            }
        );
        std::fs::remove_file(temp.path().join("leaf")).unwrap();
        assert_eq!(
            inspect_native(&mut FileOwner::new(), &root, &leaf(), deadline, || false).unwrap(),
            NativeObservation::Absent
        );
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
        if stage == FileStage::FingerprintAfter {
            NATIVE_HOOK.with(|h| {
                if let Some(hook) = h.borrow_mut().take() {
                    hook();
                }
            });
        }
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
