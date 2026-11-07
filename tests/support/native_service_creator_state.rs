//! Fixture-only creator ownership and terminal join evidence.
#![allow(dead_code)]
use anyhow::{Result, ensure};
use std::{
    any::Any,
    fmt,
    fs::File,
    io::{self, Read, Write},
    path::Path,
    sync::Arc,
    thread::JoinHandle,
};
use theseus_host::fs::{
    CommitScope, HostDir, PublicationError, PublicationState, PublishStage, RelativePath,
    RootPolicy, fingerprint,
};

#[derive(Debug)]
pub struct PublicationFailure {
    stage: Option<PublishStage>,
    state: PublicationState,
    error: io::Error,
}
impl PublicationFailure {
    fn from_io(error: io::Error) -> Self {
        let typed = error
            .get_ref()
            .and_then(|e| e.downcast_ref::<PublicationError>());
        Self {
            stage: typed.map(PublicationError::stage),
            state: typed.map_or(PublicationState::Unknown, PublicationError::state),
            error,
        }
    }
}
impl fmt::Display for PublicationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "original publication {:?}/{:?}: {}",
            self.stage, self.state, self.error
        )
    }
}
impl std::error::Error for PublicationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Default)]
pub struct CreatedFiles {
    current: Option<(File, Vec<u8>)>,
    partial: Vec<(File, Vec<u8>)>,
    failed: Option<Arc<PublicationFailure>>,
    revoked: bool,
    unresolved: bool,
}
impl CreatedFiles {
    fn valid(&self) -> Result<()> {
        ensure!(
            !self.revoked,
            "original creator file authority permanently revoked"
        );
        ensure!(
            !self.unresolved,
            "original creator native operation remains unresolved"
        );
        if let Some(error) = &self.failed {
            return Err(anyhow::anyhow!("unresolved original publication: {error}"));
        }
        Ok(())
    }
    pub fn current(
        &mut self,
        root: &HostDir,
        path: &Path,
        leaf: &str,
    ) -> Result<Option<(File, Vec<u8>)>> {
        self.valid()?;
        let result = (|| -> Result<Option<(File, Vec<u8>)>> {
            self.verify_root(root, path)?;
            let file = match root.open_file(&RelativePath::parse(leaf)?) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound && self.current.is_none() => {
                    return Ok(None);
                }
                Err(e) => return Err(e.into()),
            };
            let (original, expected) = self.current.as_ref().ok_or_else(|| {
                anyhow::anyhow!("preexisting file has no original creation owner")
            })?;
            let before = fingerprint(&file)?;
            ensure!(
                before.len <= 65536 && before == fingerprint(original)?,
                "original created plist replaced; no same-byte adoption"
            );
            let mut bytes = Vec::new();
            (&file).take(65537).read_to_end(&mut bytes)?;
            ensure!(
                fingerprint(&file)? == before && &bytes == expected,
                "original created plist bytes changed"
            );
            self.verify_root(root, path)?;
            Ok(Some((file, bytes)))
        })();
        if result.is_err() {
            self.revoked = true;
        }
        result
    }
    pub fn verify_root(&mut self, root: &HostDir, path: &Path) -> Result<()> {
        self.valid()?;
        let result = (|| -> Result<()> {
            ensure!(
                root.same_directory_object(&HostDir::open(
                    path,
                    RootPolicy::AllComponentsNoFollow
                )?)?,
                "original creator definition directory replaced"
            );
            Ok(())
        })();
        if result.is_err() {
            self.revoked = true;
        }
        result
    }
    pub fn publish_using(
        &mut self,
        bytes: &[u8],
        publish: impl FnOnce(
            &mut dyn FnMut(&mut File) -> io::Result<()>,
        ) -> std::result::Result<CommitScope, PublicationFailure>,
    ) -> Result<()> {
        self.valid()?;
        let mut original = None;
        let result = publish(&mut |file| {
            original = Some(file.try_clone()?);
            file.write_all(bytes)
        });
        match result {
            Ok(_) => {
                self.current = original.map(|file| (file, bytes.to_vec()));
                Ok(())
            }
            Err(error) => {
                // Even a NotReplaced failure can leave a partial owned object;
                // Replaced/Unknown must never discard the new creation File.
                if let Some(file) = original {
                    self.partial.push((file, bytes.to_vec()));
                }
                let error = Arc::new(error);
                self.failed = Some(error.clone());
                Err(SharedPublicationFailure(error).into())
            }
        }
    }
    pub fn publish(&mut self, root: &HostDir, leaf: &str, bytes: &[u8]) -> Result<()> {
        self.publish_using(bytes, |write| {
            root.publish(leaf.as_ref(), write)
                .map_err(PublicationFailure::from_io)
        })
    }
    pub fn dispatch<T>(
        &mut self,
        root: &HostDir,
        path: &Path,
        leaf: &str,
        inspect: impl FnOnce() -> Result<()>,
        effect: impl FnOnce(&File) -> Result<T>,
    ) -> Result<T> {
        self.valid()?;
        inspect()?;
        // This is the final creator check AFTER potentially blocking native
        // inspection. Keep this exact witness through the effect's completion.
        let Some((file, _)) = self.current(root, path, leaf)? else {
            self.revoked = true;
            anyhow::bail!("original created plist missing at effect boundary");
        };
        effect(&file)
    }
    pub fn has_owners(&self) -> bool {
        self.current.is_some() || !self.partial.is_empty() || self.failed.is_some()
    }
    pub fn clear_committed(&mut self) {
        self.current.take();
    }
    pub fn retain_unresolved(&mut self) {
        self.unresolved = true;
    }
    pub fn evidence(&self) -> serde_json::Value {
        serde_json::json!({"retained_created_files":usize::from(self.current.is_some())+self.partial.len(),"revoked":self.revoked,"unresolved_native_operation":self.unresolved,"publication_failure":self.failed.as_ref().map(|e|serde_json::json!({"stage":format!("{:?}",e.stage),"state":format!("{:?}",e.state),"detail":e.to_string()}))})
    }
}
#[derive(Debug)]
struct SharedPublicationFailure(Arc<PublicationFailure>);
impl fmt::Display for SharedPublicationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.0.as_ref(), f)
    }
}
impl std::error::Error for SharedPublicationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}
impl Drop for CreatedFiles {
    fn drop(&mut self) {
        if self.failed.is_some() || self.revoked || self.unresolved {
            // Unproved partial creation is retained until process disposal.
            // Never silently close its only original File on worker failure.
            if let Some(original) = self.current.take() {
                std::mem::forget(original);
            }
            for original in self.partial.drain(..) {
                std::mem::forget(original);
            }
        }
    }
}

struct FinishFailure {
    error: anyhow::Error,
    _panic: std::sync::Mutex<Option<Box<dyn Any + Send>>>,
}
impl fmt::Debug for FinishFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.error, f)
    }
}
#[derive(Debug)]
struct SharedFinishFailure(Arc<FinishFailure>);
impl fmt::Display for SharedFinishFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.error, f)
    }
}
impl std::error::Error for SharedFinishFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.error.as_ref())
    }
}
pub struct FinishOwner {
    worker: Option<JoinHandle<Result<bool>>>,
    terminal: Option<std::result::Result<bool, Arc<FinishFailure>>>,
    join_evidence: String,
}
impl FinishOwner {
    pub fn new(worker: JoinHandle<Result<bool>>) -> Self {
        Self {
            join_evidence: format!(
                "original creator {:?}, name={:?}",
                worker.thread().id(),
                worker.thread().name()
            ),
            worker: Some(worker),
            terminal: None,
        }
    }
    pub fn finish(&mut self) -> Result<bool> {
        if self.terminal.is_none() {
            let (result, panic) = match self.worker.take() {
                Some(worker) => match worker.join() {
                    Ok(result) => (result, None),
                    Err(payload) => {
                        let detail = payload
                            .downcast_ref::<String>()
                            .map(String::as_str)
                            .or_else(|| payload.downcast_ref::<&'static str>().copied())
                            .unwrap_or("opaque original panic payload");
                        (
                            Err(anyhow::anyhow!("{} panicked: {detail}", self.join_evidence)),
                            Some(payload),
                        )
                    }
                },
                None => (
                    Err(anyhow::anyhow!(
                        "original creator join owner missing without terminal evidence"
                    )),
                    None,
                ),
            };
            self.terminal = Some(result.map_err(|error| {
                Arc::new(FinishFailure {
                    error,
                    _panic: std::sync::Mutex::new(panic),
                })
            }));
        }
        match self.terminal.as_ref().unwrap() {
            Ok(owned) => Ok(*owned),
            Err(error) => Err(SharedFinishFailure(error.clone()).into()),
        }
    }
}
impl Drop for FinishOwner {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            eprintln!("retained original creator completion failure: {error:#}");
        }
    }
}

#[derive(Debug)]
struct CompletionFailures(Vec<anyhow::Error>);
impl fmt::Display for CompletionFailures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, error) in self.0.iter().enumerate() {
            if index > 0 {
                write!(f, "; ")?;
            }
            write!(f, "{error:#}")?;
        }
        Ok(())
    }
}
impl std::error::Error for CompletionFailures {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0
            .first()
            .map(|e| e.as_ref() as &(dyn std::error::Error + 'static))
    }
}
pub fn completion_result(owned: bool, failures: Vec<anyhow::Error>) -> Result<bool> {
    if failures.is_empty() {
        Ok(owned)
    } else {
        Err(CompletionFailures(failures).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    fn creator_rechecks_file_after_inspection_for_each_effect_and_revokes_permanently() {
        for action in ["bootstrap", "bootout", "kickstart"] {
            for foreign in [b"original".as_slice(), b"foreign".as_slice()] {
                let directory = tempfile::tempdir().unwrap();
                let root =
                    HostDir::open(directory.path(), RootPolicy::AllComponentsNoFollow).unwrap();
                let mut owner = CreatedFiles::default();
                owner.publish(&root, "job.plist", b"original").unwrap();
                let calls = Cell::new(0);
                let outcome = owner.dispatch(
                    &root,
                    directory.path(),
                    "job.plist",
                    || {
                        std::fs::rename(
                            directory.path().join("job.plist"),
                            directory.path().join("original.plist"),
                        )?;
                        std::fs::write(directory.path().join("job.plist"), foreign)?;
                        Ok(())
                    },
                    |_| {
                        calls.set(calls.get() + 1);
                        Ok(())
                    },
                );
                assert!(
                    outcome.is_err(),
                    "{action} must reject replacement after inspect"
                );
                assert_eq!(calls.get(), 0);
                assert_eq!(
                    std::fs::read(directory.path().join("original.plist")).unwrap(),
                    b"original"
                );
                assert_eq!(
                    std::fs::read(directory.path().join("job.plist")).unwrap(),
                    foreign
                );
                std::fs::rename(
                    directory.path().join("job.plist"),
                    directory.path().join("foreign.plist"),
                )
                .unwrap();
                std::fs::rename(
                    directory.path().join("original.plist"),
                    directory.path().join("job.plist"),
                )
                .unwrap();
                assert!(
                    owner
                        .dispatch(
                            &root,
                            directory.path(),
                            "job.plist",
                            || Ok(()),
                            |_| {
                                calls.set(calls.get() + 1);
                                Ok(())
                            }
                        )
                        .is_err(),
                    "restoration cannot resurrect this revoked creator"
                );
                assert_eq!(calls.get(), 0);
            }
        }
    }
    #[test]
    fn creator_retains_initial_and_prior_files_after_injected_post_rename_parent_sync_error() {
        for configured in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let root = HostDir::open(directory.path(), RootPolicy::AllComponentsNoFollow).unwrap();
            let mut owner = CreatedFiles::default();
            if configured {
                owner.publish(&root, "job.plist", b"prior").unwrap();
            }
            let result = owner.publish_using(b"new", |write| {
                root.publish("job.plist".as_ref(), write).unwrap();
                Err(PublicationFailure {
                    stage: Some(PublishStage::ParentSync),
                    state: PublicationState::Replaced,
                    error: io::Error::other("injected post-rename parent sync failure"),
                })
            });
            assert!(result.is_err());
            assert!(
                owner.has_owners(),
                "partial publication is never no-owner cleanup"
            );
            assert_eq!(owner.partial.len(), 1);
            assert_eq!(
                fingerprint(&owner.partial[0].0).unwrap(),
                fingerprint(&File::open(directory.path().join("job.plist")).unwrap()).unwrap()
            );
            if configured {
                assert_eq!(owner.current.as_ref().unwrap().1, b"prior");
                let mut prior = [0u8; 5];
                theseus_host::fs::read_exact_at(&owner.current.as_ref().unwrap().0, &mut prior, 0)
                    .unwrap();
                assert_eq!(
                    &prior, b"prior",
                    "the actual prior creation File remains held after replacement"
                );
            }
            let calls = Cell::new(0);
            assert!(
                owner
                    .dispatch(
                        &root,
                        directory.path(),
                        "job.plist",
                        || Ok(()),
                        |_| {
                            calls.set(1);
                            Ok(())
                        }
                    )
                    .is_err()
            );
            assert_eq!(calls.get(), 0, "failed publication never bootstraps");
            assert_eq!(owner.evidence()["publication_failure"]["state"], "Replaced");
        }
    }
    #[test]
    fn creator_finish_preserves_teardown_receipt_and_panic_failures_on_every_call() {
        for reason in [
            "native teardown failed",
            "receipt publication failed",
            "panic",
        ] {
            let mut finish = FinishOwner::new(std::thread::spawn(move || {
                if reason == "panic" {
                    panic!("original panic evidence");
                }
                anyhow::bail!(reason)
            }));
            let first = finish.finish().unwrap_err().to_string();
            let second = finish.finish().unwrap_err().to_string();
            assert_eq!(first, second);
            assert!(first.contains(if reason == "panic" {
                "original panic evidence"
            } else {
                reason
            }));
        }
    }
    #[test]
    fn creator_successful_finish_is_idempotent_and_preserves_owned_result() {
        let mut finish = FinishOwner::new(std::thread::spawn(|| Ok(true)));
        assert!(finish.finish().unwrap());
        assert!(finish.finish().unwrap());
    }
    #[test]
    fn creator_panic_payload_survives_owner_drop_through_returned_failure_evidence() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Payload(Arc<AtomicBool>);
        impl Drop for Payload {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let payload = Payload(dropped.clone());
        let mut owner =
            FinishOwner::new(std::thread::spawn(move || std::panic::panic_any(payload)));
        let first = owner.finish().unwrap_err();
        let second = owner.finish().unwrap_err();
        drop(owner);
        assert!(!dropped.load(Ordering::SeqCst));
        drop(first);
        assert!(!dropped.load(Ordering::SeqCst));
        drop(second);
        assert!(dropped.load(Ordering::SeqCst));
    }
}
