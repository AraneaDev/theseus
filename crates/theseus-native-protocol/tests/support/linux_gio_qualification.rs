//! Synthetic DTO fixture: not SDK/runtime acceptance.
use serde_json::json;
use theseus_native_protocol::v2::{self, broker::*};
fn artifact(root: &str, leaf: &str, byte: &str) -> v2::LinuxQualifiedArtifact {
    v2::LinuxQualifiedArtifact {
        root: root.into(),
        relative_path: leaf.into(),
        length: 128,
        sha256: byte.repeat(32),
        native_abi: v2::NativeLinuxAbi::LinuxX86_64,
    }
}
pub fn fixture() -> (
    v2::IntegrationManifest,
    v2::IntegrationRuntimeContext,
    v2::LinuxGioBuildAttestation,
) {
    let sdk = v2::LinuxSdkGraph {
        glib_version: [2, 80, 0],
        native_abi: v2::NativeLinuxAbi::LinuxX86_64,
        qualification_profile: "gio_compiled_native_v1".into(),
        images: vec![
            v2::LinuxSdkImage {
                role: v2::LinuxSdkImageRole::Gio,
                artifact: artifact("/qualified/sdk", "libgio.so", "11"),
            },
            v2::LinuxSdkImage {
                role: v2::LinuxSdkImageRole::Glib,
                artifact: artifact("/qualified/sdk", "libglib.so", "22"),
            },
            v2::LinuxSdkImage {
                role: v2::LinuxSdkImageRole::Gobject,
                artifact: artifact("/qualified/sdk", "libgobject.so", "33"),
            },
        ],
    };
    let helper = artifact("/qualified/helper", "gio-launch-desktop", "44");
    let app = artifact("/qualified/apps", "reader", "55");
    let ready:v2::ReadinessDefinitionExpectation=serde_json::from_value(json!({"role":"configured","artifact":"target","artifact_length":app.length,"artifact_sha256":app.sha256,"version_literal":"target","data_sha256":"77".repeat(32),"configuration_digest":"88".repeat(32)})).unwrap();
    let manifest:v2::IntegrationManifest=serde_json::from_value(json!({"schema":1,"core_manifest_digest":"11".repeat(32),"desktop":{"schema":1,"core_manifest_digest":"11".repeat(32),"slot":"desktop","entry_root":"/qualified/entries","namespace_policy":"exclusive_quiescent","entry":"app.desktop","entry_digest":"66".repeat(32),"executable_digest":app.sha256,"allowed_modes":["native_activation"],"ready_endpoint":{"kind":"unix_path","path":"/qualified/ready.sock"},"close_endpoint":{"kind":"unix_path","path":"/qualified/close.sock"},"expectation_digest":ready.digest().unwrap().to_hex()},"desktop_executable":{"root":app.root,"relative_path":app.relative_path,"length":app.length,"sha256":app.sha256,"arguments":["/qualified/apps/reader"],"environment":[],"working_directory":"/qualified/apps"},"desktop_readiness":ready,"desktop_native":{"kind":"linux_gio","bootstrap":"theseus_fixture_ready_bootstrap_v1","lifetime":"owned_primary_exited","sdk":{"glib_version":sdk.glib_version,"native_abi":sdk.native_abi,"helper_root":helper.root,"helper_relative_path":helper.relative_path,"helper":{"length":helper.length,"sha256":helper.sha256},"sdk_receipt_sha256":sdk.digest().unwrap().to_hex(),"helper_receipt_sha256":helper.digest().unwrap().to_hex(),"application_receipt_sha256":app.digest().unwrap().to_hex(),"provenance":"provider_build_attested"}},"definitions":[],"artifact_slots":[{"slot":"desktop","relative_path":"app.desktop","length":64,"sha256":"66".repeat(32)}],"readiness_protocol":"theseus_fixture_ready_v1","desktop_capabilities":["inspect_desktop","native_activation","request_close"]})).unwrap();
    manifest.validate().unwrap();
    let core = v2::Digest::parse(&manifest.core_manifest_digest).unwrap();
    let integration = manifest.digest().unwrap();
    let full = v2::integration_manifest_digest(core, integration);
    let header:IntegrationBrokerHeader=serde_json::from_value(json!({"profile":"native_integration_broker_v1","binding":"01".repeat(16),"manifest_digest":full.to_hex(),"workspace_marker":"02".repeat(16),"os":"linux","target_id":"fixture","ownership_id":"03".repeat(16),"provider_epoch":"04".repeat(32),"session_id":"supplied","session_epoch":"05".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/qualified/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"helper_principal":{"kind":"posix","effective_uid":1000,"effective_gid":1000},"display":{"kind":"x11","display":":92","authority_file":"/qualified/auth","bus":"/qualified/bus.sock"},"allowed_modes":["native_activation"]})).unwrap();
    let mut context:v2::IntegrationRuntimeContext=serde_json::from_value(json!({"kind":"theseus_native_integration_context","schema":1,"ownership_id":header.ownership_id,"binding":header.binding,"manifest_digest":header.manifest_digest,"workspace_marker":header.workspace_marker,"provider_epoch":header.provider_epoch,"integration_manifest_digest":integration.to_hex(),"desktop_broker_header":header})).unwrap();
    let attestation = v2::LinuxGioBuildAttestation {
        schema: 1,
        provider: "provider".into(),
        prepare_invocation_id: "12".repeat(16),
        prepare_invocation_digest: "13".repeat(32),
        launch_fingerprint: "14".repeat(32),
        target_id: header.target_id.clone(),
        ownership_id: context.ownership_id.clone(),
        manifest_digest: full.to_hex(),
        core_manifest_digest: core.to_hex(),
        integration_manifest_digest: integration.to_hex(),
        provider_epoch: context.provider_epoch.clone(),
        binding: context.binding.clone(),
        workspace_marker: context.workspace_marker.clone(),
        base_context_digest: context.qualification_binding_digest().unwrap().to_hex(),
        base_broker_header_digest: header.qualification_binding_digest().unwrap().to_hex(),
        namespace_policy: "exclusive_quiescent".into(),
        sdk,
        helper,
        applications: vec![v2::LinuxQualifiedApplication {
            definition: v2::DesktopDefinitionKey::CONFIGURED_TARGET,
            tuple_digest: manifest
                .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
                .unwrap()
                .digest()
                .unwrap()
                .to_hex(),
            artifact: app,
        }],
    };
    let reference = attestation.reference().unwrap();
    context.linux_gio_qualification = Some(reference.clone());
    context
        .desktop_broker_header
        .as_mut()
        .unwrap()
        .linux_gio_qualification = Some(reference);
    (manifest, context, attestation)
}
