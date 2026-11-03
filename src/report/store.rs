use super::bounded::LimitedWriter;
use crate::{
    model::{
        Checkpoint, JOURNAL_SCHEMA_VERSION, LogReference, RunJournal, RunReport, ScenarioKind,
        StepName,
    },
    recipe::DEFAULT_LOG_LIMIT,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File},
    io::{BufWriter, Read, Write},
    path::{Component, Path, PathBuf},
};
pub use theseus_host::fs::FileFingerprint as LogFingerprint;
use theseus_host::fs::{HostDir, RelativePath, RootPolicy, fingerprint, read_exact_at};

const MAX_REPORT_BYTES: u64 = 64 * 1024 * 1024;
#[derive(Debug)]
pub struct ReportStore {
    root: PathBuf,
    directory: HostDir,
    #[cfg(test)]
    fail_child_creation: bool,
}
#[derive(Debug, Clone)]
pub struct LogRead {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

/// Raw stored-byte window, bounded by both the published reference and file length.
#[derive(Debug, Clone)]
pub struct LogPage {
    pub bytes: Vec<u8>,
    pub offset: u64,
    pub readable_total: u64,
    pub recorded_bytes: u64,
    pub current_bytes: u64,
    pub next_offset: Option<u64>,
    pub fingerprint: LogFingerprint,
}
/// Create exactly this run directory, never reusing an existing directory.
pub fn create(root: &Path) -> Result<ReportStore> {
    let directory = HostDir::create_private(root)
        .context("create fresh run directory (existing runs are never overwritten)")?;
    Ok(ReportStore {
        root: root.canonicalize()?,
        directory,
        #[cfg(test)]
        fail_child_creation: false,
    })
}
/// Reopen a run directory for atomic recovery-journal updates; refuses symlinks.
pub fn open_existing(root: &Path) -> Result<ReportStore> {
    open_store(root)
}
fn open_store(root: &Path) -> Result<ReportStore> {
    let directory = HostDir::open(root, RootPolicy::LeafNoFollow).context("open run directory")?;
    Ok(ReportStore {
        root: root.canonicalize()?,
        directory,
        #[cfg(test)]
        fail_child_creation: false,
    })
}
impl ReportStore {
    /// Classify every reserved family name through the retained directory.
    /// Only a completed native missing result proves absence.
    pub fn artifact_family(&self) -> Result<crate::native::storage::ArtifactFamily> {
        use crate::native::storage::ArtifactFamily as F;
        use theseus_host::fs::LeafOpenResult;
        let mut family = F::Empty;
        for (name, current) in [
            ("report.json", F::Docker),
            ("journal.json", F::Docker),
            ("native-report.json", F::Native),
            ("native-journal.json", F::Native),
            ("comparison.json", F::Comparison),
        ] {
            match self
                .directory
                .open_leaf_classified(std::ffi::OsStr::new(name))
            {
                LeafOpenResult::Opened(_) => {
                    ensure!(
                        family == F::Empty || family == current,
                        "mixed artifact families"
                    );
                    family = current;
                }
                LeafOpenResult::MissingSettled => (),
                LeafOpenResult::FailedSettled(error) => {
                    return Err(error).context("classify retained artifact family");
                }
                LeafOpenResult::Unsettled(owner) => {
                    return Err(owner).context("unsettled artifact open");
                }
            }
        }
        Ok(family)
    }
    /// Retain a private native intent namespace under this run's directory owner.
    /// Reopening never prepares, republishes, or manufactures an original intent.
    pub fn intent_store(&self) -> Result<theseus_native_helper_store::IntentStore> {
        crate::native::storage::require_family(
            self.artifact_family()?,
            crate::native::storage::ArtifactFamily::Native,
        )?;
        let name = std::ffi::OsStr::new("native-intents");
        let directory = match self.directory.create_child(name) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.directory.open_child_name(name)?
            }
            Err(error) => return Err(error).context("create retained native intent directory"),
        };
        Ok(theseus_native_helper_store::IntentStore::new(directory))
    }
    pub fn open_intent_store(&self) -> Result<theseus_native_helper_store::IntentStore> {
        crate::native::storage::require_family(
            self.artifact_family()?,
            crate::native::storage::ArtifactFamily::Native,
        )?;
        Ok(theseus_native_helper_store::IntentStore::new(
            self.directory
                .open_child_name(std::ffi::OsStr::new("native-intents"))?,
        ))
    }
    pub fn native_report_path(&self) -> PathBuf {
        self.root.join(crate::native::model::REPORT_FILENAME)
    }
    pub fn save_native_report(&self, report: &crate::native::model::NativeReport) -> Result<()> {
        crate::native::storage::validate_report(report)?;
        crate::native::storage::require_family(
            self.artifact_family()?,
            crate::native::storage::ArtifactFamily::Native,
        )?;
        self.atomic_json(crate::native::model::REPORT_FILENAME, report)
    }
    pub fn save_native_journal(&self, journal: &crate::native::model::NativeJournal) -> Result<()> {
        journal.validate()?;
        crate::native::storage::require_family(
            self.artifact_family()?,
            crate::native::storage::ArtifactFamily::Native,
        )?;
        self.atomic_json(crate::native::model::JOURNAL_FILENAME, journal)
    }
    pub fn load_native_report(&self) -> Result<crate::native::model::NativeReport> {
        crate::native::storage::require_family(
            self.artifact_family()?,
            crate::native::storage::ArtifactFamily::Native,
        )?;
        let report = crate::native::model::NativeReport::decode(
            &self.read_bytes(crate::native::model::REPORT_FILENAME)?,
        )?;
        crate::native::storage::validate_report(&report)?;
        Ok(report)
    }
    pub fn load_native_journal(&self) -> Result<crate::native::model::NativeJournal> {
        crate::native::storage::require_family(
            self.artifact_family()?,
            crate::native::storage::ArtifactFamily::Native,
        )?;
        crate::native::model::NativeJournal::decode(
            &self.read_bytes(crate::native::model::JOURNAL_FILENAME)?,
        )
    }
    pub fn load_selected_report(&self) -> Result<crate::engine::domain::StoredRun> {
        use crate::{engine::domain::StoredRun, native::storage::ArtifactFamily};
        match self.artifact_family()? {
            ArtifactFamily::Native => Ok(StoredRun::Native(self.load_native_report()?)),
            ArtifactFamily::Docker => Ok(StoredRun::Docker(decode_report(
                &self.read_bytes("report.json")?,
            )?)),
            _ => bail!("run report family missing or unsupported"),
        }
    }
    pub fn load_selected_journal(&self) -> Result<crate::engine::domain::StoredJournal> {
        use crate::{engine::domain::StoredJournal, native::storage::ArtifactFamily};
        match self.artifact_family()? {
            ArtifactFamily::Native => Ok(StoredJournal::Native(self.load_native_journal()?)),
            ArtifactFamily::Docker => Ok(StoredJournal::Docker(self.load_journal()?)),
            _ => bail!("run journal family missing or unsupported"),
        }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn report_path(&self) -> PathBuf {
        self.root.join("report.json")
    }
    pub fn save(&self, report: &RunReport) -> Result<()> {
        crate::native::storage::require_family(
            self.artifact_family()?,
            crate::native::storage::ArtifactFamily::Docker,
        )?;
        validate_report(report)?;
        if report.schema_version >= 4 {
            let mut normalized = report.clone();
            normalized.recompute_outcome();
            self.atomic_json("report.json", &normalized)
        } else {
            self.atomic_json("report.json", report)
        }
    }
    pub fn save_journal(&self, journal: &RunJournal) -> Result<()> {
        crate::native::storage::require_family(
            self.artifact_family()?,
            crate::native::storage::ArtifactFamily::Docker,
        )?;
        validate_journal(journal)?;
        self.atomic_json("journal.json", journal)
    }
    pub(crate) fn atomic_json(&self, name: &str, value: &impl Serialize) -> Result<()> {
        self.atomic_json_with_limit(name, value, MAX_REPORT_BYTES)
    }
    fn atomic_json_with_limit(&self, name: &str, value: &impl Serialize, limit: u64) -> Result<()> {
        // Preserve the typed publication error: replaced/unknown states must
        // never become an assertion that the previous report survived.
        self.directory
            .publish(std::ffi::OsStr::new(name), &mut |file| {
                let mut writer = LimitedWriter::new(BufWriter::new(file), limit);
                serde_json::to_writer_pretty(&mut writer, value).map_err(std::io::Error::other)?;
                writer.flush()
            })
            .context("publish report metadata")?;
        Ok(())
    }
    /// Create one stream exclusively through retained private directory authority.
    pub fn create_log(&self, relative: &str) -> Result<File> {
        log_components(relative)?;
        self.directory
            .create_file(&RelativePath::parse(relative)?)
            .context("create private command log")
    }
    pub fn write_log(&self, relative: &str, bytes: &[u8], limit: usize) -> Result<LogReference> {
        let mut file = self.create_log(relative)?;
        let kept = bytes.len().min(limit).min(DEFAULT_LOG_LIMIT as usize);
        file.write_all(&bytes[..kept])?;
        file.sync_all()?;
        Ok(LogReference {
            path: relative.into(),
            bytes: kept as u64,
            truncated: kept < bytes.len(),
        })
    }
    pub fn read_log(&self, relative: &str, limit: usize) -> Result<LogRead> {
        let file = self.open_log(relative)?;
        let limit = limit.min(DEFAULT_LOG_LIMIT as usize);
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        let truncated = bytes.len() > limit;
        bytes.truncate(limit);
        Ok(LogRead { bytes, truncated })
    }
    fn open_log(&self, relative: &str) -> Result<File> {
        log_components(relative)?;
        self.directory
            .open_file(&RelativePath::parse(relative)?)
            .context("open contained command log")
    }
    /// Read a raw page without reading its prefix. EOF itself is a valid offset.
    pub fn read_log_page(
        &self,
        reference: &LogReference,
        offset: u64,
        limit: usize,
    ) -> Result<LogPage> {
        ensure!(
            (1..=65536).contains(&limit),
            "log page limit must be 1..=65536"
        );
        ensure!(
            reference.bytes <= DEFAULT_LOG_LIMIT,
            "published log exceeds capture limit"
        );
        offset
            .checked_add(limit as u64)
            .context("log page offset overflow")?;
        let file = self.open_log(&reference.path)?;
        let before = fingerprint(&file)?;
        let total = reference.bytes.min(before.len).min(DEFAULT_LOG_LIMIT);
        ensure!(offset <= total, "log page offset exceeds readable length");
        let length = (total - offset).min(limit as u64) as usize;
        let mut bytes = vec![0; length];
        read_exact_at(&file, &mut bytes, offset)
            .context("read log page (file may have changed)")?;
        let after = fingerprint(&file)?;
        // Re-open via the pinned directory too: a renamed/replaced path is not a fresh page.
        let current = fingerprint(&self.open_log(&reference.path)?)?;
        ensure!(
            before == after && before == current,
            "log changed while reading page"
        );
        let end = offset + length as u64;
        Ok(LogPage {
            bytes,
            offset,
            readable_total: total,
            recorded_bytes: reference.bytes,
            current_bytes: before.len,
            next_offset: (end < total).then_some(end),
            fingerprint: before,
        })
    }
    pub(crate) fn read_bytes(&self, name: &str) -> Result<Vec<u8>> {
        let file = self
            .directory
            .open_file(&RelativePath::parse(name)?)
            .context("open report/journal")?;
        let mut bytes = Vec::new();
        file.take(MAX_REPORT_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_REPORT_BYTES,
            "report/journal exceeds size limit"
        );
        Ok(bytes)
    }
    pub(crate) fn read_json<T: DeserializeOwned>(&self, name: &str) -> Result<T> {
        serde_json::from_slice(&self.read_bytes(name)?).context("decode report/journal JSON")
    }
    /// Walk every directory component without following links. The resulting fd,
    /// not the display path, is the authority for all subsequent operations.
    pub(crate) fn safe_root(root: &Path, create: bool) -> Result<Self> {
        let root = if root.is_absolute() {
            root.to_path_buf()
        } else {
            std::env::current_dir()?.join(root)
        };
        let mut resolved = PathBuf::new();
        let mut parts = Vec::new();
        for component in root.components() {
            match component {
                Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
                Component::RootDir => resolved.push(component.as_os_str()),
                Component::CurDir => (),
                Component::Normal(part) => parts.push(part),
                _ => bail!("unsafe directory path"),
            }
        }
        let mut directory = HostDir::open(&resolved, RootPolicy::AllComponentsNoFollow)?;
        for part in parts {
            directory = if create {
                match directory.create_child(part) {
                    Ok(child) => child,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        directory.open_child_name(part)?
                    }
                    Err(error) => return Err(error).context("create durable recovery directory"),
                }
            } else {
                directory
                    .open_child_name(part)
                    .context("open no-follow directory")?
            };
            resolved.push(part);
        }
        Ok(Self {
            root: resolved,
            directory,
            #[cfg(test)]
            fail_child_creation: false,
        })
    }
    pub(crate) fn create_child(&self, name: &str) -> Result<Self> {
        let parts = log_components(name)?;
        ensure!(parts.len() == 1, "expected one directory component");
        #[cfg(test)]
        if self.fail_child_creation {
            bail!("injected child creation failure");
        }
        let directory = self.directory.create_child(std::ffi::OsStr::new(name))?;
        Ok(Self {
            root: self.root.join(name),
            directory,
            #[cfg(test)]
            fail_child_creation: false,
        })
    }
    pub(crate) fn open_child(&self, relative: &str) -> Result<Self> {
        log_components(relative)?;
        let directory = self.directory.open_child(&RelativePath::parse(relative)?)?;
        Ok(Self {
            root: self.root.join(relative),
            directory,
            #[cfg(test)]
            fail_child_creation: false,
        })
    }
    pub(crate) fn load_journal(&self) -> Result<RunJournal> {
        crate::native::storage::require_family(
            self.artifact_family()?,
            crate::native::storage::ArtifactFamily::Docker,
        )?;
        let bytes = self.read_bytes("journal.json")?;
        crate::native::storage::ensure_legacy_namespace(&bytes)?;
        let journal = serde_json::from_slice(&bytes)?;
        validate_journal(&journal)?;
        Ok(journal)
    }
}
fn log_components(relative: &str) -> Result<Vec<&str>> {
    ensure!(
        !relative.is_empty()
            && !relative.contains('\0')
            && !relative.starts_with('/')
            && !relative.ends_with('/'),
        "log reference must be a nonempty relative path"
    );
    let parts = relative.split('/').collect::<Vec<_>>();
    ensure!(
        parts
            .iter()
            .all(|p| !p.is_empty() && !matches!(*p, "." | "..") && !p.starts_with('.')),
        "unsafe log reference"
    );
    ensure!(
        !matches!(
            parts[0],
            "report.json"
                | "journal.json"
                | "native-report.json"
                | "native-journal.json"
                | "comparison.json"
                | "native-intents"
        ),
        "log reference conflicts with report metadata"
    );
    // Component checks are deliberately lexical, before any filesystem access.
    ensure!(
        Path::new(relative)
            .components()
            .all(|p| matches!(p, Component::Normal(_))),
        "unsafe log reference"
    );
    RelativePath::parse(relative)?;
    Ok(parts)
}
fn validate_report(report: &RunReport) -> Result<()> {
    ensure!(
        matches!(report.schema_version, 1..=8),
        "unsupported report schema version {}",
        report.schema_version
    );
    ensure!(
        report.schema_version >= 8 || report.scenarios.iter().all(|s| s.mcp.is_none()),
        "MCP lifecycle requires report schema version 8"
    );
    if report.schema_version < 7 {
        ensure!(
            !report
                .selected_scenarios
                .contains(&ScenarioKind::PluginLifecycle)
                && report
                    .scenarios
                    .iter()
                    .all(|s| s.scenario != ScenarioKind::PluginLifecycle
                        && s.plugin.is_none()
                        && s.stages.iter().all(|st| !matches!(
                            st.step,
                            Some(StepName::DisablePlugin | StepName::EnablePlugin)
                        ) && !matches!(
                            st.checkpoint,
                            Some(Checkpoint::PluginDisabled | Checkpoint::PluginEnabled)
                        ) && st.checks.iter().all(|c| !matches!(
                            c.checkpoint,
                            Checkpoint::PluginDisabled | Checkpoint::PluginEnabled
                        )))),
            "plugin lifecycle requires report schema version 7"
        );
    }
    if report.schema_version == 1 {
        ensure!(
            !report.selected_scenarios.contains(&ScenarioKind::Reinstall)
                && report.scenarios.iter().all(|scenario| {
                    scenario.scenario != ScenarioKind::Reinstall
                        && scenario.stages.iter().all(|stage| {
                            stage.step != Some(StepName::Reinstall)
                                && stage.checkpoint != Some(Checkpoint::Reinstalled)
                                && stage.checks.iter().all(|check| {
                                    check.checkpoint != Checkpoint::Reinstalled
                                        && check.comparison.is_none()
                                })
                        })
                }),
            "reinstall evidence and comparison references require report schema version 2"
        );
    }
    if report.schema_version < 3 {
        ensure!(
            !report.selected_scenarios.iter().any(|kind| matches!(
                kind,
                ScenarioKind::InterruptedInstall | ScenarioKind::InterruptedUpgrade
            )) && report.scenarios.iter().all(|scenario| !matches!(
                scenario.scenario,
                ScenarioKind::InterruptedInstall | ScenarioKind::InterruptedUpgrade
            )) && report
                .scenarios
                .iter()
                .all(|scenario| scenario.stages.iter().all(|stage| {
                    !matches!(
                        stage.step,
                        Some(
                            StepName::InterruptInstall
                                | StepName::RecoverInstall
                                | StepName::InterruptUpgrade
                                | StepName::RecoverUpgrade
                        )
                    ) && !matches!(
                        stage.checkpoint,
                        Some(Checkpoint::Interrupted | Checkpoint::Recovered)
                    ) && stage.checks.iter().all(|check| {
                        !matches!(
                            check.checkpoint,
                            Checkpoint::Interrupted | Checkpoint::Recovered
                        ) && check.comparison.as_ref().is_none_or(|reference| {
                            reference == &crate::model::ComparisonReference::reinstall()
                        })
                    }) && stage.restart.is_none()
                        && stage.commands.iter().all(|command| {
                            command.interruption.is_none()
                                && command.outcome != crate::model::CommandOutcome::Interrupted
                        })
                })),
            "interruption and restart evidence require report schema version 3"
        );
    }
    if report.schema_version < 4 {
        ensure!(
            !report.selected_scenarios.contains(&ScenarioKind::Downgrade)
                && report.scenarios.iter().all(|scenario| scenario.scenario
                    != ScenarioKind::Downgrade
                    && scenario.downgrade.is_none()
                    && scenario.stages.iter().all(|stage| {
                        !matches!(
                            stage.step,
                            Some(StepName::Downgrade | StepName::ExerciseDowngraded)
                        ) && !matches!(
                            stage.checkpoint,
                            Some(Checkpoint::Downgraded | Checkpoint::DowngradeRejected)
                        ) && stage.checks.iter().all(|check| {
                            !matches!(
                                check.checkpoint,
                                Checkpoint::Downgraded | Checkpoint::DowngradeRejected
                            ) && check.comparison.as_ref().is_none_or(|reference| {
                                reference.before_stage != "before_downgrade"
                                    && reference.after_stage != "downgraded"
                                    && reference.after_stage != "downgrade_rejected"
                            })
                        }) && stage.commands.iter().all(|command| {
                            command.rejection.is_none()
                                && command.outcome
                                    != crate::model::CommandOutcome::ExpectedRejection
                        })
                    })),
            "downgrade evidence requires report schema version 4"
        );
    }
    if report.schema_version < 6 {
        ensure!(
            !report
                .selected_scenarios
                .contains(&ScenarioKind::InterruptedRollback)
                && report
                    .scenarios
                    .iter()
                    .all(|s| s.scenario != ScenarioKind::InterruptedRollback
                        && s.interrupted_rollback.is_none()
                        && s.stages.iter().all(|stage| stage
                            .checks
                            .iter()
                            .all(|c| c.command_expectation.is_none()))),
            "interrupted rollback requires report schema version 6"
        );
    }
    if report.schema_version < 5 {
        ensure!(
            !report.selected_scenarios.contains(&ScenarioKind::Rollback)
                && report.scenarios.iter().all(|scenario| {
                    scenario.scenario != ScenarioKind::Rollback
                        && scenario.rollback.is_none()
                        && scenario.stages.iter().all(|stage| {
                            !crate::model::rollback_reference(&stage.name)
                                && !matches!(
                                    stage.step,
                                    Some(
                                        StepName::Backup
                                            | StepName::AttemptUpgrade
                                            | StepName::Rollback
                                            | StepName::ExerciseRolledBack
                                            | StepName::CleanupBackup
                                    )
                                )
                                && !matches!(
                                    stage.checkpoint,
                                    Some(
                                        Checkpoint::KnownGood
                                            | Checkpoint::BackupCreated
                                            | Checkpoint::UpgradeFailed
                                            | Checkpoint::RolledBack
                                            | Checkpoint::BackupRemoved
                                    )
                                )
                                && stage.commands.iter().all(|command| {
                                    command.expected_failure.is_none()
                                        && command.outcome
                                            != crate::model::CommandOutcome::ExpectedFailure
                                })
                                && stage.checks.iter().all(|check| {
                                    !matches!(
                                        check.checkpoint,
                                        Checkpoint::KnownGood
                                            | Checkpoint::BackupCreated
                                            | Checkpoint::UpgradeFailed
                                            | Checkpoint::RolledBack
                                            | Checkpoint::BackupRemoved
                                    ) && check.backup.is_none()
                                        && check.comparison.as_ref().is_none_or(|reference| {
                                            !crate::model::rollback_reference(
                                                &reference.before_stage,
                                            ) && !crate::model::rollback_reference(
                                                &reference.after_stage,
                                            )
                                        })
                                })
                        })
                }),
            "rollback evidence requires report schema version 5"
        );
    }
    for scenario in &report.scenarios {
        for stage in &scenario.stages {
            ensure!(
                report.schema_version >= 7
                    || stage
                        .checks
                        .iter()
                        .all(|check| !check.json_required && check.json_expectation.is_none()),
                "JSON evidence requires report schema version 7"
            );
            ensure!(
                report.schema_version >= 8
                    || stage
                        .checks
                        .iter()
                        .all(|c| !c.mcp_required && c.mcp.is_none()),
                "MCP evidence requires report schema version 8"
            );
            ensure!(
                stage.valid_mcp_checks(),
                "invalid MCP check evidence or command binding"
            );
            ensure!(
                stage.valid_json_checks(),
                "invalid JSON check evidence or command binding"
            );
            for command in &stage.commands {
                for log in command.stdout.iter().chain(&command.stderr) {
                    log_components(&log.path)?;
                }
            }
        }
    }
    Ok(())
}
pub fn load(path: &Path) -> Result<RunReport> {
    let root = report_root(path, "report.json")?;
    let store = open_store(&root)?;
    crate::native::storage::require_family(
        store.artifact_family()?,
        crate::native::storage::ArtifactFamily::Docker,
    )?;
    decode_report(&store.read_bytes("report.json")?)
}
pub(crate) fn decode_report(bytes: &[u8]) -> Result<RunReport> {
    crate::native::storage::ensure_legacy_namespace(bytes)?;
    let mut report: RunReport = serde_json::from_slice(bytes).context("decode report JSON")?;
    validate_report(&report)?;
    if report.schema_version >= 4 {
        report.recompute_outcome();
    }
    Ok(report)
}
pub fn load_journal(path: &Path) -> Result<RunJournal> {
    let root = report_root(path, "journal.json")?;
    let store = open_store(&root)?;
    crate::native::storage::require_family(
        store.artifact_family()?,
        crate::native::storage::ArtifactFamily::Docker,
    )?;
    let bytes = store.read_bytes("journal.json")?;
    crate::native::storage::ensure_legacy_namespace(&bytes)?;
    let journal: RunJournal = serde_json::from_slice(&bytes)?;
    validate_journal(&journal)?;
    Ok(journal)
}
fn validate_journal(journal: &RunJournal) -> Result<()> {
    ensure!(
        journal.schema_version == JOURNAL_SCHEMA_VERSION,
        "unsupported journal schema version {}",
        journal.schema_version
    );
    Ok(())
}
pub fn read_log(root: &Path, relative: &str, limit: usize) -> Result<LogRead> {
    open_store(root)?.read_log(relative, limit)
}
/// Freshness from a safely reopened regular file, including native replacement identity.
pub fn log_fingerprint(root: &Path, relative: &str) -> Result<LogFingerprint> {
    fingerprint(&open_store(root)?.open_log(relative)?).context("fingerprint contained command log")
}
pub fn read_log_page(
    root: &Path,
    reference: &LogReference,
    offset: u64,
    limit: usize,
) -> Result<LogPage> {
    open_store(root)?.read_log_page(reference, offset, limit)
}
fn report_root(path: &Path, filename: &str) -> Result<PathBuf> {
    if fs::symlink_metadata(path)?.is_dir() {
        return Ok(path.into());
    }
    if path.file_name().is_some_and(|n| n == filename) {
        return Ok(path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .into());
    }
    bail!("expected run directory or {filename}")
}

#[cfg(test)]
mod durability_tests {
    use super::*;

    use crate::{
        engine::CancellationToken,
        model::*,
        recipe::{Limits, ValidatedRecipe},
        runner::*,
        snapshot::Snapshot,
    };
    #[test]
    fn strict_root_creation_and_reopen_preserve_native_ancestry() {
        #[cfg(all(unix, not(target_os = "macos")))]
        let name = {
            use std::os::unix::ffi::OsStringExt;
            std::ffi::OsString::from_vec(b"native-\xff".to_vec())
        };
        #[cfg(target_os = "macos")]
        let name = std::ffi::OsString::from("native-λ");
        #[cfg(windows)]
        let name = {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(&[110, 97, 116, 105, 118, 101, 45, 0xd800])
        };
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join(name).join("run");
        let created = ReportStore::safe_root(&root, true).unwrap();
        created.save(&RunReport::new("native-ancestry")).unwrap();
        let reopened = ReportStore::safe_root(&root, false).unwrap();
        assert_eq!(
            reopened
                .read_json::<RunReport>("report.json")
                .unwrap()
                .run_id,
            "native-ancestry"
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn strict_root_macos_invalid_utf8_creation_is_rejected_with_eilseq() {
        use std::os::unix::ffi::OsStringExt;
        let temp = tempfile::tempdir().unwrap();
        let invalid = std::ffi::OsString::from_vec(b"native-\xff".to_vec());
        let root = temp
            .path()
            .canonicalize()
            .unwrap()
            .join(invalid)
            .join("run");
        let error = ReportStore::safe_root(&root, true).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(rustix::io::Errno::ILSEQ.raw_os_error()),
            "invalid UTF-8 creation must preserve the native EILSEQ failure"
        );
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn strict_root_macos_absent_invalid_utf8_ancestry_returns_enoent() {
        use std::os::unix::ffi::OsStringExt;
        let temp = tempfile::tempdir().unwrap();
        let absent = std::ffi::OsString::from_vec(b"native-\xff".to_vec());
        let root = temp.path().canonicalize().unwrap().join(absent).join("run");
        let error = ReportStore::safe_root(&root, false).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(rustix::io::Errno::NOENT.raw_os_error()),
            "lookup of absent ancestry reports ENOENT, not creation validation"
        );
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }
    struct PreparationObserver {
        called: bool,
    }
    impl Environment for PreparationObserver {
        fn prepare(
            &mut self,
            _: &ValidatedRecipe,
            _: bool,
            _: &CancellationToken,
        ) -> Result<PreparedEnvironment> {
            self.called = true;
            bail!("unexpected prepare")
        }
        fn create(&mut self, _: &str, _: &str, _: &ValidatedRecipe) -> Result<OwnedResource> {
            unreachable!()
        }
        fn setup(
            &mut self,
            _: &OwnedResource,
            _: &ValidatedRecipe,
            _: &CancellationToken,
        ) -> Result<()> {
            unreachable!()
        }
        fn execute(
            &mut self,
            _: &OwnedResource,
            _: CommandRequest,
            _: &ReportStore,
            _: &CancellationToken,
        ) -> Result<ExecutedCommand> {
            unreachable!()
        }
        fn observe(
            &mut self,
            _: &OwnedResource,
            _: &[String],
            _: Limits,
            _: &CancellationToken,
        ) -> Result<Vec<Snapshot>> {
            unreachable!()
        }
        fn stop(&mut self, _: &OwnedResource) -> Result<()> {
            unreachable!()
        }
        fn remove(&mut self, _: &OwnedResource) -> Result<()> {
            unreachable!()
        }
    }
    #[test]
    fn injected_run_directory_creation_failure_precedes_started_and_environment_preparation() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("target")).unwrap();
        let recipe_path = temp.path().join("recipe.toml");
        fs::write(
            &recipe_path,
            include_str!("../../tests/fixtures/recipes/v2_clean.toml"),
        )
        .unwrap();
        let recipe = crate::recipe::load(&recipe_path).unwrap();
        let mut parent = open_existing(temp.path()).unwrap();
        parent.fail_child_creation = true;
        let mut environment = PreparationObserver { called: false };
        let mut emitted = false;
        let result = crate::engine::run_with_parent(
            &recipe,
            crate::engine::RunOptions::all(&recipe),
            CancellationToken::default(),
            &mut environment,
            Some(&parent),
            |_| emitted = true,
        );
        assert!(format!("{:#}", result.err().unwrap()).contains("injected child creation failure"));
        assert!(
            !emitted,
            "failed run directory creation must precede Started"
        );
        assert!(!environment.called, "environment preparation must not run");
    }
}

#[cfg(test)]
mod bounded_tests {
    use super::*;
    use serde::ser::SerializeSeq;
    use std::cell::Cell;
    struct Many<'a>(&'a Cell<usize>);
    impl Serialize for Many<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut seq = serializer.serialize_seq(Some(10000))?;
            for _ in 0..10000 {
                self.0.set(self.0.get() + 1);
                seq.serialize_element("\u{0001}\u{0001}\u{0001}")?;
            }
            seq.end()
        }
    }
    #[test]
    fn serializer_failure_preserves_exact_report_and_recomputed_outcome_with_typed_state() {
        let temp = tempfile::tempdir().unwrap();
        let store = create(&temp.path().join("run")).unwrap();
        let mut report = RunReport::new("cancelled");
        report.cancelled = true;
        store.save(&report).unwrap();
        let previous = fs::read(store.report_path()).unwrap();
        let error = store
            .atomic_json_with_limit("report.json", &Many(&Cell::new(0)), 512)
            .unwrap_err();
        let io_error = error.downcast_ref::<std::io::Error>().unwrap();
        let detail = io_error
            .get_ref()
            .unwrap()
            .downcast_ref::<theseus_host::fs::PublicationError>()
            .unwrap();
        assert_eq!(
            detail.state(),
            theseus_host::fs::PublicationState::NotReplaced
        );
        assert_eq!(fs::read(store.report_path()).unwrap(), previous);
        assert_eq!(
            load(store.root()).unwrap().outcome,
            crate::model::RunOutcome::Cancelled
        );
        assert_eq!(fs::read_dir(store.root()).unwrap().count(), 1);
    }
    #[test]
    fn atomic_serialization_stops_early_preserves_previous_and_removes_pending() {
        let temp = tempfile::tempdir().unwrap();
        let store = create(&temp.path().join("run")).unwrap();
        use crate::comparison::{ComparisonArtifact, ComparisonReport, SuiteManifest};
        use sha2::{Digest, Sha256};
        let manifest: SuiteManifest =
            toml::from_str(include_str!("../../examples/comparisons/methods.toml")).unwrap();
        let artifact = ComparisonArtifact {
            schema_version: 1,
            suite_id: "previous".into(),
            manifest_digest: format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&manifest).unwrap())
            ),
            recipe_digests: manifest
                .variants
                .iter()
                .map(|v| (v.id.clone(), "a".repeat(64)))
                .collect(),
            manifest,
            sources: vec![],
            report: ComparisonReport {
                sources: vec![],
                cells: vec![],
                outcome: crate::model::RunOutcome::Incomplete,
                diagnostics: vec![],
            },
            cancelled: false,
            finished: false,
        };
        store.atomic_json("comparison.json", &artifact).unwrap();
        crate::comparison::load_report(store.root()).unwrap();
        let previous = fs::read(store.root().join("comparison.json")).unwrap();
        let visited = Cell::new(0);
        assert!(
            store
                .atomic_json_with_limit("comparison.json", &Many(&visited), 512)
                .is_err()
        );
        assert!(
            visited.get() < 100,
            "serialization visited {} entries after the limit",
            visited.get()
        );
        assert_eq!(
            fs::read(store.root().join("comparison.json")).unwrap(),
            previous
        );
        assert_eq!(fs::read_dir(store.root()).unwrap().count(), 1);
        crate::comparison::load_report(store.root()).unwrap();
        crate::comparison::cleanup(store.root()).unwrap();
        store
            .atomic_json("journal.json", &vec!["cleanup remains usable"])
            .unwrap();
    }
}
