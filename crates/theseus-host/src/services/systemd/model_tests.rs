use super::*;
fn authority() -> SystemdAuthority {
    SystemdAuthority {
        endpoint: UnixPath::new("/tmp/theseus/bus".into()).unwrap(),
        scope: SystemdScope::User,
        bus_uid: 1000,
        manager_uid: 1000,
        manager_unique_name: ":1.17".into(),
        manager_version: "262".into(),
        unit: "theseus-fixture.service".into(),
        definition_root: PathBuf::from("/tmp/theseus/units"),
        unit_load_paths: vec!["/tmp/theseus/units".into()],
        ownership_id: "reserved-context".into(),
        context_receipt: "provider-receipt".into(),
        allowed_start: true,
        allowed_stop: true,
    }
}
#[test]
fn model_rejects_incomplete_or_ambiguous_authority_before_library_loading() {
    let a = authority();
    assert!(a.validate().is_ok());
    let mut a = authority();
    a.unit = "other*.service".into();
    assert!(a.validate().is_err());
    let mut a = authority();
    a.manager_unique_name = MANAGER.into();
    assert!(a.validate().is_err());
    let mut a = authority();
    a.context_receipt.clear();
    assert!(a.validate().is_err());
    let mut a = authority();
    a.unit_load_paths.push(a.unit_load_paths[0].clone());
    assert!(a.validate().is_err());
    let mut a = authority();
    a.unit_load_paths.clear();
    assert!(a.validate().is_err());
}
#[test]
fn model_rejects_unbounded_or_relative_exec_definition() {
    let mut d = SystemdDefinition {
        executable: "/fixture/service".into(),
        arguments: vec!["/fixture/service".into()],
        environment: vec!["A=value".into()],
        working_directory: "/fixture".into(),
    };
    assert!(d.validate().is_ok());
    d.executable = "service".into();
    assert!(d.validate().is_err());
    d.executable = "/fixture/service".into();
    d.environment = vec!["A=1".into(), "A=2".into()];
    assert!(d.validate().is_err());
    d.environment.clear();
    d.arguments.clear();
    assert!(d.validate().is_err());
}
