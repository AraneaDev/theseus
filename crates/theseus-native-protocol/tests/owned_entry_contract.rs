//! Pure entry declarations; no installed bytes, SDK, session or process authority.
use serde_json::{Value, json};
use theseus_native_protocol::v2;
fn raw(windows: bool) -> Value {
    let (root, entry, executable, ready, close) = if windows {
        (
            r"C:\fixture",
            "Fixture.lnk",
            "app.exe",
            json!({"kind":"named_pipe","path":r"\\.\pipe\theseus.ready"}),
            json!({"kind":"named_pipe","path":r"\\.\pipe\theseus.close"}),
        )
    } else {
        (
            "/fixture",
            "Fixture.app",
            "Fixture.app/Contents/MacOS/app",
            json!({"kind":"unix_path","path":"/fixture/ready.sock"}),
            json!({"kind":"unix_path","path":"/fixture/close.sock"}),
        )
    };
    let program = if windows {
        format!("{root}\\{executable}")
    } else {
        format!("{root}/{executable}")
    };
    let expected = json!({"role":"configured","artifact":"target","artifact_length":123,"artifact_sha256":"33".repeat(32),"version_literal":"target","data_sha256":"44".repeat(32),"configuration_digest":"55".repeat(32)});
    let expectation: v2::ReadinessDefinitionExpectation =
        serde_json::from_value(expected.clone()).unwrap();
    json!({"schema":1,"core_manifest_digest":"11".repeat(32),"desktop":{"schema":1,"core_manifest_digest":"11".repeat(32),"slot":"desktop","entry_root":root,"namespace_policy":"exclusive_quiescent","entry":entry,"entry_digest":"22".repeat(32),"executable_digest":"33".repeat(32),"allowed_modes":["owned_executable"],"ready_endpoint":ready,"close_endpoint":close,"expectation_digest":expectation.digest().unwrap().to_hex()},"desktop_executable":{"root":root,"relative_path":executable,"length":123,"sha256":"33".repeat(32),"arguments":[program],"environment":[],"working_directory":root},"desktop_readiness":expected,"definitions":[],"artifact_slots":[{"slot":"desktop","relative_path":if windows {entry.to_string()}else {format!("{entry}/Contents/Info.plist")},"length":128,"sha256":"22".repeat(32)}],"readiness_protocol":"theseus_fixture_ready_v1","desktop_capabilities":["inspect_desktop","owned_executable","request_close"]})
}
fn entry() -> Value {
    json!({"kind":"macos_bundle","app_leaf":"Fixture.app","bundle_identifier":"org.theseus.fixture","executable_leaf":"app","info_plist":{"length":128,"sha256":"22".repeat(32)}})
}
fn parse(value: Value) -> Result<v2::IntegrationManifest, String> {
    let m: v2::IntegrationManifest = serde_json::from_value(value).map_err(|e| e.to_string())?;
    m.validate().map_err(|e| format!("{e:?}"))?;
    Ok(m)
}
fn mac() -> v2::IntegrationManifest {
    let mut value = raw(false);
    value["desktop_owned_entry"] = entry();
    parse(value).expect("committed owned bundle must be valid without native activation")
}
#[test]
fn owned_bundle_entry_has_separate_committed_identity_and_mac_primary_may_coexist() {
    let m = mac();
    assert!(m.desktop_native.is_none());
    let tuple = m
        .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
        .unwrap();
    assert_eq!(
        tuple.entry_relative_path(),
        "Fixture.app/Contents/Info.plist"
    );
    assert_eq!(
        m.desktop_entry_relative_path().unwrap(),
        tuple.entry_relative_path()
    );
    assert_eq!(
        m.native_entry_relative_path().unwrap(),
        tuple.entry_relative_path()
    );
    assert_eq!(tuple.owned_entry, m.desktop_owned_entry);
    assert_eq!(tuple.owned_entry.as_ref().unwrap().os(), "macos");
    let mut lifetime = serde_json::to_value(&m).unwrap();
    lifetime["desktop_owned_lifetime"] = json!({"kind":"macos_original_primary_v1"});
    assert!(parse(lifetime).is_ok());
    let encoded = serde_json::to_vec(&m).unwrap();
    assert_eq!(
        serde_json::from_slice::<v2::IntegrationManifest>(&encoded).unwrap(),
        m
    );
}
#[test]
fn legacy_none_fields_preserve_manifest_and_tuple_bytes_and_digest() {
    let mut value = raw(true);
    let old = parse(value.clone()).unwrap();
    value["desktop_owned_entry"] = Value::Null;
    let new = parse(value).unwrap();
    assert_eq!(
        serde_json::to_vec(&old).unwrap(),
        serde_json::to_vec(&new).unwrap()
    );
    assert_eq!(old.digest().unwrap(), new.digest().unwrap());
    assert!(
        serde_json::to_value(&new)
            .unwrap()
            .get("desktop_owned_entry")
            .is_none()
    );
    let tuple = old
        .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
        .unwrap();
    let mut value = serde_json::to_value(&tuple).unwrap();
    value["owned_entry"] = Value::Null;
    let omitted: v2::DesktopDefinition = serde_json::from_value(value).unwrap();
    assert_eq!(
        serde_json::to_vec(&tuple).unwrap(),
        serde_json::to_vec(&omitted).unwrap()
    );
    assert_eq!(tuple.digest().unwrap(), omitted.digest().unwrap());
}
#[test]
fn owned_shortcut_requires_closed_target_cwd_and_retains_legacy_full_tree_lifetime() {
    let mut value = raw(true);
    value["desktop_owned_entry"] = json!({"kind":"windows_shortcut"});
    let m = parse(value.clone()).unwrap();
    assert_eq!(m.desktop_owned_entry.as_ref().unwrap().os(), "windows");
    assert!(m.desktop_owned_lifetime.is_none());
    assert_eq!(m.desktop_entry_relative_path().unwrap(), "Fixture.lnk");
    value["desktop_executable"]["working_directory"] = json!(r"C:\other");
    assert!(parse(value).is_err());
}
#[test]
fn owned_entry_rejects_native_authority_bootstrap_and_wrong_metadata_paths() {
    let valid = serde_json::to_value(mac()).unwrap();
    for (field, value) in [
        ("app_leaf", json!("Other.app")),
        ("bundle_identifier", json!("bad..id")),
        ("executable_leaf", json!("other")),
        ("info_plist", json!({"length":129,"sha256":"22".repeat(32)})),
        ("bootstrap", json!("theseus_fixture_ready_bootstrap_v1")),
        ("lifetime", json!("owned_primary_exited")),
    ] {
        let mut bad = valid.clone();
        bad["desktop_owned_entry"][field] = value;
        assert!(parse(bad).is_err(), "{field}");
    }
    let mut native = valid.clone();
    native["desktop_native"] = json!({"kind":"windows_shortcut","bootstrap":"theseus_fixture_ready_bootstrap_v1","lifetime":"owned_primary_exited"});
    assert!(parse(native).is_err());
    let mut mixed = valid;
    mixed["desktop"]["allowed_modes"] = json!(["owned_executable", "native_activation"]);
    assert!(parse(mixed).is_err());
}
#[test]
fn owned_entry_generation_identity_cannot_disappear_or_change_but_image_hash_can() {
    let m = mac();
    let tuple = m
        .desktop_definition(&v2::DesktopDefinitionKey::CONFIGURED_TARGET)
        .unwrap();
    let mut extra = tuple.clone();
    extra.key.role = v2::DesktopDefinitionRole::Target;
    extra.readiness.role = "target".into();
    extra.desktop.expectation_digest = extra.readiness.digest().unwrap().to_hex();
    assert!(tuple.same_namespace(&extra));
    let old_digest = extra.digest().unwrap();
    let mut raw = serde_json::to_value(&extra).unwrap();
    raw["owned_entry"]["info_plist"]["sha256"] = json!("aa".repeat(32));
    raw["desktop"]["entry_digest"] = json!("aa".repeat(32));
    let image: v2::DesktopDefinition = serde_json::from_value(raw).unwrap();
    image.validate().unwrap();
    assert!(tuple.same_namespace(&image));
    assert_ne!(old_digest, image.digest().unwrap());
    let mut missing = extra.clone();
    missing.owned_entry = None;
    assert!(!tuple.same_namespace(&missing));
    let mut changed = serde_json::to_value(&extra).unwrap();
    changed["owned_entry"]["bundle_identifier"] = json!("org.other.fixture");
    let changed: v2::DesktopDefinition = serde_json::from_value(changed).unwrap();
    assert!(!tuple.same_namespace(&changed));
    let mut bad = serde_json::to_value(&m).unwrap();
    bad["desktop_definitions"] = json!([missing]);
    assert!(parse(bad).is_err());
}

#[test]
fn owned_entry_context_platform_must_match_without_granting_session_authority() {
    let m = mac();
    let full = v2::integration_manifest_digest(
        v2::Digest::parse(&m.core_manifest_digest).unwrap(),
        m.digest().unwrap(),
    );
    let mut header = json!({"profile":"native_integration_broker_v1","binding":"11".repeat(16),"manifest_digest":full.to_hex(),"workspace_marker":"33".repeat(16),"os":"macos","target_id":"fixture","ownership_id":"44".repeat(16),"provider_epoch":"55".repeat(32),"session_id":"supplied","session_epoch":"66".repeat(32),"broker_endpoint":{"kind":"unix_path","path":"/fixture/broker.sock"},"broker_principal":{"kind":"posix","effective_uid":501,"effective_gid":20},"helper_principal":{"kind":"posix","effective_uid":501,"effective_gid":20},"display":{"kind":"mac_os_gui","domain":"gui/501"},"allowed_modes":["owned_executable"]});
    for (os, ok) in [(v2::Os::Macos, true), (v2::Os::Linux, false)] {
        if os == v2::Os::Linux {
            header["os"] = json!("linux");
            header["display"] = json!({"kind":"x11","display":":91","authority_file":"/fixture/auth","bus":"/fixture/bus"});
        }
        let context:v2::IntegrationRuntimeContext=serde_json::from_value(json!({"kind":"theseus_native_integration_context","schema":1,"ownership_id":header["ownership_id"],"binding":header["binding"],"manifest_digest":header["manifest_digest"],"workspace_marker":header["workspace_marker"],"provider_epoch":header["provider_epoch"],"integration_manifest_digest":m.digest().unwrap().to_hex(),"desktop_broker_header":header})).unwrap();
        assert_eq!(m.validate_context(&context, os).is_ok(), ok);
    }
}
