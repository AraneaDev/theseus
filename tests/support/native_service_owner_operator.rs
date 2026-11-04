//! Test-owned original creator. SUT requests and fixture teardown are distinct.
#![cfg(any(target_os = "macos", windows))]
#[path = "native_service_definition.rs"]
mod definition;
#[cfg(target_os = "macos")]
#[path = "native_service_launchd_sut.rs"]
mod platform_sut;
#[cfg(windows)]
#[path = "native_service_scm_sut.rs"]
mod platform_sut;
use super::creator_state::{self, FinishOwner};
use super::owner_bridge::{Action, Directory, Reply, Request, validate_request};
use anyhow::{Result, ensure};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use theseus_native_protocol::v2;

pub struct Operator {
    stop: Arc<AtomicBool>,
    worker: FinishOwner,
    _original_directory: Arc<Directory>,
}
impl Operator {
    pub fn start(
        root: PathBuf,
        original_directory: theseus_host::fs::HostDir,
        manifest: v2::IntegrationManifest,
        template: v2::IntegrationRuntimeContext,
        fault: String,
    ) -> Result<Self> {
        let directory = Arc::new(Directory::from_created(root.clone(), original_directory)?);
        let retained_directory = directory.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let worker = thread::spawn(move || -> Result<bool> {
            let mut manager: Option<platform_sut::NativeServiceSut> = None;
            let mut original_context = None;
            let mut handled = BTreeSet::new();
            let mut failure: Option<anyhow::Error> = None;
            while !signal.load(Ordering::Acquire) {
                let iteration = (|| -> Result<()> {
                    directory.verify()?;
                    for (index, entry) in std::fs::read_dir(&root)?.enumerate() {
                        ensure!(index < 1024, "bounded native operator directory");
                        let name = entry?
                            .file_name()
                            .into_string()
                            .map_err(|_| anyhow::anyhow!("native request filename must be UTF8"))?;
                        let Some(id) = name.strip_suffix(".request") else {
                            continue;
                        };
                        if handled.contains(id) {
                            continue;
                        }
                        ensure!(handled.len() < 128, "bounded native original request count");
                        let request: Request = directory.read(&name)?;
                        validate_request(&request)?;
                        ensure!(
                            id == request.request_id,
                            "native request filename correlation"
                        );
                        // Reserve this exact request before effects. Existing files
                        // are never replayed, even after a failed native reply.
                        handled.insert(id.to_owned());
                        let result = (|| -> Result<()> {
                            ensure!(
                                failure.is_none(),
                                "original native owner already failed closed"
                            );
                            let mut expected = template.clone();
                            expected.ownership_id = request.context.ownership_id.clone();
                            ensure!(
                                serde_json::to_value(&expected)?
                                    == serde_json::to_value(&request.context)?,
                                "original native fixture context differs"
                            );
                            if let Some(context) = &original_context {
                                ensure!(
                                    serde_json::to_value(context)?
                                        == serde_json::to_value(&request.context)?,
                                    "native original owner context cannot change"
                                );
                            } else {
                                ensure!(
                                    request.action == Action::Connect,
                                    "first original native request must connect"
                                );
                                let cancel = AtomicBool::new(false);
                                let deadline = Instant::now() + Duration::from_secs(30);
                                #[cfg(target_os = "macos")]
                                let connected = platform_sut::NativeServiceSut::connect(
                                    &manifest,
                                    &request.context,
                                    deadline,
                                    &cancel,
                                )?;
                                #[cfg(windows)]
                                let connected = platform_sut::NativeServiceSut::connect(
                                    &manifest,
                                    &request.context,
                                    &fault,
                                    deadline,
                                    &cancel,
                                )?;
                                #[cfg(target_os = "macos")]
                                let _ = &fault;
                                manager = Some(connected);
                                original_context = Some(request.context.clone());
                            }
                            let manager = manager.as_mut().unwrap();
                            let definition = request
                                .definition
                                .as_ref()
                                .map(|(role, artifact)| {
                                    manifest
                                        .definitions
                                        .iter()
                                        .find(|d| &d.role == role && &d.artifact == artifact)
                                        .ok_or_else(|| {
                                            anyhow::anyhow!("undeclared original native definition")
                                        })
                                })
                                .transpose()?;
                            let cancel = AtomicBool::new(false);
                            let deadline = Instant::now() + Duration::from_secs(30);
                            match request.action {
                                Action::Connect => Ok(()),
                                Action::Install => {
                                    manager.install(definition.unwrap(), deadline, &cancel)
                                }
                                Action::Configure => {
                                    manager.configure(definition.unwrap(), deadline, &cancel)
                                }
                                Action::StopForTransition => {
                                    manager.stop_for_transition(deadline, &cancel)
                                }
                                Action::StartAfterTransition => {
                                    manager.start_after_transition(deadline, &cancel)
                                }
                                Action::Uninstall => manager.uninstall(deadline, &cancel),
                                #[cfg(windows)]
                                Action::RemoveSidecar => manager.remove_sidecar(),
                                #[cfg(target_os = "macos")]
                                Action::RemoveSidecar => {
                                    anyhow::bail!("SCM-only original sidecar action")
                                }
                            }
                        })();
                        let success = result.is_ok();
                        let detail = result
                            .as_ref()
                            .map(|_| "original SUT native action settled".into())
                            .unwrap_or_else(|e| format!("{e:#}"));
                        if let Err(error) = result {
                            failure.get_or_insert(error);
                        }
                        directory.publish(
                            &format!("{id}.reply"),
                            &Reply {
                                request,
                                success,
                                detail,
                            },
                        )?;
                    }
                    Ok(())
                })();
                if let Err(error) = iteration {
                    failure.get_or_insert(error);
                }
                thread::sleep(Duration::from_millis(5));
            }
            // Only explicit fixture finish/Drop reaches this independently
            // labelled teardown, after the controller/report failure is observed.
            let owned = manager.as_ref().is_some_and(|m| m.has_owned_registration());
            #[cfg(target_os = "macos")]
            let creation_evidence = manager.as_ref().map(|m| m.creation_evidence());
            #[cfg(windows)]
            let creation_evidence = manager.as_ref().map(
                |m| serde_json::json!({"retained_original_scm_creator":m.has_owned_registration()}),
            );
            let teardown = if owned {
                manager.as_mut().unwrap().uninstall(
                    Instant::now() + Duration::from_secs(30),
                    &AtomicBool::new(false),
                )
            } else {
                Ok(())
            };
            let receipt = serde_json::json!({"kind":"fixture_original_creator_teardown","schema":1,"had_original_registration":owned,"creation_evidence":creation_evidence,"success":teardown.is_ok()&&failure.is_none(),"sut_failure":failure.as_ref().map(|e|format!("{e:#}")),"detail":teardown.as_ref().err().map(|e|format!("{e:#}"))});
            let mut failures = Vec::new();
            if let Err(error) = directory.publish("fixture-teardown.json", &receipt) {
                failures.push(error);
            }
            if let Err(error) = teardown {
                failures.push(error);
            }
            if let Some(error) = failure {
                failures.push(error);
            }
            creator_state::completion_result(owned, failures)
        });
        Ok(Self {
            stop,
            worker: FinishOwner::new(worker),
            _original_directory: retained_directory,
        })
    }
    pub fn finish(&mut self) -> Result<bool> {
        self.stop.store(true, Ordering::Release);
        self.worker.finish()
    }
}
impl Drop for Operator {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            eprintln!("fixture original-creator teardown failed: {error:#}");
        }
    }
}
