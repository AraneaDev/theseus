use crate::IntentError;
use std::{
    ffi::OsStr,
    io::{self, Read, Write},
};
use theseus_host::fs::{CommitScope, HostDir, RelativePath};
use theseus_native_protocol::v2::{self, BindingId, Digest, IntentId, WorkspaceMarker};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntentRecord {
    pub intent: IntentId,
    pub binding: BindingId,
    pub manifest_digest: Digest,
    pub workspace_marker: WorkspaceMarker,
}
/// The caller exclusively owns and serializes this namespace, including across
/// independently opened stores. This is local persistence, not authentication.
pub struct IntentStore {
    root: HostDir,
    #[cfg(test)]
    publisher: Option<TestPublisher>,
}
#[cfg(test)]
type TestPublisher = fn(&HostDir, &OsStr, &[u8]) -> io::Result<CommitScope>;
pub struct PreparedAcquire {
    record: IntentRecord,
    scope: CommitScope,
}
impl IntentStore {
    pub fn new(root: HostDir) -> Self {
        Self {
            root,
            #[cfg(test)]
            publisher: None,
        }
    }
    pub fn prepare(&mut self, record: IntentRecord) -> Result<PreparedAcquire, IntentError> {
        let name = format!("{}.json", record.intent.to_hex());
        let path = RelativePath::parse(&name).map_err(IntentError::Io)?;
        let bytes = canonical(&record);
        match self.root.open_file(&path) {
            Ok(file) => {
                let mut existing = Vec::new();
                file.take(513)
                    .read_to_end(&mut existing)
                    .map_err(IntentError::Io)?;
                if existing.len() > 512 {
                    return Err(IntentError::InvalidExisting);
                }
                let parsed = parse_canonical(&existing).ok_or(IntentError::InvalidExisting)?;
                if parsed.intent != record.intent {
                    return Err(IntentError::InvalidExisting);
                }
                if existing != bytes {
                    return Err(IntentError::Conflict);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(IntentError::Io(error)),
        }
        // Even equal existing bytes must observe a new publication barrier.
        // Never inspect file equality to turn a failed barrier into a receipt.
        let scope = self
            .publish(OsStr::new(&name), &bytes)
            .map_err(IntentError::Publication)?;
        Ok(PreparedAcquire { record, scope })
    }
    fn publish(&self, name: &OsStr, bytes: &[u8]) -> io::Result<CommitScope> {
        #[cfg(test)]
        if let Some(publisher) = self.publisher {
            return publisher(&self.root, name, bytes);
        }
        self.root.publish(name, &mut |file| file.write_all(bytes))
    }
}

fn canonical(record: &IntentRecord) -> Vec<u8> {
    format!("{{\"schema\":1,\"intent\":\"{}\",\"binding\":\"{}\",\"manifest_digest\":\"{}\",\"workspace_marker\":\"{}\"}}",
        record.intent.to_hex(), record.binding.to_hex(), record.manifest_digest.to_hex(), record.workspace_marker.to_hex()).into_bytes()
}

fn parse_canonical(bytes: &[u8]) -> Option<IntentRecord> {
    let text = std::str::from_utf8(bytes).ok()?;
    let rest = text.strip_prefix("{\"schema\":1,\"intent\":\"")?;
    let (intent, rest) = rest.split_once("\",\"binding\":\"")?;
    let (binding, rest) = rest.split_once("\",\"manifest_digest\":\"")?;
    let (digest, rest) = rest.split_once("\",\"workspace_marker\":\"")?;
    let marker = rest.strip_suffix("\"}")?;
    Some(IntentRecord {
        intent: IntentId::parse(intent).ok()?,
        binding: BindingId::parse(binding).ok()?,
        manifest_digest: Digest::parse(digest).ok()?,
        workspace_marker: WorkspaceMarker::parse(marker).ok()?,
    })
}

impl PreparedAcquire {
    pub fn record(&self) -> &IntentRecord {
        &self.record
    }
    pub fn commit_scope(&self) -> CommitScope {
        self.scope
    }
    pub fn request(&self) -> v2::Request {
        v2::Request::Acquire {
            intent: self.record.intent,
            binding: self.record.binding,
            manifest_digest: self.record.manifest_digest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use theseus_host::fs::{PublicationError, PublicationState, PublishStage};

    fn record() -> IntentRecord {
        IntentRecord {
            intent: IntentId::from_bytes([1; 16]),
            binding: BindingId::from_bytes([2; 16]),
            manifest_digest: Digest::from_bytes([3; 32]),
            workspace_marker: WorkspaceMarker::from_bytes([4; 16]),
        }
    }
    fn fixture() -> (tempfile::TempDir, IntentStore) {
        let temp = tempfile::tempdir().unwrap();
        let root = HostDir::create_private(&temp.path().join("intent")).unwrap();
        (temp, IntentStore::new(root))
    }

    #[derive(Debug)]
    struct Primary;
    impl std::fmt::Display for Primary {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("primary")
        }
    }
    impl Error for Primary {}

    fn native_write_failure(root: &HostDir, name: &OsStr, _: &[u8]) -> io::Result<CommitScope> {
        root.publish(name, &mut |_| Err(io::Error::other(Primary)))
    }
    fn native_reserve_failure(root: &HostDir, _: &OsStr, _: &[u8]) -> io::Result<CommitScope> {
        root.publish(OsStr::new("."), &mut |_| {
            panic!("invalid basename must fail before serialization")
        })
    }

    #[test]
    fn publication_failure_preserves_actual_native_typed_payload_and_primary() {
        let (_temp, mut store) = fixture();
        store.publisher = Some(native_write_failure);
        let Err(IntentError::Publication(error)) = store.prepare(record()) else {
            panic!("failed publication must not create a receipt")
        };
        let publication = error
            .get_ref()
            .unwrap()
            .downcast_ref::<PublicationError>()
            .unwrap();
        assert_eq!(publication.stage(), PublishStage::Write);
        assert_eq!(publication.state(), PublicationState::NotReplaced);
        assert!(publication.cleanup_error().is_none());
        let primary = publication
            .source()
            .unwrap()
            .downcast_ref::<io::Error>()
            .unwrap();
        assert!(primary.get_ref().unwrap().is::<Primary>());
    }

    #[test]
    fn equal_existing_bytes_still_require_a_real_publication_and_keep_reserve_error() {
        let (temp, mut store) = fixture();
        let path = temp
            .path()
            .join("intent")
            .join("01010101010101010101010101010101.json");
        std::fs::write(&path, canonical(&record())).unwrap();
        store.publisher = Some(native_reserve_failure);
        let Err(IntentError::Publication(error)) = store.prepare(record()) else {
            panic!("equality cannot grant a receipt without a publication barrier")
        };
        let publication = error
            .get_ref()
            .unwrap()
            .downcast_ref::<PublicationError>()
            .unwrap();
        assert_eq!(publication.stage(), PublishStage::Reserve);
        assert_eq!(publication.state(), PublicationState::NotReplaced);
        assert!(publication.cleanup_error().is_none());
        assert_eq!(std::fs::read(path).unwrap(), canonical(&record()));
    }

    // Consumer-side fault model only: HostDir's stage/state constructors and
    // native fault injection are private. These do not certify native faults.
    #[derive(Debug)]
    struct ModeledFailure {
        stage: PublishStage,
        state: PublicationState,
        cleanup: io::Error,
    }
    impl std::fmt::Display for ModeledFailure {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("modeled publication")
        }
    }
    impl Error for ModeledFailure {}
    fn modeled(
        root: &HostDir,
        name: &OsStr,
        bytes: &[u8],
        state: PublicationState,
    ) -> io::Result<CommitScope> {
        root.publish(name, &mut |file| file.write_all(bytes))?;
        Err(io::Error::other(ModeledFailure {
            stage: if state == PublicationState::Replaced {
                PublishStage::ParentSync
            } else {
                PublishStage::Replace
            },
            state,
            cleanup: io::Error::other(Primary),
        }))
    }
    fn after_replace(root: &HostDir, name: &OsStr, bytes: &[u8]) -> io::Result<CommitScope> {
        modeled(root, name, bytes, PublicationState::Replaced)
    }
    fn unknown(root: &HostDir, name: &OsStr, bytes: &[u8]) -> io::Result<CommitScope> {
        modeled(root, name, bytes, PublicationState::Unknown)
    }

    #[test]
    fn equal_file_after_modeled_failed_or_unknown_barrier_is_not_success() {
        for (publisher, state, stage) in [
            (
                after_replace as TestPublisher,
                PublicationState::Replaced,
                PublishStage::ParentSync,
            ),
            (
                unknown as TestPublisher,
                PublicationState::Unknown,
                PublishStage::Replace,
            ),
        ] {
            let (temp, mut store) = fixture();
            store.publisher = Some(publisher);
            let Err(IntentError::Publication(error)) = store.prepare(record()) else {
                panic!("failed barrier must not create a receipt")
            };
            let failure = error
                .get_ref()
                .unwrap()
                .downcast_ref::<ModeledFailure>()
                .unwrap();
            assert_eq!(failure.stage, stage);
            assert_eq!(failure.state, state);
            assert!(failure.cleanup.get_ref().unwrap().is::<Primary>());
            let path = temp
                .path()
                .join("intent")
                .join("01010101010101010101010101010101.json");
            assert_eq!(std::fs::read(&path).unwrap(), canonical(&record()));
            // Explicit later retry observes its own real successful barrier.
            store.publisher = None;
            assert_eq!(store.prepare(record()).unwrap().record(), &record());
            assert_eq!(failure.state, state); // Original failure remains owned.
        }
    }
}
