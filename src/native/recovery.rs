//! Recovery imports original identities; journal bytes never grant new execution authority.
use super::{launch::LaunchAuthority, model::*};
use anyhow::{Result, bail, ensure};
use theseus_native_client::{ClientMode, RecoveredOperation, RecoveryImage};
use theseus_native_helper_store::{IntentRecord, IntentStore};
use theseus_native_protocol::v2;
pub fn recovery_mode(
    journal: &NativeJournal,
    ownership_id: &str,
    launch: &LaunchAuthority,
    store: &IntentStore,
) -> Result<ClientMode> {
    journal.validate()?;
    ensure!(
        journal.launch_fingerprint.as_deref() == Some(launch.view().fingerprint.as_str()),
        "native recovery requires unchanged explicit launch fingerprint"
    );
    let owner = journal
        .ownership
        .iter()
        .find(|o| o.ownership_id == ownership_id)
        .ok_or_else(|| anyhow::anyhow!("original native owner missing"))?;
    let (prepared, lease) = match &owner.ownership {
        NativeOwnership::Intent { prepared } => (prepared, None),
        NativeOwnership::Acquired { prepared, lease } => {
            (prepared, Some(v2::LeaseId::parse(lease)?))
        }
        NativeOwnership::Preparing { .. } => {
            bail!("provider preparation must query original provider identity")
        }
    };
    let expected = launch.binding();
    ensure!(
        prepared.binding == expected.binding.to_hex()
            && prepared.manifest_digest == expected.manifest_digest.to_hex()
            && prepared.workspace_marker == expected.workspace_marker.to_hex()
            && prepared.reset_epoch == launch.provider_epoch().to_hex(),
        "recovery launch differs from original binding/epoch"
    );
    let record = IntentRecord {
        intent: v2::IntentId::parse(&prepared.intent)?,
        binding: expected.binding,
        manifest_digest: expected.manifest_digest,
        workspace_marker: expected.workspace_marker,
    };
    let original = store.verify_original(&record)?;
    let operations = journal
        .operations
        .iter()
        .filter(|o| o.ownership_id == ownership_id)
        .map(|o| {
            Ok(RecoveredOperation {
                request: o.request()?,
                digest: v2::Digest::parse(&o.request_digest)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ClientMode::Recover {
        image: Box::new(RecoveryImage {
            binding: record,
            profile: launch.expected().profile,
            operations,
            lease,
        }),
        original,
    })
}
