//! Pure config/bootstrap tests: no host session, filesystem, SDK or activation.
use super::*;
use theseus_native_protocol::v2;
pub(crate) fn native_config() -> serde_json::Value {
    let expectation = v2::ReadinessDefinitionExpectation {
        role: "configured".into(),
        artifact: "target".into(),
        artifact_length: 3,
        artifact_sha256: "22".repeat(32),
        version_literal: "target".into(),
        data_sha256: "44".repeat(32),
        configuration_digest: "55".repeat(32),
    };
    let manifest:v2::IntegrationManifest=serde_json::from_value(serde_json::json!({"schema":1,"core_manifest_digest":"11".repeat(32),"desktop":{"schema":1,"namespace_policy":"exclusive_quiescent","core_manifest_digest":"11".repeat(32),"slot":"fixture_desktop","entry_root":"/fixture","entry":"fixture.desktop","entry_digest":"33".repeat(32),"executable_digest":"22".repeat(32),"allowed_modes":["native_activation"],"ready_endpoint":{"kind":"unix_path","path":"/fixture/ready.sock"},"close_endpoint":{"kind":"unix_path","path":"/fixture/close.sock"},"expectation_digest":expectation.digest().unwrap().to_hex()},"desktop_executable":{"root":"/fixture","relative_path":"app","length":3,"sha256":"22".repeat(32),"arguments":["/fixture/app","--theseus-bootstrap=theseus_fixture_ready_bootstrap_v1","--theseus-ready=/fixture/ready.sock","--theseus-close=/fixture/close.sock"],"environment":[],"working_directory":"/fixture"},"desktop_readiness":expectation,"desktop_native":{"kind":"linux_gio","bootstrap":"theseus_fixture_ready_bootstrap_v1","lifetime":"owned_primary_exited","sdk":{"glib_version":[2,90,1],"native_abi":"linux_x86_64","helper_root":"/fixture","helper_relative_path":"gio-helper","helper":{"length":42,"sha256":"66".repeat(32)},"sdk_receipt_sha256":"77".repeat(32),"helper_receipt_sha256":"88".repeat(32),"application_receipt_sha256":"99".repeat(32),"provenance":"provider_build_attested"}},"definitions":[],"artifact_slots":[{"slot":"fixture_desktop","relative_path":"fixture.desktop","length":128,"sha256":"33".repeat(32)}],"readiness_protocol":"theseus_fixture_ready_v1","desktop_capabilities":["inspect_desktop","native_activation","request_close"]})).unwrap();
    manifest.validate().unwrap();
    let full = v2::integration_manifest_digest(
        v2::Digest::parse(&manifest.core_manifest_digest).unwrap(),
        manifest.digest().unwrap(),
    );
    serde_json::json!({"kind":"theseus_native_desktop_broker","schema":1,"profile":"native_integration_broker_v1","ledger_root":"/fixture/ledger","workspace_root":"/fixture/workspace","manifest":manifest,"header":{"profile":"native_integration_broker_v1","binding":"11".repeat(16),"manifest_digest":full.to_hex(),"workspace_marker":"33".repeat(16),"os":"linux","target_id":"fixture","ownership_id":"44".repeat(16),"provider_epoch":"55".repeat(32),"session_id":"supplied-display","session_epoch":"66".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"x11","display":":91","authority_file":"/fixture/auth","bus":"/fixture/bus"},"allowed_modes":["native_activation"]}})
}
#[test]
fn closed_native_bootstrap_is_exact_static_and_never_per_action() {
    let value = native_config();
    let manifest: v2::IntegrationManifest =
        serde_json::from_value(value["manifest"].clone()).unwrap();
    assert!(native_policy::validate_static_bootstrap(&manifest).is_ok());
    for bad in [
        "--theseus-instance=action-digest",
        "--theseus-activation-nonce=nonce",
        "--theseus-bootstrap=theseus_fixture_ready_bootstrap_v1",
        "--theseus-ready=/foreign",
    ] {
        let mut changed = manifest.clone();
        changed
            .desktop_executable
            .as_mut()
            .unwrap()
            .arguments
            .push(bad.into());
        assert!(
            native_policy::validate_static_bootstrap(&changed).is_err(),
            "{bad}"
        );
    }
    let mut changed = manifest.clone();
    changed
        .desktop_executable
        .as_mut()
        .unwrap()
        .arguments
        .remove(1);
    assert!(native_policy::validate_static_bootstrap(&changed).is_err());
    assert_eq!(
        manifest.desktop_executable.unwrap().arguments[1],
        "--theseus-bootstrap=theseus_fixture_ready_bootstrap_v1"
    );
}
#[cfg(target_os = "linux")]
#[test]
fn linux_serialized_receipt_claims_cannot_admit_or_acquire_native_authority() {
    let value = native_config();
    let error = BrokerServerConfig::from_bytes(&serde_json::to_vec(&value).unwrap())
        .err()
        .expect("no authenticated qualification bridge");
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    assert!(
        error
            .to_string()
            .contains("authenticated original SDK/application qualification bridge")
    );
}
#[test]
fn pending_activation_and_probe_keep_the_original_deadline() {
    use crate::broker_transport::server::Pending;
    let deadline = std::time::Instant::now();
    let identity = v2::broker::BrokerOperationIdentity {
        helper_lease: "11".repeat(16),
        helper_operation: "22".repeat(16),
        broker_lease: "33".repeat(16),
        request_digest: "44".repeat(32),
        binding_digest: "55".repeat(32),
        activation_nonce: "66".repeat(16),
    };
    let pending = Pending::Activation {
        reply_to: 1,
        identity: Box::new(identity.clone()),
        deadline,
    };
    assert_eq!(pending.semantic_deadline(), deadline);
    let ready = Pending::Ready {
        reply_to: 2,
        identity: Box::new(identity),
        nonce: v2::BindingId::from_bytes([1; 16]),
        deadline,
    };
    assert_eq!(ready.semantic_deadline(), deadline);
}
