#![allow(dead_code)]
//! Public frontend assertions and retained ownership for explicit disposable service fixtures.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
    thread,
    time::{Duration, Instant},
};
use theseus_host::process::{
    ChildEnvironment, HostCleanup, InputMode, NativeCommand, OwnedChild, PipeReader, PipeWriter,
};
pub const FRAME: usize = 65536;
const TRANSCRIPT: usize = 8 * 1024 * 1024;
pub struct Controller {
    child: Option<OwnedChild>,
    child_status: Option<ExitStatus>,
    input: Option<PipeWriter>,
    output: Option<PipeReader>,
    errors: Option<PipeReader>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub pending: Vec<u8>,
    output_eof: bool,
    errors_eof: bool,
    serial: u64,
    deadline: Instant,
    evidence: PathBuf,
    prefix: &'static str,
    requests: Vec<u8>,
    pub active: Option<(String, String)>,
    nonblocking: bool,
}
fn inherited_command(command: &Command) -> NativeCommand {
    let mut environment = std::env::vars_os().collect::<std::collections::BTreeMap<_, _>>();
    for (name, value) in command.get_envs() {
        if let Some(value) = value {
            environment.insert(name.to_owned(), value.to_owned());
        } else {
            environment.remove(name);
        }
    }
    NativeCommand {
        program: command.get_program().to_owned(),
        args: command.get_args().map(|s| s.to_owned()).collect(),
        cwd: command.get_current_dir().map(Path::to_owned),
        environment: ChildEnvironment::Replace(environment.into_iter().collect()),
    }
}
impl Controller {
    pub fn spawn(command: &mut Command, evidence: &Path, prefix: &'static str) -> Result<Self> {
        let acquired =
            OwnedChild::spawn(&inherited_command(command), InputMode::Pipe).map_err(|failure| {
                let error = failure.error;
                if let HostCleanup::Pending(mut pending) = failure.cleanup {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while Instant::now() < deadline {
                        match pending.try_complete() {
                            Ok(true) => return anyhow::Error::new(error),
                            Ok(false) | Err(_) => thread::sleep(Duration::from_millis(2)),
                        }
                    }
                    let _ = Box::leak(Box::new(pending));
                }
                anyhow::Error::new(error)
            })?;
        let owner = Self {
            input: acquired.stdin,
            output: Some(acquired.stdout),
            errors: Some(acquired.stderr),
            child: Some(acquired.child),
            child_status: None,
            stdout: vec![],
            stderr: vec![],
            pending: vec![],
            output_eof: false,
            errors_eof: false,
            serial: 0,
            deadline: Instant::now() + Duration::from_secs(180),
            evidence: evidence.to_owned(),
            prefix,
            requests: vec![],
            active: None,
            nonblocking: true,
        };
        Ok(owner)
    }
    fn drain(
        pipe: &mut PipeReader,
        target: &mut Vec<u8>,
        mut duplicate: Option<&mut Vec<u8>>,
        cap: usize,
    ) -> Result<bool> {
        for _ in 0..8 {
            let mut bytes = [0; 8192];
            match pipe.try_read(&mut bytes) {
                Ok(0) => return Ok(true),
                Ok(n) => {
                    ensure!(
                        target.len() + n <= cap,
                        "controller output exceeds test bound"
                    );
                    target.extend_from_slice(&bytes[..n]);
                    if let Some(copy) = duplicate.as_mut() {
                        copy.extend_from_slice(&bytes[..n]);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(false)
    }
    fn pump(&mut self) -> Result<()> {
        if !self.output_eof {
            self.output_eof = Self::drain(
                self.output.as_mut().context("stdout owner")?,
                &mut self.stdout,
                Some(&mut self.pending),
                TRANSCRIPT,
            )?;
        }
        if !self.errors_eof {
            self.errors_eof = Self::drain(
                self.errors.as_mut().context("stderr owner")?,
                &mut self.stderr,
                None,
                FRAME,
            )?;
        }
        Ok(())
    }
    pub fn send(&mut self, value: &Value, deadline: Instant) -> Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        ensure!(bytes.len() <= FRAME, "request exceeds shipping frame bound");
        ensure!(
            self.requests.len() + bytes.len() <= TRANSCRIPT,
            "request transcript bound"
        );
        self.requests.extend_from_slice(&bytes);
        let mut sent = 0;
        while sent < bytes.len() {
            ensure!(
                Instant::now() < deadline,
                "controller request write deadline"
            );
            self.pump()?;
            match self
                .input
                .as_mut()
                .context("controller input closed")?
                .try_write(&bytes[sent..])
            {
                Ok(0) => bail!("controller stdin closed"),
                Ok(n) => sent += n,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2))
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.serial += 1;
        ensure!(self.serial <= 4096, "bounded request count");
        let id = self.serial;
        let deadline = (Instant::now()
            + Duration::from_secs(if self.serial == 1 { 60 } else { 5 }))
        .min(self.deadline);
        self.send(
            &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            deadline,
        )?;
        for _ in 0..128 {
            loop {
                self.pump()?;
                if let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
                    ensure!(end < FRAME, "response exceeds shipping frame bound");
                    let line = self.pending.drain(..=end).collect::<Vec<_>>();
                    let response: Value = serde_json::from_slice(&line)
                        .context("ordinary stdout must be only JSON lines")?;
                    if response.get("id").is_none() {
                        break;
                    }
                    ensure!(
                        response["id"] == id,
                        "original request correlation mismatch: {response}"
                    );
                    ensure!(response.get("error").is_none(), "RPC error: {response}");
                    return Ok(response);
                }
                ensure!(
                    self.pending.len() < FRAME,
                    "unterminated response exceeds bound"
                );
                ensure!(
                    !self.output_eof,
                    "controller stdout closed: {}",
                    String::from_utf8_lossy(&self.stderr)
                );
                ensure!(
                    Instant::now() < deadline,
                    "response deadline: method={method}"
                );
                thread::sleep(Duration::from_millis(2));
            }
        }
        bail!("notification count exceeds bound")
    }
    pub fn application(&mut self, tool: &str, arguments: Value) -> Result<Value> {
        let response = self.request("tools/call", json!({"name":tool,"arguments":arguments}))?;
        let result = &response["result"];
        let text: Value = serde_json::from_str(
            result["content"][0]["text"]
                .as_str()
                .context("tool content text")?,
        )?;
        if let Some(structured) = result.get("structuredContent") {
            ensure!(*structured == text, "structured/text content disagreement");
        }
        ensure!(
            result["isError"].as_bool() == Some(text["ok"] != true),
            "tool error envelope disagreement"
        );
        Ok(text)
    }
    pub fn tool(&mut self, tool: &str, arguments: Value) -> Result<Value> {
        let application = self.application(tool, arguments)?;
        ensure!(application["ok"] == true, "{tool} rejected: {application}");
        Ok(application)
    }
    pub fn close(&mut self) -> Result<ExitStatus> {
        self.close_bounded(Duration::from_secs(if self.prefix == "fingerprint" {
            60
        } else {
            8
        }))
    }
    pub fn close_bounded(&mut self, budget: Duration) -> Result<ExitStatus> {
        if let Some(input) = self.input.as_mut() {
            input.close()?;
        }
        self.input.take();
        let deadline = (Instant::now() + budget).min(self.deadline);
        loop {
            self.pump()?;
            if self.child_status.is_none() {
                let child = self.child.as_mut().context("controller child owner")?;
                if child.exited_reserved()? && self.output_eof && self.errors_eof {
                    self.child_status = child.try_reap_and_release()?;
                }
            }
            if let Some(status) = self.child_status {
                if let Some(output) = self.output.as_mut() {
                    output.cancel_and_complete()?;
                }
                if let Some(errors) = self.errors.as_mut() {
                    errors.cancel_and_complete()?;
                }
                self.output.take();
                self.errors.take();
                self.child.take();
                return Ok(status);
            }
            ensure!(
                Instant::now() < deadline,
                "controller join/pipe EOF deadline"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn retain_transcript(&self) {
        let _ = std::fs::write(
            self.evidence.join(format!("{}-stdout.jsonl", self.prefix)),
            &self.stdout,
        );
        let _ = std::fs::write(
            self.evidence.join(format!("{}-stderr.log", self.prefix)),
            &self.stderr,
        );
        let _ = std::fs::write(
            self.evidence
                .join(format!("{}-requests.jsonl", self.prefix)),
            &self.requests,
        );
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        if self.child.is_some() {
            if self.nonblocking {
                if let Some((instance, handle)) = self.active.take() {
                    let _=self.request("tools/call",json!({"name":"run_cancel","arguments":{"server_instance":instance,"handle":handle}}));
                }
                let _ = self.close();
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut complete = false;
            while Instant::now() < deadline {
                let input = self.input.as_mut().is_none_or(|p| p.close().is_ok());
                let output = self
                    .output
                    .as_mut()
                    .is_none_or(|p| p.cancel_and_complete().is_ok());
                let errors = self
                    .errors
                    .as_mut()
                    .is_none_or(|p| p.cancel_and_complete().is_ok());
                let child = self.child_status.is_some()
                    || self.child.as_mut().is_none_or(|c| {
                        c.terminate_owned().is_ok()
                            && matches!(c.try_reap_and_release(), Ok(Some(_)))
                    });
                if input && output && errors && child {
                    complete = true;
                    break;
                }
                thread::sleep(Duration::from_millis(2));
            }
            if complete {
                self.input.take();
                self.output.take();
                self.errors.take();
                self.child.take();
            } else {
                // Preserve unresolved original job/group and asynchronous endpoint storage.
                let _ = Box::leak(Box::new((
                    self.child.take(),
                    self.input.take(),
                    self.output.take(),
                    self.errors.take(),
                )));
            }
        }
        self.retain_transcript();
    }
}

use serde::Serialize;
use sha2::{Digest as _, Sha256};
use theseus::{
    native::{
        integration::{model::*, recipe::ValidatedIntegrationRecipe},
        storage,
    },
    report,
};
use theseus_native_protocol::v2;
#[path = "native_service_views.rs"]
pub mod saved_views;

pub struct Source<'a> {
    pub controller: &'a Path,
    pub graph: &'a Path,
    pub project: &'a Path,
    pub recipe_path: &'a Path,
    pub recipe: &'a ValidatedIntegrationRecipe,
    pub authority: &'a v2::ServiceAuthority,
    pub output: &'a Path,
}
pub struct SavedRun {
    pub report: NativeIntegrationReport,
    pub directory: PathBuf,
    report_bytes: Vec<u8>,
    journal_bytes: Vec<u8>,
    evidence_root: PathBuf,
    pub projection: Value,
}
pub fn fingerprint(source: &Source<'_>) -> Result<String> {
    source.authority.validate()?;
    source.recipe.recipe.validate()?;
    ensure!(
        source.controller.is_absolute() && source.controller.is_file(),
        "private frozen controller required"
    );
    let mut command = Command::new(source.controller);
    command
        .arg("--fixture-graph")
        .arg(source.graph)
        .arg("--fixture-fingerprint");
    let mut child = Controller::spawn(&mut command, source.project, "fingerprint")?;
    ensure!(
        child.close()?.success() && child.stderr.is_empty(),
        "pure fingerprint query failed"
    );
    let fingerprint = std::str::from_utf8(&child.stdout)?.trim().to_owned();
    ensure!(
        fingerprint.len() == 64
            && fingerprint
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "canonical fingerprint"
    );
    let graph: Value = serde_json::from_slice(&std::fs::read(source.graph)?)?;
    for field in [
        "launch",
        "config",
        "helper",
        "provider",
        "controller",
        "base",
        "target",
    ] {
        let pin = graph
            .get(field)
            .context("complete fixture graph source pin")?;
        let path = pin["path"].as_str().context("graph source pin")?;
        ensure!(
            pin["sha256"] == format!("{:x}", Sha256::digest(std::fs::read(path)?)),
            "original {field} source pin"
        );
    }
    ensure!(
        graph["controller"]["path"]
            .as_str()
            .is_some_and(|path| Path::new(path) == source.controller),
        "spawn uses the graph's private frozen controller"
    );
    ensure!(
        source.recipe.digest == format!("{:x}", Sha256::digest(std::fs::read(source.recipe_path)?)),
        "exact original recipe bytes"
    );
    Ok(fingerprint)
}
fn saved_bytes(directory: &Path, name: &str) -> Result<Vec<u8>> {
    use theseus_host::fs::{HostDir, RelativePath, RootPolicy, fingerprint};
    let root = HostDir::open(directory, RootPolicy::AllComponentsNoFollow)?;
    let leaf = RelativePath::parse(name)?;
    let mut file = root.open_file(&leaf)?;
    let before = fingerprint(&file)?;
    let mut bytes = vec![];
    (&mut file)
        .take(theseus::native::model::MAX_ARTIFACT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= theseus::native::model::MAX_ARTIFACT_BYTES,
        "bounded saved artifact"
    );
    ensure!(
        before == fingerprint(&file)?
            && before == fingerprint(&root.open_file(&leaf)?)?
            && root.same_object(&HostDir::open(
                directory,
                RootPolicy::AllComponentsNoFollow
            )?)?,
        "saved bytes changed while read"
    );
    Ok(bytes)
}
pub fn saved_run(
    source: &Source<'_>,
    directory: &Path,
    expected_fingerprint: &str,
) -> Result<SavedRun> {
    let store = report::open_existing(directory)?;
    let (NativeReportVersion::IntegrationV2(report), NativeJournalVersion::IntegrationV2(journal)) =
        storage::load_native_versioned(&store)?
    else {
        bail!("schema2 pair required")
    };
    let graph: Value = serde_json::from_slice(&std::fs::read(source.graph)?)?;
    ensure!(
        report.recipe_digest == source.recipe.digest
            && report.core_recipe_digest == source.recipe.core.digest,
        "own full/core source binding"
    );
    ensure!(
        report.manifest_digest == graph["manifest_digest"]
            && report.core_manifest_digest == graph["core_manifest_digest"]
            && report.integration_manifest_digest == graph["integration_manifest_digest"],
        "own component/full manifest binding"
    );
    ensure!(
        report.manifest_digest == source.recipe.full_manifest_digest()?
            && journal.launch_fingerprint.as_deref() == Some(expected_fingerprint),
        "own composed manifest and exact authority fingerprint"
    );
    ensure!(
        journal.ownership.len() == 1
            && journal.ownership[0].ownership_id == report.integrations[0].ownership_id
            && journal.ownership[0].scenario_id == report.integrations[0].scenario_id,
        "original owned scenario binding"
    );
    let report_bytes = saved_bytes(directory, theseus::native::model::REPORT_FILENAME)?;
    let journal_bytes = saved_bytes(directory, theseus::native::model::JOURNAL_FILENAME)?;
    let projection = theseus::cli::fixture_saved_native_projection(directory)?;
    saved_views::assert_service_views(report.clone(), directory);
    Ok(SavedRun {
        report,
        directory: directory.to_owned(),
        report_bytes,
        journal_bytes,
        evidence_root: source.project.to_owned(),
        projection,
    })
}
pub fn run_cli(source: &Source<'_>) -> Result<SavedRun> {
    ensure!(!source.output.exists(), "fresh CLI fixture context");
    let expected = fingerprint(source)?;
    let mut command = Command::new(source.controller);
    command
        .arg("--fixture-graph")
        .arg(source.graph)
        .args(["--no-animation", "run", "--headless", "--recipe"])
        .arg(source.recipe_path)
        .arg("--native-target")
        .arg(source.project.join("launch.json"))
        .args(["--scenario", "clean"]);
    let mut child = Controller::spawn(&mut command, source.project, "cli")?;
    let status = child.close_bounded(Duration::from_secs(150))?;
    ensure!(
        status.success(),
        "public CLI failed: {}",
        String::from_utf8_lossy(&child.stderr)
    );
    let directories = std::fs::read_dir(source.output)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    ensure!(directories.len() == 1, "exactly one CLI owned run");
    let saved = saved_run(source, &directories[0], &expected)?;
    ensure!(
        saved.report.outcome == theseus::model::RunOutcome::Passed,
        "positive CLI lifecycle must pass"
    );
    Ok(saved)
}
struct RejectingLoader(std::sync::atomic::AtomicUsize);
impl theseus::target_authority::TargetAuthorityLoader for RejectingLoader {
    fn load(
        &self,
        _: &theseus::target_authority::RetainedLaunchFile,
    ) -> Result<theseus::native::launch::LaunchAuthority> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        bail!("saved projection requested execution authority")
    }
}
pub fn repeat_saved_without_authority(saved: &SavedRun) -> Result<()> {
    use clap::Parser;
    let loader = RejectingLoader(std::sync::atomic::AtomicUsize::new(0));
    let args = vec![
        std::ffi::OsString::from("theseus"),
        "--no-animation".into(),
        "report".into(),
        saved.directory.as_os_str().to_owned(),
        "--json".into(),
    ];
    ensure!(
        theseus::cli::execute_with_loader(theseus::cli::Cli::try_parse_from(args)?, &loader)? == 0,
        "saved CLI report failed"
    );
    ensure!(
        loader.0.load(std::sync::atomic::Ordering::SeqCst) == 0,
        "rejecting authority loader was consulted"
    );
    saved_views::assert_service_views(saved.report.clone(), &saved.directory);
    ensure!(
        theseus::cli::fixture_saved_native_projection(&saved.directory)? == saved.projection,
        "saved public projection changed offline"
    );
    ensure!(
        saved_bytes(&saved.directory, theseus::native::model::REPORT_FILENAME)?
            == saved.report_bytes
            && saved_bytes(&saved.directory, theseus::native::model::JOURNAL_FILENAME)?
                == saved.journal_bytes,
        "offline views mutated saved bytes"
    );
    std::fs::write(
        saved
            .evidence_root
            .join("offline-rejecting-loader-proof.json"),
        serde_json::to_vec_pretty(
            &json!({"authority_load_requests":loader.0.load(std::sync::atomic::Ordering::SeqCst),"saved_cli_exit":0,"report_sha256":format!("{:x}",Sha256::digest(&saved.report_bytes)),"journal_sha256":format!("{:x}",Sha256::digest(&saved.journal_bytes)),"saved_revision":saved.projection["saved"]["revision"]}),
        )?,
    )?;
    Ok(())
}
fn provenance<T>(value: &IntegrationProvenance<T>) -> &'static str {
    match value {
        IntegrationProvenance::Observed { .. } => "observed",
        IntegrationProvenance::ManagerReported { .. } => "manager_reported",
        IntegrationProvenance::ApplicationReported { .. } => "application_reported",
        IntegrationProvenance::ProviderReported { .. } => "provider_reported",
        IntegrationProvenance::Unavailable { .. } => "unavailable",
    }
}
fn disposition(value: &theseus::native::model::Disposition) -> &'static str {
    use theseus::native::model::Disposition::*;
    match value {
        NotStarted => "not_started",
        Complete { .. } => "complete",
        Unknown { .. } => "unknown",
        Failed { .. } => "failed",
    }
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct NormalizedFact {
    phase: IntegrationPhase,
    definition: Option<DefinitionKey>,
    complete: bool,
    provenance: &'static str,
    state: &'static str,
    registration: Option<bool>,
    configuration: Option<bool>,
    running: Option<bool>,
    readiness: Option<NormalizedReadiness>,
}
#[derive(Debug, PartialEq, Eq, Serialize)]
struct NormalizedReadiness {
    state: v2::ReadinessState,
    authenticated: bool,
    application_provenance: Option<&'static str>,
    initialized: Option<bool>,
    peer_observed: bool,
    artifact_matches: bool,
    version_matches: bool,
    data_matches: bool,
    configuration_matches: bool,
}
fn normalized_fact(observation: &BoundServiceObservation) -> NormalizedFact {
    let state = match &observation.state {
        IntegrationProvenance::Observed { value, .. }
        | IntegrationProvenance::ManagerReported { value, .. }
        | IntegrationProvenance::ApplicationReported { value, .. }
        | IntegrationProvenance::ProviderReported { value, .. } => Some(value),
        IntegrationProvenance::Unavailable { .. } => None,
    };
    let (state, registration, configuration, running) = match state {
        Some(ServiceState::Absent) => ("absent", None, None, None),
        Some(ServiceState::Present {
            registered,
            configured,
            running,
            ..
        }) => (
            "present",
            Some(*registered),
            Some(*configured),
            Some(*running),
        ),
        Some(ServiceState::Unknown { .. }) => ("unknown", None, None, None),
        None => ("unavailable", None, None, None),
    };
    let readiness = observation
        .readiness
        .as_ref()
        .map(|ready| NormalizedReadiness {
            state: ready.state,
            authenticated: ready.authenticated_instance,
            application_provenance: ready
                .response_digest
                .as_ref()
                .map(|_| "application_reported"),
            initialized: ready.initialized,
            peer_observed: ready.peer_uid.is_some() || ready.peer_sid.is_some(),
            artifact_matches: ready.artifact_sha256.as_ref()
                == Some(&ready.expectation.artifact_sha256),
            version_matches: ready.version_literal.as_ref()
                == Some(&ready.expectation.version_literal),
            data_matches: ready.data_sha256.as_ref() == Some(&ready.expectation.data_sha256),
            configuration_matches: ready.configuration_digest.as_ref()
                == Some(&ready.expectation.configuration_digest),
        });
    NormalizedFact {
        phase: observation.binding.phase,
        definition: observation.binding.definition.clone(),
        complete: observation.binding.complete,
        provenance: provenance(&observation.state),
        state,
        registration,
        configuration,
        running,
        readiness,
    }
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct NormalizedCheck {
    phase: IntegrationPhase,
    role: Option<DefinitionRole>,
    artifact: Option<ArtifactRole>,
    predicate: IntegrationPredicate,
    origin: CheckOrigin,
    outcome: theseus::model::CheckOutcome,
    facts: Vec<NormalizedFact>,
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct NormalizedScenario {
    scenario: theseus::model::ScenarioKind,
    outcome: theseus::model::RunOutcome,
    checks: Vec<NormalizedCheck>,
    catalog_stages: Vec<(
        Option<theseus::model::StepName>,
        Option<theseus::model::Checkpoint>,
        theseus::model::StageOutcome,
    )>,
    dispositions: [&'static str; 10],
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct NormalizedRun {
    outcome: theseus::model::RunOutcome,
    identities: Vec<&'static str>,
    scenarios: Vec<NormalizedScenario>,
}
fn sort_facts<T: Serialize + PartialEq>(values: &mut Vec<T>) {
    values.sort_by_cached_key(|v| serde_json::to_string(v).expect("closed fact serialization"));
    values.dedup();
}
pub fn normalize(report: &NativeIntegrationReport) -> Result<NormalizedRun> {
    report.validate()?;
    let mut scenarios = vec![];
    for integration in &report.integrations {
        let catalog = report
            .catalog
            .scenarios
            .iter()
            .find(|s| s.id == integration.scenario_id)
            .context("own scenario reference")?;
        let mut checks = vec![];
        for check in &integration.checks {
            let mut facts = integration
                .services
                .iter()
                .flat_map(|s| &s.observations)
                .filter(|o| check.observation_ids.contains(&o.binding.id))
                .map(normalized_fact)
                .collect::<Vec<_>>();
            sort_facts(&mut facts);
            checks.push(NormalizedCheck {
                phase: check.phase,
                role: check.role,
                artifact: check.artifact,
                predicate: check.predicate.clone(),
                origin: check.origin,
                outcome: check.outcome,
                facts,
            });
        }
        sort_facts(&mut checks);
        let d = &integration.dispositions;
        let c = &catalog.dispositions;
        scenarios.push(NormalizedScenario {
            scenario: catalog.scenario,
            outcome: catalog.outcome,
            checks,
            catalog_stages: catalog
                .stages
                .iter()
                .map(|s| (s.step, s.checkpoint, s.outcome))
                .collect(),
            dispositions: [
                disposition(&d.helper_worker),
                disposition(&d.registration),
                disposition(&d.manager_action),
                disposition(&d.broker),
                disposition(&d.primary),
                disposition(&d.descendant_scope),
                disposition(&c.local),
                disposition(&c.remote),
                disposition(&c.release),
                disposition(&c.reset),
            ],
        });
    }
    sort_facts(&mut scenarios);
    let mut identities = report
        .catalog
        .identities
        .iter()
        .map(|identity| match identity {
            theseus::native::model::Provenance::Observed { .. } => "observed",
            theseus::native::model::Provenance::ProviderReported { .. } => "provider_reported",
            theseus::native::model::Provenance::Unavailable { .. } => "unavailable",
        })
        .collect::<Vec<_>>();
    identities.sort();
    identities.dedup();
    Ok(NormalizedRun {
        outcome: report.outcome,
        identities,
        scenarios,
    })
}

#[cfg(target_os = "linux")]
use std::{
    ffi::OsString,
    fs::File,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Component,
};
#[cfg(target_os = "linux")]
const DIRECTORY_FLAGS: rustix::fs::OFlags = rustix::fs::OFlags::RDONLY
    .union(rustix::fs::OFlags::DIRECTORY)
    .union(rustix::fs::OFlags::NOFOLLOW)
    .union(rustix::fs::OFlags::CLOEXEC);
#[cfg(target_os = "linux")]
fn retained_directory(path: &Path) -> Result<File> {
    ensure!(
        path.is_absolute(),
        "explicit absolute private endpoint parent required"
    );
    let mut file = File::from(rustix::fs::open(
        "/",
        DIRECTORY_FLAGS,
        rustix::fs::Mode::empty(),
    )?);
    for component in path.components() {
        match component {
            Component::RootDir => (),
            Component::Normal(name) => {
                file = File::from(rustix::fs::openat(
                    &file,
                    name,
                    DIRECTORY_FLAGS,
                    rustix::fs::Mode::empty(),
                )?)
            }
            _ => bail!("unsafe private endpoint parent"),
        }
    }
    Ok(file)
}
#[cfg(target_os = "linux")]
fn identity(file: &File) -> Result<(u64, u64)> {
    let md = file.metadata()?;
    Ok((md.dev(), md.ino()))
}
#[cfg(target_os = "linux")]
fn socket_leaf(parent: &File, name: &std::ffi::OsStr) -> Result<File> {
    let file = File::from(rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?);
    ensure!(
        file.metadata()?.file_type().is_socket(),
        "private endpoint must be an original socket"
    );
    Ok(file)
}
/// Acts only on the supplied whole-private fixture's exact socket name.
/// Both descriptors stay held; restoration never replaces an unexpected name.
#[cfg(target_os = "linux")]
pub struct PrivateEndpointGuard {
    parent: File,
    original: File,
    parent_path: PathBuf,
    path: PathBuf,
    name: OsString,
    disabled: OsString,
    evidence: PathBuf,
    restored: bool,
}
#[cfg(target_os = "linux")]
impl PrivateEndpointGuard {
    pub fn disable(authority: &v2::ServiceAuthority, evidence: &Path) -> Result<Self> {
        authority.validate()?;
        let v2::ServiceAuthority::Systemd {
            bus_endpoint: v2::LocalEndpoint::UnixPath { path },
            manager_context:
                v2::ManagerContext::WholePrivate {
                    provider_receipt_required: true,
                    ..
                },
            expected_peer_uid,
            expected_manager_uid,
            ..
        } = authority
        else {
            bail!("only an explicitly granted private systemd endpoint can be disabled")
        };
        let uid = rustix::process::geteuid().as_raw();
        ensure!(
            *expected_manager_uid == uid,
            "fixture manager must be this dedicated account"
        );
        ensure!(
            path != &format!("/run/user/{uid}/bus")
                && path != "/run/dbus/system_bus_socket"
                && path != "/var/run/dbus/system_bus_socket",
            "normal user/system bus is never a fixture endpoint"
        );
        Self::disable_owned_socket(Path::new(path), evidence, *expected_peer_uid)
    }
    fn disable_path(path: &Path, evidence: &Path) -> Result<Self> {
        Self::disable_owned_socket(path, evidence, rustix::process::geteuid().as_raw())
    }
    fn disable_owned_socket(
        path: &Path,
        evidence: &Path,
        expected_socket_uid: u32,
    ) -> Result<Self> {
        let parent_path = path.parent().context("endpoint parent")?.to_owned();
        let parent = retained_directory(&parent_path)?;
        let metadata = parent.metadata()?;
        ensure!(
            metadata.uid() == rustix::process::geteuid().as_raw()
                && metadata.mode() & 0o7777 == 0o700,
            "owned mode0700 private endpoint directory required (uid={}, expected_uid={}, mode={:o})",
            metadata.uid(),
            rustix::process::geteuid().as_raw(),
            metadata.mode() & 0o7777
        );
        let name = path.file_name().context("endpoint basename")?.to_owned();
        let original = socket_leaf(&parent, &name)?;
        ensure!(
            original.metadata()?.uid() == expected_socket_uid,
            "original private socket must match the explicitly granted bus peer principal"
        );
        let mut random = [0; 16];
        theseus_host::random_bytes(&mut random)?;
        let disabled = OsString::from(format!(
            "theseus-offline-{}",
            random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ));
        let mut guard = Self {
            parent,
            original,
            parent_path,
            path: path.to_owned(),
            name,
            disabled,
            evidence: evidence.to_owned(),
            restored: true,
        };
        guard.verify_parent()?;
        ensure!(
            identity(&socket_leaf(&guard.parent, &guard.name)?)? == identity(&guard.original)?,
            "endpoint replaced before isolation"
        );
        rustix::fs::renameat_with(
            &guard.parent,
            &guard.name,
            &guard.parent,
            &guard.disabled,
            rustix::fs::RenameFlags::NOREPLACE,
        )?;
        guard.restored = false;
        ensure!(
            identity(&socket_leaf(&guard.parent, &guard.disabled)?)? == identity(&guard.original)?,
            "endpoint changed while isolating original name"
        );
        guard.assert_unavailable()?;
        guard.record("disabled")?;
        Ok(guard)
    }
    fn verify_parent(&self) -> Result<()> {
        ensure!(
            identity(&retained_directory(&self.parent_path)?)? == identity(&self.parent)?,
            "private endpoint parent replaced"
        );
        ensure!(
            self.parent.metadata()?.mode() & 0o7777 == 0o700,
            "private endpoint permissions changed"
        );
        Ok(())
    }
    fn absent(&self) -> Result<()> {
        match rustix::fs::statat(
            &self.parent,
            &self.name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Err(rustix::io::Errno::NOENT) => Ok(()),
            Ok(_) => bail!("unexpected replacement at disabled endpoint"),
            Err(error) => Err(error.into()),
        }
    }
    pub fn assert_unavailable(&self) -> Result<()> {
        self.verify_parent()?;
        self.absent()?;
        ensure!(
            identity(&socket_leaf(&self.parent, &self.disabled)?)? == identity(&self.original)?,
            "retained endpoint identity changed"
        );
        match std::os::unix::net::UnixStream::connect(&self.path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
            Ok(_) => bail!("disabled private endpoint unexpectedly connected"),
        }
    }
    fn record(&self, state: &str) -> Result<()> {
        let (device, inode) = identity(&self.original)?;
        std::fs::write(
            &self.evidence,
            serde_json::to_vec_pretty(
                &json!({"state":state,"path":self.path,"disabled_name":self.disabled,"original_device":device,"original_inode":inode,"parent_device_inode":identity(&self.parent)?,"parent_mode":self.parent.metadata()?.mode()&0o7777,"owner_uid":self.original.metadata()?.uid()}),
            )?,
        )?;
        std::fs::copy(
            &self.evidence,
            self.evidence.with_extension(format!("{state}.json")),
        )?;
        Ok(())
    }
    pub fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        self.verify_parent()?;
        self.absent()?;
        ensure!(
            identity(&socket_leaf(&self.parent, &self.disabled)?)? == identity(&self.original)?,
            "only original private endpoint may be restored"
        );
        rustix::fs::renameat_with(
            &self.parent,
            &self.disabled,
            &self.parent,
            &self.name,
            rustix::fs::RenameFlags::NOREPLACE,
        )?;
        ensure!(
            identity(&socket_leaf(&self.parent, &self.name)?)? == identity(&self.original)?,
            "restored endpoint differs from original"
        );
        self.restored = true;
        self.record("restored")?;
        Ok(())
    }
}
#[cfg(target_os = "linux")]
impl Drop for PrivateEndpointGuard {
    fn drop(&mut self) {
        if !self.restored
            && let Err(error) = self.restore()
        {
            eprintln!("private endpoint retained without overwrite: {error:#}");
            if !thread::panicking() {
                panic!("failed to restore original fixture endpoint: {error:#}");
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    fn private_root() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        root
    }
    #[test]
    fn normalization_excludes_context_but_preserves_phase_predicate_and_provenance_states() {
        let mut original = BoundServiceObservation {
            platform: None,
            readiness: None,
            binding: ObservationBinding {
                id: "one".into(),
                run_id: "run-one".into(),
                scenario_id: "scenario-one".into(),
                ownership_id: "owner-one".into(),
                lease: "a".repeat(32),
                slot: ResourceSlot::new("service").unwrap(),
                request_id: "request-one".into(),
                action: None,
                binding_digest: "b".repeat(64),
                source_profile: "native_integration_v1".into(),
                source_version: "systemd_dbus_private_v1".into(),
                phase: IntegrationPhase::Configured,
                definition: Some(DefinitionKey {
                    role: DefinitionRole::Configured,
                    artifact: ArtifactRole::Target,
                }),
                sequence: 1,
                started_ms: 0,
                finished_ms: 1,
                complete: true,
            },
            state: IntegrationProvenance::ManagerReported {
                value: ServiceState::Present {
                    registered: true,
                    configured: true,
                    running: true,
                    definition_digest: "c".repeat(64),
                    origin: "/context-one/unit".into(),
                },
                manager: ":one".into(),
            },
        };
        let baseline = normalized_fact(&original);
        original.binding.id = "two".into();
        original.binding.run_id = "run-two".into();
        original.binding.ownership_id = "owner-two".into();
        original.binding.lease = "d".repeat(32);
        original.binding.binding_digest = "e".repeat(64);
        original.binding.sequence = 9;
        original.binding.started_ms = 100;
        original.binding.finished_ms = 110;
        original.state = IntegrationProvenance::ManagerReported {
            value: ServiceState::Present {
                registered: true,
                configured: true,
                running: true,
                definition_digest: "f".repeat(64),
                origin: "/context-two/unit".into(),
            },
            manager: ":two".into(),
        };
        assert_eq!(baseline, normalized_fact(&original));
        let mut running_changed = original.clone();
        if let IntegrationProvenance::ManagerReported {
            value: ServiceState::Present { running, .. },
            ..
        } = &mut running_changed.state
        {
            *running = false;
        }
        assert_ne!(baseline, normalized_fact(&running_changed));
        let mut phase_changed = original.clone();
        phase_changed.binding.phase = IntegrationPhase::BeforeUninstall;
        assert_ne!(baseline, normalized_fact(&phase_changed));
        original.state = IntegrationProvenance::Unavailable {
            reason: "unproved".into(),
        };
        assert_ne!(baseline, normalized_fact(&original));
        original.state = IntegrationProvenance::ManagerReported {
            value: ServiceState::Absent,
            manager: ":two".into(),
        };
        let absent = normalized_fact(&original);
        assert_eq!(absent.running, None);
        original.state = IntegrationProvenance::ManagerReported {
            value: ServiceState::Unknown {
                reason: "unproved".into(),
            },
            manager: ":two".into(),
        };
        assert_ne!(absent, normalized_fact(&original));
        let make_check = |outcome| NormalizedCheck {
            phase: IntegrationPhase::Configured,
            role: Some(DefinitionRole::Configured),
            artifact: Some(ArtifactRole::Target),
            predicate: IntegrationPredicate::Service(ServicePredicate::Running),
            origin: CheckOrigin::SutPostcondition,
            outcome,
            facts: vec![],
        };
        assert_ne!(
            make_check(theseus::model::CheckOutcome::Passed),
            make_check(theseus::model::CheckOutcome::Incomplete)
        );
    }
    #[test]
    fn private_endpoint_guard_holds_original_and_restores_same_socket() {
        let root = private_root();
        let path = root.path().join("bus");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let before = std::fs::symlink_metadata(&path).unwrap();
        let mut guard =
            PrivateEndpointGuard::disable_path(&path, &root.path().join("guard.json")).unwrap();
        guard.assert_unavailable().unwrap();
        guard.restore().unwrap();
        let after = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        let _connection = std::os::unix::net::UnixStream::connect(&path).unwrap();
        drop(listener);
    }
    #[test]
    fn private_endpoint_guard_never_overwrites_replacement_socket() {
        let root = private_root();
        let path = root.path().join("bus");
        let _original = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let mut guard =
            PrivateEndpointGuard::disable_path(&path, &root.path().join("guard.json")).unwrap();
        let replacement = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let replacement_identity = std::fs::symlink_metadata(&path).unwrap().ino();
        assert!(guard.restore().is_err());
        assert_eq!(
            std::fs::symlink_metadata(&path).unwrap().ino(),
            replacement_identity
        );
        drop(replacement);
        std::fs::remove_file(&path).unwrap();
        guard.restore().unwrap();
    }
}

#[cfg(test)]
mod portable_command_tests {
    use super::*;
    #[test]
    fn portable_frontend_keeps_literal_argv_cwd_and_inherited_test_environment() {
        let executable = std::env::current_exe().unwrap();
        let cwd = std::env::current_dir().unwrap();
        let mut command = Command::new(&executable);
        command
            .args(["two words", r"literal\value"])
            .current_dir(&cwd)
            .env("THESEUS_FRONTEND_LITERAL", "kept exactly")
            .env_remove("THESEUS_FRONTEND_REMOVED");
        let native = inherited_command(&command);
        assert_eq!(native.program, executable.as_os_str());
        assert_eq!(
            native.args,
            [
                std::ffi::OsString::from("two words"),
                std::ffi::OsString::from(r"literal\value")
            ]
        );
        assert_eq!(native.cwd.as_deref(), Some(cwd.as_path()));
        let ChildEnvironment::Replace(entries) = native.environment else {
            panic!("explicit snapshot of inherited test environment");
        };
        assert!(
            entries
                .iter()
                .any(|(key, value)| key == "THESEUS_FRONTEND_LITERAL" && value == "kept exactly")
        );
        assert!(
            !entries
                .iter()
                .any(|(key, _)| key == "THESEUS_FRONTEND_REMOVED")
        );
        if let Some(value) = std::env::var_os("PATH") {
            assert!(
                entries
                    .iter()
                    .any(|(key, actual)| key == "PATH" && actual == &value)
            );
        }
    }
}
