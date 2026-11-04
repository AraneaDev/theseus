//! Version-aware explicit recovery entry. Saved endpoints never authorize effects.
use super::{CancellationToken, EngineEvent, NativeRecoveryResult, recover_native};
use crate::{
    native::{integration::model::*, launch::LaunchAuthority, model::Disposition},
    report::ReportStore,
};
use anyhow::{Result, bail, ensure};
use std::path::{Path, PathBuf};

pub enum SelectedNativeRecoveryResult {
    Catalog(Box<NativeRecoveryResult>),
    Integration {
        report_path: PathBuf,
        complete: bool,
    },
}
impl SelectedNativeRecoveryResult {
    pub fn report_path(&self) -> &Path {
        match self {
            Self::Catalog(r) => &r.report_path,
            Self::Integration { report_path, .. } => report_path,
        }
    }
    pub fn complete(&self) -> bool {
        match self {
            Self::Catalog(r) => r.complete(),
            Self::Integration { complete, .. } => *complete,
        }
    }
}
pub fn recover_selected_native(
    store: ReportStore,
    launch: &LaunchAuthority,
    cancel: CancellationToken,
    emit: impl FnMut(EngineEvent),
) -> Result<SelectedNativeRecoveryResult> {
    let (report, journal) = crate::native::storage::load_native_versioned(&store)?;
    match (report, journal) {
        (NativeReportVersion::CatalogV1(_), NativeJournalVersion::CatalogV1(_)) => {
            Ok(SelectedNativeRecoveryResult::Catalog(Box::new(
                recover_native(store, launch, cancel, emit)?,
            )))
        }
        (
            NativeReportVersion::IntegrationV2(report),
            NativeJournalVersion::IntegrationV2(journal),
        ) => {
            validate_pair(&report, &journal)?;
            ensure!(
                journal.launch_fingerprint.as_deref() == Some(launch.view().fingerprint.as_str()),
                "recovery requires unchanged explicit launch fingerprint"
            );
            let (core, integration) = launch.integration_components().ok_or_else(|| {
                anyhow::anyhow!("integration recovery requires explicit integration authority")
            })?;
            ensure!(
                core.to_hex() == journal.core_manifest_digest
                    && integration.to_hex() == journal.integration_manifest_digest,
                "recovery integration manifest changed"
            );
            let complete = report
                .integrations
                .iter()
                .all(|i| i.dispositions.complete())
                && report.catalog.scenarios.iter().all(|s| {
                    [
                        &s.dispositions.local,
                        &s.dispositions.remote,
                        &s.dispositions.release,
                        &s.dispositions.reset,
                    ]
                    .iter()
                    .all(|d| matches!(d, Disposition::Complete { .. }))
                })
                && journal
                    .integration_actions
                    .iter()
                    .all(|a| matches!(a.completion, Disposition::Complete { .. }));
            if !complete {
                bail!(
                    "integration recovery retains unresolved original owners; subordinate query recovery is not implemented yet"
                );
            }
            Ok(SelectedNativeRecoveryResult::Integration {
                report_path: store.report_path(),
                complete,
            })
        }
        _ => bail!("native report/journal version mismatch"),
    }
}
