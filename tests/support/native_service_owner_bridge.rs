//! Private fixture IPC only. This is never a shipping helper or service API.
#![allow(dead_code)]
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use theseus_host::fs::{HostDir, RelativePath, RootPolicy, fingerprint};
use theseus_native_protocol::v2;

const MAX_RECORD: u64 = 16384;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Connect,
    Install,
    Configure,
    StopForTransition,
    StartAfterTransition,
    Uninstall,
    RemoveSidecar,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub request_id: String,
    pub action: Action,
    pub context: v2::IntegrationRuntimeContext,
    pub definition: Option<(String, String)>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub request: Request,
    pub success: bool,
    pub detail: String,
}
pub fn validate_request(request: &Request) -> Result<()> {
    ensure!(
        request.request_id.len() == 32
            && request
                .request_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "original bounded native request ID required"
    );
    request.context.validate()?;
    ensure!(
        matches!(request.action, Action::Install | Action::Configure)
            == request.definition.is_some(),
        "native action definition binding differs"
    );
    if let Some((role, artifact)) = &request.definition {
        ensure!(
            ["base", "target", "configured"].contains(&role.as_str())
                && ["base", "target"].contains(&artifact.as_str()),
            "undeclared native definition key"
        );
    }
    Ok(())
}
pub fn validate_reply(request: &Request, reply: &Reply) -> Result<()> {
    validate_request(&reply.request)?;
    ensure!(
        serde_json::to_value(request)? == serde_json::to_value(&reply.request)?,
        "native reply original context/action/request differs"
    );
    Ok(())
}

/// A path never restores authority after the retained directory is replaced.
pub struct Directory {
    path: PathBuf,
    owner: HostDir,
}
impl Directory {
    pub fn open(path: &Path) -> Result<Self> {
        ensure!(
            path.is_absolute(),
            "explicit private native operator root required"
        );
        Ok(Self {
            path: path.into(),
            owner: HostDir::open(path, RootPolicy::AllComponentsNoFollow)?,
        })
    }
    pub fn from_created(path: PathBuf, owner: HostDir) -> Result<Self> {
        let original = Self { path, owner };
        original.verify()?;
        Ok(original)
    }
    pub fn verify(&self) -> Result<()> {
        ensure!(
            self.owner.same_directory_object(&HostDir::open(
                &self.path,
                RootPolicy::AllComponentsNoFollow
            )?)?,
            "original native operator directory replaced"
        );
        Ok(())
    }
    pub fn read<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<T> {
        self.verify()?;
        let path = RelativePath::parse(name)?;
        let mut file = self.owner.open_file(&path)?;
        let before = fingerprint(&file)?;
        ensure!(before.len <= MAX_RECORD, "native operator record bound");
        let mut bytes = Vec::new();
        (&mut file).take(MAX_RECORD + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_RECORD
                && fingerprint(&file)? == before
                && fingerprint(&self.owner.open_file(&path)?)? == before,
            "native operator record changed"
        );
        self.verify()?;
        Ok(serde_json::from_slice(&bytes)?)
    }
    pub fn publish(&self, name: &str, value: &impl Serialize) -> Result<()> {
        self.verify()?;
        RelativePath::parse(name)?;
        let bytes = serde_json::to_vec(value)?;
        ensure!(
            bytes.len() as u64 <= MAX_RECORD,
            "native operator record bound"
        );
        // Hard link publishes this original create_new object without replacing
        // any existing request/reply. Both private files remain raw evidence.
        let temporary = self.path.join(format!("{name}.publishing"));
        let mut file = self
            .owner
            .create_file(&RelativePath::parse(&format!("{name}.publishing"))?)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        self.verify()?;
        std::fs::hard_link(&temporary, self.path.join(name))?;
        self.verify()?;
        Ok(())
    }
}
pub struct Client {
    directory: Directory,
    context: v2::IntegrationRuntimeContext,
}
impl Client {
    pub fn connect(root: &Path, context: &v2::IntegrationRuntimeContext) -> Result<Self> {
        let mut client = Self {
            directory: Directory::open(root)?,
            context: context.clone(),
        };
        client.action(
            Action::Connect,
            None,
            Instant::now() + Duration::from_secs(35),
            &AtomicBool::new(false),
        )?;
        Ok(client)
    }
    pub fn action(
        &mut self,
        action: Action,
        definition: Option<&v2::ServiceDefinition>,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<()> {
        ensure!(
            !cancel.load(Ordering::Acquire) && Instant::now() < deadline,
            "original native request deadline/cancellation before publication"
        );
        let mut id = [0u8; 16];
        theseus_host::random_bytes(&mut id)?;
        let request = Request {
            request_id: id.iter().map(|b| format!("{b:02x}")).collect(),
            action,
            context: self.context.clone(),
            definition: definition.map(|d| (d.role.clone(), d.artifact.clone())),
        };
        validate_request(&request)?;
        self.directory
            .publish(&format!("{}.request", request.request_id), &request)?;
        let until = deadline.min(Instant::now() + Duration::from_secs(35));
        loop {
            match self
                .directory
                .read::<Reply>(&format!("{}.reply", request.request_id))
            {
                Ok(reply) => {
                    validate_reply(&request, &reply)?;
                    ensure!(reply.success, "original native SUT owner: {}", reply.detail);
                    return Ok(());
                }
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                {
                    ensure!(
                        !cancel.load(Ordering::Acquire) && Instant::now() < until,
                        "original native SUT reply remains pending; no replay"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
        }
    }
    pub fn install(
        &mut self,
        d: &v2::ServiceDefinition,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<()> {
        self.action(Action::Install, Some(d), deadline, cancel)
    }
    pub fn configure(
        &mut self,
        d: &v2::ServiceDefinition,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<()> {
        self.action(Action::Configure, Some(d), deadline, cancel)
    }
    pub fn stop_for_transition(&mut self, deadline: Instant, cancel: &AtomicBool) -> Result<()> {
        self.action(Action::StopForTransition, None, deadline, cancel)
    }
    pub fn start_after_transition(&mut self, deadline: Instant, cancel: &AtomicBool) -> Result<()> {
        self.action(Action::StartAfterTransition, None, deadline, cancel)
    }
    pub fn uninstall(&mut self, deadline: Instant, cancel: &AtomicBool) -> Result<()> {
        self.action(Action::Uninstall, None, deadline, cancel)
    }
    #[cfg(windows)]
    pub fn remove_sidecar(&mut self) -> Result<()> {
        self.action(
            Action::RemoveSidecar,
            None,
            Instant::now() + Duration::from_secs(35),
            &AtomicBool::new(false),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context() -> v2::IntegrationRuntimeContext {
        serde_json::from_value(serde_json::json!({
            "kind":"theseus_native_integration_context","schema":1,
            "ownership_id":"original-owner","binding":"01".repeat(16),
            "manifest_digest":"02".repeat(32),"workspace_marker":"03".repeat(16),
            "provider_epoch":"04".repeat(32),"integration_manifest_digest":"05".repeat(32),
            "manager_platform":"launchd","manager_unique_name":"original-user-domain",
            "namespace_receipt":"original-private-namespace","installation_instance_digest":"06".repeat(32)
        })).unwrap()
    }
    fn request() -> Request {
        Request {
            request_id: "ab".repeat(16),
            action: Action::Install,
            context: context(),
            definition: Some(("target".into(), "target".into())),
        }
    }
    #[test]
    fn native_owner_bridge_rejects_unsafe_ids_and_unbound_definition_actions() {
        validate_request(&request()).unwrap();
        let mut bad = request();
        bad.request_id = "../foreign".into();
        assert!(validate_request(&bad).is_err(), "no path traversal request");
        let mut bad = request();
        bad.definition = None;
        assert!(
            validate_request(&bad).is_err(),
            "install requires exact admitted definition"
        );
        let mut bad = request();
        bad.action = Action::Uninstall;
        assert!(
            validate_request(&bad).is_err(),
            "uninstall accepts no definition substitution"
        );
    }
    #[test]
    fn native_owner_bridge_replies_require_exact_context_action_and_original_request() {
        let original = request();
        for mutation in 0..4 {
            let mut reply = Reply {
                request: original.clone(),
                success: true,
                detail: "settled".into(),
            };
            match mutation {
                0 => reply.request.request_id = "cd".repeat(16),
                1 => reply.request.action = Action::Configure,
                2 => reply.request.context.ownership_id = "foreign-owner".into(),
                _ => reply.request.definition = Some(("configured".into(), "target".into())),
            }
            assert!(
                validate_reply(&original, &reply).is_err(),
                "mismatched reply {mutation}"
            );
        }
    }
    #[test]
    fn native_owner_bridge_refuses_record_replay_oversize_and_replaced_directory() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("operator");
        HostDir::create_private(&path).unwrap();
        let directory = Directory::open(&path).unwrap();
        directory.publish("original.request", &request()).unwrap();
        assert!(
            directory.publish("original.request", &request()).is_err(),
            "never replace an original request"
        );
        std::fs::write(path.join("oversize.request"), vec![b' '; 16385]).unwrap();
        assert!(directory.read::<Request>("oversize.request").is_err());
        std::fs::rename(&path, parent.path().join("retained-original")).unwrap();
        HostDir::create_private(&path).unwrap();
        assert!(directory.publish("foreign.request", &request()).is_err());
        assert!(
            std::fs::read_dir(&path).unwrap().next().is_none(),
            "same-path replacement untouched"
        );
    }
    #[test]
    fn native_owner_bridge_client_rejects_a_foreign_reply_without_another_request() {
        let root = tempfile::tempdir().unwrap();
        let directory = Directory::open(root.path()).unwrap();
        let path = root.path().to_owned();
        let server = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                let request_name = std::fs::read_dir(&path)
                    .unwrap()
                    .map(|e| e.unwrap().file_name().into_string().unwrap())
                    .find(|n| n.ends_with(".request"));
                if let Some(name) = request_name {
                    let original: Request = directory.read(&name).unwrap();
                    let mut foreign = original.clone();
                    foreign.context.ownership_id = "foreign-owner".into();
                    directory
                        .publish(
                            &format!("{}.reply", original.request_id),
                            &Reply {
                                request: foreign,
                                success: true,
                                detail: "not original".into(),
                            },
                        )
                        .unwrap();
                    return;
                }
                assert!(
                    Instant::now() < until,
                    "pure bridge client never published request"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        assert!(Client::connect(root.path(), &context()).is_err());
        server.join().unwrap();
        assert_eq!(
            std::fs::read_dir(root.path())
                .unwrap()
                .filter(|e| e
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|x| x == "request"))
                .count(),
            1,
            "a failed original reply must not issue a replacement request"
        );
    }
    #[test]
    fn native_owner_bridge_cancelled_or_expired_request_never_reaches_the_creator() {
        let root = tempfile::tempdir().unwrap();
        let mut client = Client {
            directory: Directory::open(root.path()).unwrap(),
            context: context(),
        };
        assert!(
            client
                .action(
                    Action::Uninstall,
                    None,
                    Instant::now() + Duration::from_secs(1),
                    &AtomicBool::new(true)
                )
                .is_err()
        );
        assert!(
            client
                .action(
                    Action::Uninstall,
                    None,
                    Instant::now(),
                    &AtomicBool::new(false)
                )
                .is_err()
        );
        assert!(
            std::fs::read_dir(root.path()).unwrap().next().is_none(),
            "unaccepted requests create no new native effect request"
        );
    }
}
