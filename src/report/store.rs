use crate::{
    model::{
        Checkpoint, JOURNAL_SCHEMA_VERSION, LogReference, RunJournal, RunReport, ScenarioKind,
        StepName,
    },
    recipe::DEFAULT_LOG_LIMIT,
};
use anyhow::{Context, Result, bail, ensure};
use rustix::fs::{AtFlags, Mode, OFlags, mkdirat, open, openat, renameat, unlinkat};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, DirBuilder, File},
    io::{Read, Write},
    os::unix::fs::DirBuilderExt,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const MAX_REPORT_BYTES: u64 = 64 * 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
#[derive(Debug)]
pub struct ReportStore {
    root: PathBuf,
    directory: File,
}
#[derive(Debug, Clone)]
pub struct LogRead {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

/// Create exactly this run directory, never reusing an existing directory.
pub fn create(root: &Path) -> Result<ReportStore> {
    if let Some(parent) = root.parent().filter(|p| !p.as_os_str().is_empty()) {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .context("create report parent")?;
    }
    DirBuilder::new()
        .mode(0o700)
        .create(root)
        .context("create fresh run directory (existing runs are never overwritten)")?;
    open_store(root)
}
/// Reopen a run directory for atomic recovery-journal updates; refuses symlinks.
pub fn open_existing(root: &Path) -> Result<ReportStore> {
    open_store(root)
}
fn open_store(root: &Path) -> Result<ReportStore> {
    // Reject a symlink at the user-facing directory before canonicalizing its parent.
    let directory = File::from(
        open(
            root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .context("open run directory")?,
    );
    Ok(ReportStore {
        root: root.canonicalize()?,
        directory,
    })
}
impl ReportStore {
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn report_path(&self) -> PathBuf {
        self.root.join("report.json")
    }
    pub fn save(&self, report: &RunReport) -> Result<()> {
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
        validate_journal(journal)?;
        self.atomic_json("journal.json", journal)
    }
    fn atomic_json(&self, name: &str, value: &impl Serialize) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(value)?;
        ensure!(
            bytes.len() as u64 <= MAX_REPORT_BYTES,
            "report/journal exceeds size limit"
        );
        let mut temporary = None;
        for _ in 0..32 {
            let name = format!(
                ".pending-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            );
            match openat(
                &self.directory,
                name.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            ) {
                Ok(fd) => {
                    temporary = Some((name, File::from(fd)));
                    break;
                }
                Err(rustix::io::Errno::EXIST) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        let (temporary, mut file) =
            temporary.context("could not reserve atomic report temporary file")?;
        let result = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            renameat(&self.directory, temporary.as_str(), &self.directory, name)?;
            self.directory.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = unlinkat(&self.directory, temporary.as_str(), AtFlags::empty());
        }
        result
    }
    /// Create one stream file exclusively. Runtime adapters can stream bounded
    /// output to it while preserving 0600 permissions and safe parent traversal.
    pub fn create_log(&self, relative: &str) -> Result<File> {
        let parts = log_components(relative)?;
        let parent = self.log_parent(&parts[..parts.len() - 1], true)?;
        Ok(File::from(
            openat(
                &parent,
                parts[parts.len() - 1],
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .context("create private command log")?,
        ))
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
        let parts = log_components(relative)?;
        let parent = self.log_parent(&parts[..parts.len() - 1], false)?;
        let file = File::from(
            openat(
                &parent,
                parts[parts.len() - 1],
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .context("open contained command log")?,
        );
        ensure!(
            file.metadata()?.is_file(),
            "log reference is not a regular file"
        );
        let limit = limit.min(DEFAULT_LOG_LIMIT as usize);
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        let truncated = bytes.len() > limit;
        bytes.truncate(limit);
        Ok(LogRead { bytes, truncated })
    }
    fn log_parent(&self, parts: &[&str], create: bool) -> Result<File> {
        let mut directory = self.directory.try_clone()?;
        for part in parts {
            if create {
                match mkdirat(&directory, *part, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            directory = File::from(
                openat(
                    &directory,
                    *part,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .context("open safe log parent")?,
            );
        }
        Ok(directory)
    }
    fn read_json<T: DeserializeOwned>(&self, name: &str) -> Result<T> {
        let file = File::from(
            openat(
                &self.directory,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .context("open report/journal")?,
        );
        ensure!(
            file.metadata()?.is_file(),
            "report/journal must be a regular file"
        );
        let mut bytes = Vec::new();
        file.take(MAX_REPORT_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_REPORT_BYTES,
            "report/journal exceeds size limit"
        );
        serde_json::from_slice(&bytes).context("decode report/journal JSON")
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
        !matches!(parts[0], "report.json" | "journal.json"),
        "log reference conflicts with report metadata"
    );
    // Component checks are deliberately lexical, before any filesystem access.
    ensure!(
        Path::new(relative)
            .components()
            .all(|p| matches!(p, Component::Normal(_))),
        "unsafe log reference"
    );
    Ok(parts)
}
fn validate_report(report: &RunReport) -> Result<()> {
    ensure!(
        matches!(report.schema_version, 1..=5),
        "unsupported report schema version {}",
        report.schema_version
    );
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
    if report.schema_version < 5 {
        ensure!(
            report.scenarios.iter().all(|scenario| {
                scenario.rollback.is_none()
                    && scenario.stages.iter().all(|stage| {
                        stage.commands.iter().all(|command| {
                            command.expected_failure.is_none()
                                && command.outcome != crate::model::CommandOutcome::ExpectedFailure
                        }) && stage.checks.iter().all(|check| {
                            check.backup.is_none()
                                && check.comparison.as_ref().is_none_or(|reference| {
                                    !crate::model::rollback_reference(&reference.before_stage)
                                        && !crate::model::rollback_reference(&reference.after_stage)
                                })
                        })
                    })
            }),
            "rollback evidence requires report schema version 5"
        );
    }
    for scenario in &report.scenarios {
        for stage in &scenario.stages {
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
    let mut report: RunReport = open_store(&root)?.read_json("report.json")?;
    validate_report(&report)?;
    if report.schema_version >= 4 {
        report.recompute_outcome();
    }
    Ok(report)
}
pub fn load_journal(path: &Path) -> Result<RunJournal> {
    let root = report_root(path, "journal.json")?;
    let journal: RunJournal = open_store(&root)?.read_json("journal.json")?;
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
