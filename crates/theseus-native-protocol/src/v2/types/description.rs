//! Native description constants and provenance types.
use super::{BindingId, Digest, NativeCapabilities, Os, Principal, ProgramList, WorkspaceMarker};
pub const MAX_NATIVE_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const NATIVE_INSPECT_BUFFER_BYTES: usize = 64 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelperProfile {
    Fixture,
    Native {
        provider_epoch: Digest,
    },
    Integration {
        provider_epoch: Digest,
        core_manifest_digest: Digest,
        integration_manifest_digest: Digest,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Provenance<T> {
    Observed { value: T, source: String },
    ProviderReported { value: T, provider: String },
    Unavailable { reason: String },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeDescription {
    pub binding: BindingId,
    pub manifest_digest: Digest,
    pub workspace_marker: WorkspaceMarker,
    pub os: Os,
    pub helper_principal: Principal,
    pub programs: ProgramList,
    pub capabilities: NativeCapabilities,
    pub provider_epoch: Digest,
    pub artifact_commitment: Digest,
    pub build: Provenance<String>,
    pub os_build: Provenance<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefixAuthorityFailure {
    Missing,
    Replaced,
    Renamed,
    MappingUnproved,
}
