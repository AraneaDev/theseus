use crate::InvocationError;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};
use theseus_host::process::{ChildEnvironment, NativeCommand};
use theseus_native_helper_store::StoreBinding;
use theseus_native_protocol::v2;
pub struct CatalogEntry {
    pub id: v2::ProgramId,
    pub command: NativeCommand,
}
/// Invocation authority is fixed before peer input. No cwd confinement is implied.
pub struct Invocation {
    pub(crate) integration: Option<Box<crate::integration::service::ServiceConfig>>,
    #[cfg(feature = "test-support")]
    pub(crate) fixture: Option<crate::test_support::FixtureControl>,
    pub(crate) ledger_root: PathBuf,
    pub(crate) workspace_root: PathBuf,
    pub(crate) expected: StoreBinding,
    pub(crate) profile: v2::HelperProfile,
    pub(crate) artifact_commitment: Option<v2::Digest>,
    pub(crate) catalog: Vec<CatalogEntry>,
}
pub(crate) fn host_os() -> v2::Os {
    #[cfg(target_os = "linux")]
    {
        v2::Os::Linux
    }
    #[cfg(target_os = "macos")]
    {
        v2::Os::Macos
    }
    #[cfg(windows)]
    {
        v2::Os::Windows
    }
}
impl Invocation {
    pub fn new_native(
        ledger_root: PathBuf,
        workspace_root: PathBuf,
        expected: StoreBinding,
        catalog: Vec<CatalogEntry>,
        provider_epoch: v2::Digest,
        artifact_commitment: v2::Digest,
    ) -> Result<Self, InvocationError> {
        let mut inv = Self::new(ledger_root, workspace_root, expected, catalog)?;
        const IDS: [&str; 8] = [
            "check_base",
            "check_data",
            "check_target",
            "configure",
            "exercise",
            "install_base",
            "install_target",
            "uninstall",
        ];
        if !inv.catalog.iter().map(|e| e.id.as_str()).eq(IDS) {
            return Err(InvocationError::InvalidCatalog);
        }
        inv.profile = v2::HelperProfile::Native { provider_epoch };
        inv.artifact_commitment = Some(artifact_commitment);
        Ok(inv)
    }
    pub fn new(
        ledger_root: PathBuf,
        workspace_root: PathBuf,
        expected: StoreBinding,
        catalog: Vec<CatalogEntry>,
    ) -> Result<Self, InvocationError> {
        if !ledger_root.is_absolute() || !workspace_root.is_absolute() {
            return Err(InvocationError::InvalidRoots);
        }
        let ld = theseus_host::fs::HostDir::open(
            &ledger_root,
            theseus_host::fs::RootPolicy::LeafNoFollow,
        )
        .map_err(InvocationError::Io)?;
        let wd = theseus_host::fs::HostDir::open(
            &workspace_root,
            theseus_host::fs::RootPolicy::LeafNoFollow,
        )
        .map_err(InvocationError::Io)?;
        if ld.same_object(&wd).map_err(InvocationError::Io)? {
            return Err(InvocationError::InvalidRoots);
        }
        if expected.os != host_os() {
            return Err(InvocationError::WrongOs);
        }
        if catalog.is_empty() || catalog.len() > 8 || catalog.windows(2).any(|p| p[0].id >= p[1].id)
        {
            return Err(InvocationError::InvalidCatalog);
        }
        for entry in &catalog {
            validate_command(&entry.command)?;
        }
        Ok(Self {
            #[cfg(feature = "test-support")]
            fixture: None,
            integration: None,
            ledger_root,
            workspace_root,
            expected,
            profile: v2::HelperProfile::Fixture,
            artifact_commitment: None,
            catalog,
        })
    }
}
fn valid(s: &OsStr, max: usize) -> bool {
    s.as_encoded_bytes().len() <= max && !s.as_encoded_bytes().contains(&0)
}
fn validate_command(c: &NativeCommand) -> Result<(), InvocationError> {
    let bad = InvocationError::InvalidCatalog;
    if c.program.is_empty()
        || !Path::new(&c.program).is_absolute()
        || !valid(&c.program, 16384)
        || c.cwd.is_some()
        || c.args.len() > 16
        || c.args.iter().any(|a| !valid(a, 512))
        || Path::new(&c.program)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
    {
        return Err(bad);
    }
    let ChildEnvironment::Replace(env) = &c.environment else {
        return Err(bad);
    };
    if env.len() > 32
        || env.iter().any(|(k, v)| {
            k.is_empty()
                || !valid(k, 128)
                || k.as_encoded_bytes().contains(&b'=')
                || !valid(v, 1024)
        })
    {
        return Err(bad);
    }
    let total = c.program.as_encoded_bytes().len()
        + c.args
            .iter()
            .map(|a| a.as_encoded_bytes().len())
            .sum::<usize>()
        + env
            .iter()
            .map(|(k, v)| k.as_encoded_bytes().len() + v.as_encoded_bytes().len())
            .sum::<usize>();
    if total > 16384 {
        return Err(bad);
    }
    Ok(())
}
