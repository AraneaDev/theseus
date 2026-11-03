//! Strict native evidence, deliberately outside the legacy Docker namespaces.
use crate::model::{
    CheckOutcome, Checkpoint, LogReference, RunOutcome, ScenarioKind, StageOutcome, StepName,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeSet;

pub const REPORT_FILENAME: &str = "native-report.json";
pub const JOURNAL_FILENAME: &str = "native-journal.json";
pub const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogProgram {
    CheckBase,
    CheckData,
    CheckTarget,
    Configure,
    Exercise,
    InstallBase,
    InstallTarget,
    Uninstall,
}
pub const CATALOG_IDS: [CatalogProgram; 8] = [
    CatalogProgram::CheckBase,
    CatalogProgram::CheckData,
    CatalogProgram::CheckTarget,
    CatalogProgram::Configure,
    CatalogProgram::Exercise,
    CatalogProgram::InstallBase,
    CatalogProgram::InstallTarget,
    CatalogProgram::Uninstall,
];
impl CatalogProgram {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CheckBase => "check_base",
            Self::CheckData => "check_data",
            Self::CheckTarget => "check_target",
            Self::Configure => "configure",
            Self::Exercise => "exercise",
            Self::InstallBase => "install_base",
            Self::InstallTarget => "install_target",
            Self::Uninstall => "uninstall",
        }
    }
    pub fn program_id(self) -> theseus_native_protocol::v2::ProgramId {
        theseus_native_protocol::v2::ProgramId::new(self.as_str()).expect("closed catalog IDs")
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeStatus {
    PosixExit { code: u8 },
    PosixSignal { signal: u16 },
    WindowsExit { code: u32 },
}
impl NativeStatus {
    pub fn validate(&self) -> Result<()> {
        if let Self::PosixSignal { signal } = self {
            ensure!((1..=255).contains(signal), "invalid POSIX signal");
        }
        Ok(())
    }
}
impl From<theseus_native_protocol::v2::NativeStatus> for NativeStatus {
    fn from(value: theseus_native_protocol::v2::NativeStatus) -> Self {
        use theseus_native_protocol::v2::NativeStatus as S;
        match value {
            S::PosixExit { code } => Self::PosixExit { code },
            S::PosixSignal { signal } => Self::PosixSignal { signal },
            S::WindowsExit { code } => Self::WindowsExit { code },
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogCommand {
    pub id: String,
    pub program: CatalogProgram,
    pub expected_status: NativeStatus,
    pub stdout_literal: Option<String>,
}
impl CatalogCommand {
    pub fn validate(&self) -> Result<()> {
        text(&self.id, 128)?;
        self.expected_status.validate()?;
        if let Some(s) = &self.stdout_literal {
            text(s, 16384)?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrefixAuthorityFailure {
    Missing,
    Replaced,
    Renamed,
    MappingUnproved,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    content = "failure",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum NativeCatalogAuthority {
    Valid,
    Revoked(PrefixAuthorityFailure),
}
impl NativeCatalogAuthority {
    pub fn revoke(&mut self, reason: PrefixAuthorityFailure) {
        if matches!(self, Self::Valid) {
            *self = Self::Revoked(reason);
        }
    }
    pub fn authorize(&self, _program: CatalogProgram) -> Result<()> {
        ensure!(
            matches!(self, Self::Valid),
            "native prefix authority revoked: {self:?}"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeFileState {
    Present { length: u64, sha256: String },
    AbsentSettled,
    Unknown { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeFileObservation {
    pub request_id: String,
    pub root_identity: String,
    pub leaf: String,
    pub state: NativeFileState,
}
impl NativeFileObservation {
    pub fn validate(&self) -> Result<()> {
        text(&self.request_id, 128)?;
        text(&self.root_identity, 1024)?;
        ensure!(
            matches!(self.leaf.as_str(), "reader" | "reader.exe" | "app" | "data"),
            "undeclared native leaf"
        );
        match &self.state {
            NativeFileState::Present { length, sha256 } => {
                ensure!(*length <= 64 * 1024 * 1024, "native file exceeds limit");
                digest(sha256)?;
                if self.leaf == "app" || self.leaf == "data" {
                    ensure!(*length <= 4096, "marker/data exceeds limit");
                }
            }
            NativeFileState::Unknown { reason } => text(reason, 4096)?,
            NativeFileState::AbsentSettled => {}
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provenance", rename_all = "snake_case", deny_unknown_fields)]
pub enum Provenance<T> {
    Observed { value: T, source: String },
    ProviderReported { value: T, provider: String },
    Unavailable { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCommandEvidence {
    pub command: CatalogCommand,
    pub operation_id: String,
    pub request_digest: String,
    pub status: Option<NativeStatus>,
    pub stdout: String,
    pub stdout_complete: bool,
    pub stdout_utf8: bool,
    pub stderr_complete: bool,
    #[serde(deserialize_with = "strict_optional_log")]
    pub stdout_log: Option<LogReference>,
    #[serde(deserialize_with = "strict_optional_log")]
    pub stderr_log: Option<LogReference>,
    pub failure: Option<String>,
}
impl NativeCommandEvidence {
    pub fn validate(&self) -> Result<()> {
        self.command.validate()?;
        if let Some(status) = &self.status {
            status.validate()?;
        }
        hex(&self.operation_id, 32)?;
        digest(&self.request_digest)?;
        ensure!(
            self.stdout.len() <= 16384,
            "native stdout exceeds capture limit"
        );
        ensure!(
            self.status.is_some() || self.failure.is_some(),
            "command lacks terminal status or failure"
        );
        if let Some(s) = &self.failure {
            text(s, 4096)?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCheckEvidence {
    pub id: String,
    pub checkpoint: Checkpoint,
    pub outcome: CheckOutcome,
    pub detail: String,
    pub command_ids: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeStage {
    pub name: String,
    pub step: Option<StepName>,
    pub checkpoint: Option<Checkpoint>,
    pub outcome: StageOutcome,
    pub commands: Vec<NativeCommandEvidence>,
    pub checks: Vec<NativeCheckEvidence>,
    pub native_files: Vec<NativeFileObservation>,
    pub failures: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Disposition {
    NotStarted,
    Complete { receipt: String },
    Unknown { reason: String },
    Failed { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeDispositions {
    pub local: Disposition,
    pub remote: Disposition,
    pub release: Disposition,
    pub reset: Disposition,
}
impl Default for NativeDispositions {
    fn default() -> Self {
        Self {
            local: Disposition::NotStarted,
            remote: Disposition::NotStarted,
            release: Disposition::NotStarted,
            reset: Disposition::NotStarted,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderReceipt {
    pub provider: String,
    pub invocation_id: String,
    pub invocation_digest: String,
    pub binding: String,
    pub manifest_digest: String,
    pub workspace_marker: String,
    pub reset_epoch: String,
    pub receipt: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeIntent {
    pub intent: String,
    pub binding: String,
    pub manifest_digest: String,
    pub workspace_marker: String,
    pub reset_epoch: String,
    pub publication_scope: String,
}
impl NativeIntent {
    pub fn validate(&self) -> Result<()> {
        hex(&self.intent, 32)?;
        hex(&self.binding, 32)?;
        digest(&self.manifest_digest)?;
        hex(&self.workspace_marker, 32)?;
        text(&self.reset_epoch, 256)?;
        ensure!(
            matches!(
                self.publication_scope.as_str(),
                "file_and_directory_synced" | "file_flushed_atomic_replace"
            ),
            "invalid intent publication scope"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeOwnership {
    Preparing {
        provider: String,
        invocation_id: String,
        receipt: Option<ProviderReceipt>,
    },
    Intent {
        prepared: NativeIntent,
    },
    Acquired {
        prepared: NativeIntent,
        lease: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeResource {
    pub scenario_id: String,
    pub ownership_id: String,
    pub ownership: NativeOwnership,
    pub catalog_authority: NativeCatalogAuthority,
}
impl NativeResource {
    pub fn validate(&self) -> Result<()> {
        text(&self.scenario_id, 128)?;
        text(&self.ownership_id, 128)?;
        match &self.ownership {
            NativeOwnership::Preparing {
                provider,
                invocation_id,
                receipt,
            } => {
                text(provider, 256)?;
                text(invocation_id, 128)?;
                if let Some(r) = receipt {
                    ensure!(
                        &r.provider == provider && &r.invocation_id == invocation_id,
                        "provider receipt identity mismatch"
                    );
                    r.validate()?;
                    hex(&r.binding, 32)?;
                    digest(&r.manifest_digest)?;
                    hex(&r.workspace_marker, 32)?;
                    text(&r.reset_epoch, 256)?;
                    text(&r.receipt, 4096)?;
                }
            }
            NativeOwnership::Intent { prepared } => prepared.validate()?,
            NativeOwnership::Acquired { prepared, lease } => {
                prepared.validate()?;
                hex(lease, 32)?;
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeScenario {
    pub id: String,
    pub scenario: ScenarioKind,
    pub ownership_id: String,
    pub stages: Vec<NativeStage>,
    pub outcome: RunOutcome,
    pub dispositions: NativeDispositions,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeReport {
    pub kind: String,
    pub schema: u32,
    pub run_id: String,
    pub recipe_digest: String,
    pub scenarios: Vec<NativeScenario>,
    pub identities: Vec<Provenance<String>>,
    pub provider_claims: Vec<ProviderReceipt>,
    pub failures: Vec<String>,
    pub outcome: RunOutcome,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeJournal {
    pub kind: String,
    pub schema: u32,
    pub run_id: String,
    pub launch_fingerprint: Option<String>,
    pub ownership: Vec<NativeResource>,
    pub operations: Vec<NativeOperation>,
    pub provider_calls: Vec<NativeProviderCall>,
    pub releases: Vec<NativeRelease>,
    pub failures: Vec<String>,
    #[serde(default)]
    pub recovery_attempts: Vec<NativeRecoveryAttempt>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRecoveryAttempt {
    pub recovery_id: String,
    pub ownership_id: String,
    pub prior_local: Disposition,
    pub local: Disposition,
    pub remote: Disposition,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeOperation {
    pub ownership_id: String,
    pub lease: String,
    pub operation_id: String,
    pub request_digest: String,
    pub command: CatalogCommand,
    pub completion: Disposition,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRelease {
    pub ownership_id: String,
    pub lease: String,
    pub completion: Disposition,
}

impl NativeOperation {
    pub fn request(&self) -> Result<theseus_native_protocol::v2::Request> {
        use theseus_native_protocol::v2::{LeaseId, OperationId, Request};
        Ok(Request::Run {
            lease: LeaseId::parse(&self.lease)?,
            operation: OperationId::parse(&self.operation_id)?,
            program: self.command.program.program_id(),
        })
    }
    pub fn validate(&self) -> Result<()> {
        text(&self.ownership_id, 128)?;
        self.command.validate()?;
        digest(&self.request_digest)?;
        ensure!(
            theseus_native_protocol::v2::request_digest(&self.request()?)?.to_hex()
                == self.request_digest,
            "native request digest differs from exact catalog request"
        );
        self.completion.validate()
    }
}
impl Disposition {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::NotStarted => Ok(()),
            Self::Complete { receipt } => text(receipt, 4096),
            Self::Unknown { reason } | Self::Failed { reason } => text(reason, 4096),
        }
    }
}
impl ProviderReceipt {
    pub fn validate(&self) -> Result<()> {
        text(&self.provider, 256)?;
        text(&self.invocation_id, 128)?;
        digest(&self.invocation_digest)?;
        hex(&self.binding, 32)?;
        digest(&self.manifest_digest)?;
        hex(&self.workspace_marker, 32)?;
        text(&self.reset_epoch, 256)?;
        text(&self.receipt, 4096)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAction {
    Prepare,
    Reset,
    Dispose,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderInvocation {
    pub ownership_id: String,
    pub provider: String,
    pub invocation_id: String,
    pub action: ProviderAction,
    pub target_id: String,
    pub baseline_label: String,
    pub launch_fingerprint: String,
    pub prior_resource: NativeResource,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProviderCall {
    pub invocation: ProviderInvocation,
    pub completion: Disposition,
    pub receipt: Option<ProviderReceipt>,
}

impl ProviderInvocation {
    pub fn validate(&self) -> Result<()> {
        text(&self.ownership_id, 128)?;
        text(&self.provider, 256)?;
        text(&self.invocation_id, 128)?;
        text(&self.target_id, 128)?;
        text(&self.baseline_label, 256)?;
        digest(&self.launch_fingerprint)?;
        self.prior_resource.validate()?;
        ensure!(
            self.ownership_id == self.prior_resource.ownership_id,
            "provider prior owner mismatch"
        );
        Ok(())
    }
    pub fn digest(&self) -> Result<String> {
        use sha2::{Digest as _, Sha256};
        self.validate()?;
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }
}

impl NativeJournal {
    pub fn new(run_id: impl Into<String>) -> Self {
        Self {
            kind: "theseus_native_journal".into(),
            schema: 1,
            run_id: run_id.into(),
            launch_fingerprint: None,
            ownership: vec![],
            operations: vec![],
            provider_calls: vec![],
            releases: vec![],
            failures: vec![],
            recovery_attempts: vec![],
        }
    }
    pub fn validate(&self) -> Result<()> {
        envelope(&self.kind, self.schema, "theseus_native_journal")?;
        text(&self.run_id, 128)?;
        match &self.launch_fingerprint {
            Some(value) => digest(value)?,
            None => ensure!(
                self.ownership.is_empty()
                    && self.operations.is_empty()
                    && self.provider_calls.is_empty()
                    && self.releases.is_empty()
                    && self.recovery_attempts.is_empty(),
                "unbound journal contains effects"
            ),
        }
        let mut keys = BTreeSet::new();
        for r in &self.ownership {
            r.validate()?;
            ensure!(keys.insert(&r.ownership_id), "duplicate native ownership");
        }
        let mut operations = BTreeSet::new();
        for op in &self.operations {
            ensure!(
                keys.contains(&op.ownership_id),
                "operation has no original owner"
            );
            hex(&op.lease, 32)?;
            hex(&op.operation_id, 32)?;
            digest(&op.request_digest)?;
            op.validate()?;
            let owner = self
                .ownership
                .iter()
                .find(|r| r.ownership_id == op.ownership_id)
                .expect("owner existence checked");
            ensure!(
                matches!(&owner.ownership,NativeOwnership::Acquired{lease,..} if lease==&op.lease),
                "operation lease does not match original owner"
            );
            ensure!(
                operations.insert((&op.lease, &op.operation_id)),
                "duplicate native operation"
            );
        }
        let mut release_ids = BTreeSet::new();
        for release in &self.releases {
            hex(&release.lease, 32)?;
            release.completion.validate()?;
            let owner = self
                .ownership
                .iter()
                .find(|r| r.ownership_id == release.ownership_id)
                .ok_or_else(|| anyhow::anyhow!("release lacks original owner"))?;
            ensure!(
                matches!(&owner.ownership,NativeOwnership::Acquired{lease,..} if lease==&release.lease),
                "release lease does not match original owner"
            );
            ensure!(
                release_ids.insert((&release.ownership_id, &release.lease)),
                "duplicate native release"
            );
        }
        let mut provider_ids = BTreeSet::new();
        for call in &self.provider_calls {
            let invocation = &call.invocation;
            invocation.validate()?;
            ensure!(
                Some(&invocation.launch_fingerprint) == self.launch_fingerprint.as_ref(),
                "provider launch fingerprint mismatch"
            );
            text(&invocation.provider, 256)?;
            text(&invocation.invocation_id, 128)?;
            ensure!(
                keys.contains(&invocation.ownership_id),
                "provider call lacks original owner"
            );
            ensure!(
                provider_ids.insert((&invocation.provider, &invocation.invocation_id)),
                "duplicate provider invocation"
            );
            call.completion.validate()?;
            if let Some(receipt) = &call.receipt {
                receipt.validate()?;
                ensure!(
                    matches!(&call.completion, Disposition::Complete { receipt: saved } if saved == &receipt.receipt),
                    "provider receipt lacks matching terminal disposition"
                );
                ensure!(
                    receipt.provider == invocation.provider
                        && receipt.invocation_id == invocation.invocation_id
                        && receipt.invocation_digest == invocation.digest()?,
                    "provider receipt mismatch"
                );
            } else {
                ensure!(
                    !matches!(call.completion, Disposition::Complete { .. }),
                    "complete provider call lacks original receipt"
                );
            }
        }
        ensure!(
            self.recovery_attempts.len() <= 128,
            "native recovery attempt limit exceeded"
        );
        let mut recoveries = BTreeSet::new();
        for attempt in &self.recovery_attempts {
            hex(&attempt.recovery_id, 32)?;
            ensure!(
                recoveries.insert(&attempt.recovery_id),
                "duplicate recovery identity"
            );
            ensure!(
                keys.contains(&attempt.ownership_id),
                "recovery lacks original ownership"
            );
            attempt.prior_local.validate()?;
            attempt.local.validate()?;
            attempt.remote.validate()?;
        }
        for f in &self.failures {
            text(f, 4096)?;
        }
        Ok(())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value: Self = decode(bytes)?;
        value.validate()?;
        Ok(value)
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(serde_json::to_vec_pretty(self)?)
    }
}
impl NativeReport {
    pub fn validate(&self) -> Result<()> {
        envelope(&self.kind, self.schema, "theseus_native_report")?;
        text(&self.run_id, 128)?;
        digest(&self.recipe_digest)?;
        let mut ids = BTreeSet::new();
        for s in &self.scenarios {
            text(&s.id, 128)?;
            text(&s.ownership_id, 128)?;
            ordinary(s.scenario)?;
            s.dispositions.local.validate()?;
            s.dispositions.remote.validate()?;
            s.dispositions.release.validate()?;
            s.dispositions.reset.validate()?;
            ensure!(ids.insert(&s.id), "duplicate native scenario");
            for stage in &s.stages {
                text(&stage.name, 128)?;
                for c in &stage.commands {
                    c.validate()?;
                }
                for o in &stage.native_files {
                    o.validate()?;
                }
                for c in &stage.checks {
                    text(&c.id, 128)?;
                    text(&c.detail, 4096)?;
                }
            }
        }
        for receipt in &self.provider_claims {
            receipt.validate()?;
        }
        for failure in &self.failures {
            text(failure, 4096)?;
        }
        Ok(())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value: Self = decode(bytes)?;
        value.validate()?;
        Ok(value)
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(serde_json::to_vec_pretty(self)?)
    }
}
pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    ensure!(
        bytes.len() <= MAX_ARTIFACT_BYTES,
        "native artifact exceeds limit"
    );
    let value = crate::strict_json::parse(std::str::from_utf8(bytes)?)?;
    Ok(serde_json::from_value(value)?)
}
pub(crate) fn envelope(kind: &str, schema: u32, expected: &str) -> Result<()> {
    ensure!(
        kind == expected && schema == 1,
        "unsupported native namespace/schema"
    );
    Ok(())
}
pub(crate) fn text(value: &str, max: usize) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= max && !value.contains('\0'),
        "invalid bounded native text"
    );
    Ok(())
}
pub(crate) fn hex(value: &str, len: usize) -> Result<()> {
    ensure!(
        value.len() == len
            && value
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v)),
        "invalid native hex identity"
    );
    Ok(())
}
pub(crate) fn digest(value: &str) -> Result<()> {
    hex(value, 64)
}
pub(crate) fn ordinary(kind: ScenarioKind) -> Result<()> {
    ensure!(
        matches!(
            kind,
            ScenarioKind::Clean | ScenarioKind::Upgrade | ScenarioKind::Reinstall
        ),
        "unsupported native scenario"
    );
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictLogReference {
    path: String,
    bytes: u64,
    truncated: bool,
}
fn strict_optional_log<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<LogReference>, D::Error> {
    Ok(
        Option::<StrictLogReference>::deserialize(deserializer)?.map(|r| LogReference {
            path: r.path,
            bytes: r.bytes,
            truncated: r.truncated,
        }),
    )
}
