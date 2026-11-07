//! Native admission/worker proofs use an inert, nonexistent process ceiling.
//! No SSH material, provider, target or transport is opened by these fixtures.
use super::*;
use crate::mcp_server::{
    query,
    registry::{AdmissionDecision, Registry, RegistryUpdate},
    roots::Roots,
    types::*,
};
use crate::{
    engine::domain::*,
    native::{launch::LaunchAuthority, recipe::ValidatedNativeRecipe},
};
use std::time::{Duration, Instant};
fn instance() -> Instance {
    Instance::parse(&"a".repeat(32)).unwrap()
}
pub(crate) fn recipe_bytes() -> Vec<u8> {
    let mut steps = serde_json::Map::new();
    for id in ["install_target", "configure", "exercise", "uninstall"] {
        steps.insert(id.into(),serde_json::json!({"id":id,"program":id,"expected_status":{"kind":"posix_exit","code":0},"stdout_literal":null}));
    }
    serde_json::to_vec(&serde_json::json!({"kind":"theseus_native_recipe","schema":1,"os":"linux","manifest_digest":"1".repeat(64),"leaves":{"reader":"reader","app":"app","data":"data"},"base":{"length":1,"sha256":"2".repeat(64)},"target":{"length":1,"sha256":"3".repeat(64)},"scenarios":["clean"],"steps":steps,"checks":[{"id":"version","checkpoint":"target_installed","command":{"id":"version","program":"check_target","expected_status":{"kind":"posix_exit","code":0},"stdout_literal":"target"}},{"id":"data","checkpoint":"configured","command":{"id":"data","program":"check_data","expected_status":{"kind":"posix_exit","code":0},"stdout_literal":"data"}}],"limits":{"run_seconds":30,"inspection_seconds":30,"max_operations":128,"output_bytes":16384}})).unwrap()
}
fn launch(root: &std::path::Path) -> LaunchAuthority {
    let path = |leaf: &str| root.join(leaf).to_string_lossy().to_string();
    let value = serde_json::json!({"kind":"theseus_native_launch","schema":1,"os":"linux","target_id":"never-contacted-target","profile":"openssh_subsystem_v1","ssh_program":path("missing-ssh"),"openssh_version":"OpenSSH_10.0 inert receipt","host":"example.invalid","port":22,"user":"inert","subsystem":"inert","private_key":path("missing-key"),"host_keys":path("missing-host-keys"),"runtime_directory":path("missing-material"),"binding":"2".repeat(32),"manifest_digest":"1".repeat(64),"workspace_marker":"3".repeat(32),"provider_epoch":"4".repeat(64),"provider":{"name":"inert","program":path("missing-provider"),"args":[],"environment":[]},"baseline_label":"inert-unprovisioned"});
    LaunchAuthority::decode(&serde_json::to_vec(&value).unwrap()).unwrap()
}
fn args(recipe: &ValidatedNativeRecipe) -> StartArgs {
    serde_json::from_value(serde_json::json!({"server_instance":instance(),"request_key":"native","recipe_path":"native.json","recipe_digest":recipe.digest,"scenarios":["clean"],"log_limit":1024})).unwrap()
}
fn registry_ready(
    recipe: ValidatedNativeRecipe,
) -> (Registry, crate::mcp_server::registry::ReadyNativeRun) {
    let mut registry = Registry::new(instance());
    let AdmissionDecision::Reserved { handle, .. } = registry.admit(args(&recipe), true).unwrap()
    else {
        panic!()
    };
    registry
        .apply(&handle, RegistryUpdate::ValidationStarted { generation: 7 })
        .unwrap();
    registry
        .apply(
            &handle,
            RegistryUpdate::NativeValidation {
                generation: 7,
                result: Ok(recipe),
            },
        )
        .unwrap();
    assert!(registry.take_dispatch(&handle).is_none());
    let ready = registry.take_native_dispatch(&handle).unwrap();
    assert!(registry.take_native_dispatch(&handle).is_none());
    (registry, ready)
}
fn joined(worker: &mut EngineWorker) -> EngineCompletion {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(WorkerCompletion::Engine(result)) = worker.try_join() {
            return result;
        }
        assert!(Instant::now() < end, "native worker never actually joined");
        std::thread::yield_now();
    }
}
#[cfg(feature = "native-catalog-test-support")]
pub(crate) fn fixture_launch(root: &std::path::Path) -> LaunchAuthority {
    let helper = theseus_host::process::NativeCommand::new(root.join("missing-helper"));
    let mut provider = theseus_host::process::NativeCommand::new(root.join("missing-provider"));
    provider.environment = theseus_host::process::ChildEnvironment::Replace(vec![]);
    LaunchAuthority::fixture(
        helper,
        theseus_native_helper_store::StoreBinding {
            binding: theseus_native_protocol::v2::BindingId::parse(&"2".repeat(32)).unwrap(),
            manifest_digest: theseus_native_protocol::v2::Digest::parse(&"1".repeat(64)).unwrap(),
            workspace_marker: theseus_native_protocol::v2::WorkspaceMarker::parse(&"3".repeat(32))
                .unwrap(),
            os: theseus_native_protocol::v2::Os::Linux,
        },
        theseus_native_protocol::v2::Digest::parse(&"4".repeat(64)).unwrap(),
    )
    .with_fixture_provider("inert".into(), provider)
    .unwrap()
}
#[test]
fn native_admission_requires_server_ceiling_and_omits_docker_role() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let bytes = recipe_bytes();
    std::fs::write(root.join("native.json"), &bytes).unwrap();
    let recipe = ValidatedNativeRecipe::decode(&bytes, root.join("native.json")).unwrap();
    let mut args = args(&recipe);
    assert!(args.profiles.is_empty());
    let mut roots = Roots::open(&root, &root.join("output")).unwrap();
    assert!(query::validate_selected_start(&roots, &args).is_err());
    roots.native_target = Some(Arc::new(launch(&root)));
    assert!(matches!(
        query::validate_selected_start(&roots, &args).unwrap(),
        SelectedRecipe::Native(_)
    ));
    assert_eq!(std::fs::read_dir(roots.output.root()).unwrap().count(), 0);
    args.profiles = vec![crate::model::ProfileName::User];
    assert!(query::validate_selected_start(&roots, &args).is_err());
    args.profiles.clear();
    args.pull = true;
    assert!(query::validate_selected_start(&roots, &args).is_err());
    assert!(!root.join("missing-material").exists());
}
#[test]
fn native_validation_generation_and_narrowed_options_use_existing_admission() {
    let temp = tempfile::tempdir().unwrap();
    let recipe =
        ValidatedNativeRecipe::decode(&recipe_bytes(), temp.path().join("native.json")).unwrap();
    let mut registry = Registry::new(instance());
    let AdmissionDecision::Reserved { handle, .. } = registry.admit(args(&recipe), true).unwrap()
    else {
        panic!()
    };
    registry
        .apply(&handle, RegistryUpdate::ValidationStarted { generation: 7 })
        .unwrap();
    assert!(
        registry
            .apply(
                &handle,
                RegistryUpdate::NativeValidation {
                    generation: 8,
                    result: Ok(recipe.clone())
                }
            )
            .is_err()
    );
    assert!(registry.take_native_dispatch(&handle).is_none());
    registry
        .apply(
            &handle,
            RegistryUpdate::NativeValidation {
                generation: 7,
                result: Ok(recipe),
            },
        )
        .unwrap();
    let ready = registry.take_native_dispatch(&handle).unwrap();
    assert_eq!(ready.options.limits.output_bytes, 1024);
    assert!(ready.options.output_root.as_os_str().is_empty());
    assert!(registry.take_dispatch(&handle).is_none());
}
#[test]
fn native_worker_cancelled_before_prepare_binds_only_native_saved_family() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let recipe = ValidatedNativeRecipe::decode(&recipe_bytes(), root.join("native.json")).unwrap();
    let (_registry, ready) = registry_ready(recipe);
    let token = ready.token;
    token.cancel();
    let output = Arc::new(crate::report::create(&root.join("output")).unwrap());
    let mut worker = EngineWorker::spawn_native(
        ready.handle,
        ready.recipe,
        ready.options,
        token,
        output.clone(),
        Arc::new(launch(&root)),
    )
    .unwrap();
    let summary = joined(&mut worker).result.unwrap();
    assert_eq!(summary.outcome, crate::model::RunOutcome::Cancelled);
    assert_eq!(
        summary.report_path.file_name().unwrap(),
        crate::native::model::REPORT_FILENAME
    );
    assert_eq!(
        summary.journal_path.file_name().unwrap(),
        crate::native::model::JOURNAL_FILENAME
    );
    assert_eq!(
        summary.report_path.parent().unwrap().parent().unwrap(),
        output.root()
    );
    assert!(!summary.report_path.with_file_name("report.json").exists());
    let binding = worker.try_binding().unwrap();
    assert!(!binding.report_id.is_empty());
    let saved = crate::report::open_existing(summary.report_path.parent().unwrap()).unwrap();
    assert!(
        saved
            .load_native_journal()
            .unwrap()
            .provider_calls
            .is_empty()
    );
    assert!(saved.load_native_report().unwrap().scenarios.is_empty());
    assert!(!root.join("missing-material").exists());
}
#[test]
#[cfg(feature = "native-catalog-test-support")]
fn native_failed_preparation_keeps_original_owner_and_blocks_next_admission() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let recipe = ValidatedNativeRecipe::decode(&recipe_bytes(), root.join("native.json")).unwrap();
    let (mut registry, ready) = registry_ready(recipe.clone());
    let handle = ready.handle.clone();
    registry.apply(&handle, RegistryUpdate::Dispatched).unwrap();
    let output = Arc::new(crate::report::create(&root.join("output")).unwrap());
    let launch = fixture_launch(&root);
    let mut worker = EngineWorker::spawn_native(
        ready.handle,
        ready.recipe,
        ready.options,
        ready.token,
        output.clone(),
        Arc::new(launch),
    )
    .unwrap();
    let completion = joined(&mut worker);
    let returned_handle = completion.handle;
    let owner = completion.result.unwrap_err();
    let original = owner.primary().root_cause() as *const dyn std::error::Error;
    assert!(
        engine::native_cleanup_pending(owner.primary()),
        "view={:?}; original={:#}",
        owner.view(),
        owner.primary()
    );
    assert!(owner.view().cleanup_blocked);
    assert_eq!(
        owner.view().native,
        NativeCleanup::Unknown,
        "settled local processes do not become pending because provider disposition is unknown"
    );
    let facts = engine::native_cleanup_facts(owner.primary()).unwrap();
    assert!(!facts.local_pending && facts.remote_unknown);
    let owner = owner.finalize();
    assert!(std::ptr::addr_eq(owner.primary().root_cause(), original));
    assert_eq!(owner.view().native, NativeCleanup::Unknown);
    assert!(
        owner.view().cleanup_blocked,
        "local finalization cannot invent a remote receipt"
    );
    let completion = EngineCompletion {
        handle: returned_handle,
        result: Err(owner),
    };
    let binding = worker.try_binding().unwrap();
    registry
        .apply(&handle, RegistryUpdate::Bound(binding.clone()))
        .unwrap();
    registry
        .apply(&handle, RegistryUpdate::Joined(completion))
        .unwrap();
    let mut next = args(&recipe);
    next.request_key = Key::parse("next").unwrap();
    assert_eq!(
        registry.admit(next, true).unwrap_err().error.code,
        ErrorCode::CleanupPending
    );
    let saved = output.open_child(&binding.report_id).unwrap();
    let journal = saved.load_native_journal().unwrap();
    assert!(!journal.provider_calls.is_empty());
    assert!(journal.operations.is_empty());
    assert!(!root.join("missing-material").exists());
}
