use super::model::*;
use crate::fs::{FileFingerprint, HostDir, LeafOpenResult, RootPolicy, fingerprint};
use std::{
    ffi::OsStr,
    fs::File,
    io::{self, Read},
};
/// Original, non-serializable registration. A reported PID can never construct it.
pub struct RegistrationOwner {
    pub(super) file: File,
    pub(super) fingerprint: FileFingerprint,
    pub(super) definition_bytes: Vec<u8>,
    pub(super) definition: LaunchdDefinition,
    pub(super) manager_instance: [u8; 32],
    pub(super) install_receipt: String,
    pub(super) revoked: bool,
}
impl RegistrationOwner {
    pub fn install_receipt(&self) -> &str {
        &self.install_receipt
    }
    pub fn is_revoked(&self) -> bool {
        self.revoked
    }
    pub fn definition_fingerprint(&self) -> Vec<u8> {
        self.fingerprint.canonical_bytes()
    }
    pub fn definition_bytes(&self) -> &[u8] {
        &self.definition_bytes
    }
}
pub(super) fn read_definition(
    root: &HostDir,
    label: &str,
) -> io::Result<Option<(File, FileFingerprint, Vec<u8>)>> {
    let name = format!("{label}.plist");
    let mut file = match root.open_leaf_classified(OsStr::new(&name)) {
        LeafOpenResult::Opened(file) => file,
        LeafOpenResult::MissingSettled => return Ok(None),
        LeafOpenResult::FailedSettled(e) => return Err(e),
        LeafOpenResult::Unsettled(owner) => return Err(io::Error::other(owner)),
    };
    let before = fingerprint(&file)?;
    if before.len > OUTPUT_LIMIT as u64 {
        return Err(invalid("plist exceeds bound"));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(OUTPUT_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > OUTPUT_LIMIT || fingerprint(&file)? != before {
        return Err(invalid("plist changed during read"));
    }
    Ok(Some((file, before, bytes)))
}
pub(super) fn verify_original(
    authority: &LaunchdAuthority,
    root: &HostDir,
    owner: &mut RegistrationOwner,
    instance: [u8; 32],
) -> io::Result<()> {
    verify_original_observation(authority, root, owner, instance, false)
}
pub(super) fn verify_original_observation(
    authority: &LaunchdAuthority,
    root: &HostDir,
    owner: &mut RegistrationOwner,
    instance: [u8; 32],
    allow_sut_removal: bool,
) -> io::Result<()> {
    let result = (|| {
        if owner.revoked || owner.manager_instance != instance {
            return Err(invalid("original launchd registration permanently revoked"));
        }
        let current_root = HostDir::open(
            &authority.definition_root,
            RootPolicy::AllComponentsNoFollow,
        )?;
        if !root.same_object(&current_root)? {
            return Err(invalid("original plist root replaced"));
        }
        let Some((_, current, bytes)) = read_definition(root, &authority.label)? else {
            return if allow_sut_removal {
                Ok(())
            } else {
                Err(invalid("original plist missing"))
            };
        };
        if current != owner.fingerprint
            || fingerprint(&owner.file)? != owner.fingerprint
            || bytes != owner.definition_bytes
        {
            return Err(invalid("original plist replaced or modified"));
        }
        Ok(())
    })();
    if result.is_err() {
        owner.revoked = true;
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    fn original() -> (
        tempfile::TempDir,
        HostDir,
        LaunchdAuthority,
        RegistrationOwner,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("org.theseus.fixture.plist");
        std::fs::write(&path, b"original").unwrap();
        let root = HostDir::open(dir.path(), RootPolicy::AllComponentsNoFollow).unwrap();
        let (file, fp, bytes) = read_definition(&root, "org.theseus.fixture")
            .unwrap()
            .unwrap();
        let mut a = super::super::model::tests::authority();
        a.definition_root = dir.path().into();
        a.exclusive_load_roots = vec![dir.path().into()];
        let owner = RegistrationOwner {
            file,
            fingerprint: fp,
            definition_bytes: bytes,
            definition: LaunchdDefinition {
                executable: "/fixture/service".into(),
                arguments: vec!["/fixture/service".into()],
                working_directory: "/fixture".into(),
            },
            manager_instance: [7; 32],
            install_receipt: "original accepted installer".into(),
            revoked: false,
        };
        (dir, root, a, owner)
    }
    #[test]
    fn launchd_replaced_plist_sticky_revocation_survives_restoration() {
        let (dir, root, a, mut owner) = original();
        let path = a.plist_path();
        assert!(verify_original_observation(&a, &root, &mut owner, [7; 32], false).is_ok());
        // An accepted uninstall permits absence, but cannot hide a replacement.
        let saved = dir.path().join("saved");
        std::fs::rename(&path, &saved).unwrap();
        std::fs::write(&path, b"original").unwrap();
        assert!(verify_original_observation(&a, &root, &mut owner, [7; 32], false).is_err());
        assert!(owner.is_revoked());
        assert!(verify_original_observation(&a, &root, &mut owner, [7; 32], true).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&saved, &path).unwrap();
        assert!(verify_original_observation(&a, &root, &mut owner, [7; 32], false).is_err());
    }
    #[test]
    fn launchd_only_explicit_sut_uninstall_allows_absence_without_clearing_revocation() {
        let (_dir, root, a, mut owner) = original();
        std::fs::remove_file(a.plist_path()).unwrap();
        assert!(verify_original_observation(&a, &root, &mut owner, [7; 32], true).is_ok());
        assert!(!owner.is_revoked());
        assert!(verify_original_observation(&a, &root, &mut owner, [7; 32], false).is_err());
        assert!(owner.is_revoked());
        assert!(verify_original_observation(&a, &root, &mut owner, [7; 32], true).is_err());
    }
    #[test]
    fn launchd_explicit_uninstall_never_allows_byte_identical_replacement() {
        let (dir, root, a, mut owner) = original();
        std::fs::rename(a.plist_path(), dir.path().join("saved")).unwrap();
        std::fs::write(a.plist_path(), owner.definition_bytes()).unwrap();
        assert!(verify_original_observation(&a, &root, &mut owner, [7; 32], true).is_err());
        assert!(owner.is_revoked());
    }
}
