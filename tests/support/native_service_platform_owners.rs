//! Native original-owner tests complement the full shared-controller Clean cases.
use super::setup::{Fault, IntegrationFixture};
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use theseus::native::integration::model::ManagerKind;
use theseus_native_protocol::v2;

#[cfg(target_os = "macos")]
#[test]
fn launchd_wrong_domain_and_replaced_plist_never_authorize_stop_or_bootout() {
    use theseus_host::services::launchd as host;
    let fixture =
        IntegrationFixture::supplied_manager(ManagerKind::LaunchdUser, Fault::None).unwrap();
    let (manifest, context) = fixture.native_input().unwrap();
    let v2::ServiceAuthority::Launchd {
        domain: v2::LaunchdDomain::User { uid },
        label,
        definition_root,
        exclusive_load_roots,
        launchctl,
        qualification: q,
        ..
    } = manifest
        .service_authority
        .as_ref()
        .expect("service authority missing")
    else {
        panic!("explicit User domain required")
    };
    let authority = host::LaunchdAuthority {
        domain: host::LaunchdDomain::User(*uid),
        label: label.clone(),
        definition_root: definition_root.into(),
        exclusive_load_roots: exclusive_load_roots.iter().map(Into::into).collect(),
        launchctl: launchctl.into(),
        qualification: host::PrintQualification {
            absence_profile: q
                .absence_profile
                .as_ref()
                .map(|p| host::AbsenceQualification {
                    profile: p.profile.clone(),
                    exit_code: p.exit_code,
                    stderr: p.stderr.as_bytes().to_vec(),
                    receipt: p.receipt_sha256.clone(),
                }),
            os_version: q.os_version.clone(),
            os_build: q.os_build.clone(),
            api_profile: q.api_profile.clone(),
            receipt: q.receipt_sha256.clone(),
            launchctl_fingerprint: q.launchctl_fingerprint.clone(),
        },
        ownership_id: context.ownership_id.clone(),
        context_receipt: context
            .namespace_receipt
            .clone()
            .expect("service namespace receipt missing"),
        allowed_start: true,
        allowed_stop: true,
    };
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut wrong = authority.clone();
    wrong.domain = host::LaunchdDomain::Gui(*uid);
    assert!(
        host::LaunchdManager::connect(wrong, deadline, &cancel).is_err(),
        "wrong domain rejected without native controls"
    );
    let receipt = fixture.native_sut_action("install_target").unwrap();
    let definition = manifest
        .definitions
        .iter()
        .find(|d| d.role == "target" && d.artifact == "target")
        .unwrap();
    let admitted = host::LaunchdDefinition {
        executable: definition.executable.clone(),
        arguments: definition.arguments.clone(),
        working_directory: definition.working_directory.clone(),
    };
    let mut manager = host::LaunchdManager::connect(authority.clone(), deadline, &cancel).unwrap();
    let mut original = manager
        .qualify_registration(&admitted, receipt, deadline, &cancel)
        .unwrap();
    let path = authority.plist_path();
    let retained = std::fs::File::open(&path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let saved = path.with_extension("original");
    std::fs::rename(&path, &saved).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert!(manager.verify_registration(&mut original).is_err());
    assert!(original.is_revoked() && manager.is_revoked());
    assert!(
        manager
            .action(&mut original, host::ServiceAction::Stop, deadline, &cancel)
            .is_err()
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&saved, &path).unwrap();
    assert!(
        manager.verify_registration(&mut original).is_err(),
        "restoring exact original object cannot restore revoked helper authority"
    );
    assert!(
        manager
            .action(&mut original, host::ServiceAction::Stop, deadline, &cancel)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    // Explicit SUT teardown is independent of this permanently revoked helper owner.
    fixture.native_sut_action("uninstall").unwrap();
    drop(retained);
    drop(original);
    drop(manager);
    fixture.finish_native_fixture().unwrap();
}

#[cfg(windows)]
#[test]
fn scm_delete_success_remains_pending_until_original_handles_close_and_name_is_absent() {
    use std::{ffi::c_void, ptr};
    use theseus_host::services::scm as host;
    type Handle = *mut c_void;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenSCManagerW(machine: *const u16, database: *const u16, access: u32) -> Handle;
        fn OpenServiceW(manager: Handle, name: *const u16, access: u32) -> Handle;
        fn CloseServiceHandle(handle: Handle) -> i32;
    }
    struct Original(Option<Handle>);
    impl Original {
        fn close(&mut self) {
            if let Some(raw) = self.0 {
                assert_ne!(unsafe { CloseServiceHandle(raw) }, 0);
                self.0 = None;
            }
        }
    }
    impl Drop for Original {
        fn drop(&mut self) {
            self.close();
        }
    }
    let fixture =
        IntegrationFixture::supplied_manager(ManagerKind::WindowsScm, Fault::None).unwrap();
    let (manifest, context) = fixture.native_input().unwrap();
    let v2::ServiceAuthority::Scm {
        service_name,
        qualification: q,
        preflight: p,
        ..
    } = manifest
        .service_authority
        .as_ref()
        .expect("service authority missing")
    else {
        panic!("SCM required")
    };
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    let receipt = fixture.native_sut_action("install_target").unwrap();
    let authority = scm_authority(service_name, q, p, &context);
    let mut manager = host::ScmManager::connect(authority, deadline, &cancel).unwrap();
    let definition = manifest
        .definitions
        .iter()
        .find(|d| d.role == "target" && d.artifact == "target")
        .unwrap()
        .scm
        .as_ref()
        .unwrap();
    let mut original = manager
        .qualify_registration(&raw(definition), receipt, deadline, &cancel)
        .unwrap();
    let manager_raw = unsafe { OpenSCManagerW(ptr::null(), ptr::null(), 1) };
    assert!(!manager_raw.is_null());
    let mut extra_manager = Original(Some(manager_raw));
    let name = service_name
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let extra_raw = unsafe { OpenServiceW(manager_raw, name.as_ptr(), 5) };
    assert!(!extra_raw.is_null());
    let mut extra = Original(Some(extra_raw));
    let delete_receipt = fixture.native_sut_action("uninstall").unwrap();
    let mut deletion = manager
        .close_after_sut_delete(&mut original, delete_receipt, deadline, &cancel)
        .unwrap();
    assert!(original.original_handle_closed());
    assert_ne!(
        manager
            .poll_deletion(&mut deletion, deadline, &cancel)
            .unwrap(),
        host::DeletionProgress::Complete,
        "extra original SC_HANDLE keeps real deletion pending"
    );
    extra.close();
    extra_manager.close();
    loop {
        match manager
            .poll_deletion(&mut deletion, deadline, &cancel)
            .unwrap()
        {
            host::DeletionProgress::Complete => break,
            host::DeletionProgress::SameNamePresent => {
                panic!("unrelated same-name object must never be adopted or deleted")
            }
            _ => {}
        }
        assert!(
            Instant::now() < deadline,
            "native original deletion never settled"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    fixture.finish_native_fixture().unwrap();
}

#[cfg(windows)]
fn raw(d: &v2::ScmDefinition) -> theseus_host::services::scm::ScmDefinition {
    theseus_host::services::scm::ScmDefinition {
        command_line: d.command_line.clone(),
        display_name: d.display_name.clone(),
        account: d.account.clone(),
        service_type: d.service_type,
        start_type: d.start_type,
        error_control: d.error_control,
        load_order_group: d.load_order_group.clone(),
        dependencies: d.dependencies.clone(),
        tag_id: d.tag_id,
        delayed_auto_start: d.delayed_auto_start,
        failure_actions: d
            .failure_actions
            .iter()
            .map(|a| theseus_host::services::scm::FailureAction {
                kind: a.kind,
                delay_ms: a.delay_ms,
            })
            .collect(),
        failure_reset_period: d.failure_reset_period,
        failure_reboot_message: d.failure_reboot_message.clone(),
        failure_command: d.failure_command.clone(),
        failure_actions_on_non_crash: d.failure_actions_on_non_crash,
        service_sid_type: d.service_sid_type,
        required_privileges: d.required_privileges.clone(),
        triggers_present: d.triggers_present,
        launch_protected: d.launch_protected,
    }
}

#[cfg(windows)]
fn scm_authority(
    service_name: &str,
    q: &v2::ManagerQualification,
    p: &v2::ScmPreflight,
    context: &v2::IntegrationRuntimeContext,
) -> theseus_host::services::scm::ScmAuthority {
    use theseus_host::services::scm as host;
    let authority = host::ScmAuthority {
        context: host::ScmContext::LocalMachine,
        service_name: service_name.to_owned(),
        ownership_id: context.ownership_id.clone(),
        context_receipt: context
            .namespace_receipt
            .clone()
            .expect("service namespace receipt missing"),
        qualification_receipt: q.receipt_sha256.clone(),
        allowed_access: 0x35,
        allowed_start: true,
        allowed_stop: true,
        allow_local_system: true,
        preflight: host::ScmPreflight {
            helper_sid: p.helper_sid.clone(),
            configured_account_sid: p.configured_account_sid.clone(),
            executable: p.executable.encode_utf16().collect(),
            definition: p.definition.encode_utf16().collect(),
            data: p.data.encode_utf16().collect(),
            pipe: p.pipe.encode_utf16().collect(),
            executable_and_definition_read_execute: p.executable_and_definition_read_execute,
            data_declared_modify_only: p.data_declared_modify_only,
            pipe_system_and_helper_only: p.pipe_system_and_helper_only,
            pipe_rejects_remote_clients: p.pipe_rejects_remote_clients,
            no_other_acl_grants: p.no_other_acl_grants,
            exact_access_receipt: p.exact_access_receipt.clone(),
        },
    };
    authority
}

#[cfg(windows)]
#[test]
fn scm_native_pipe_rejects_actual_helper_sid_before_challenge() {
    use std::{ffi::c_void, os::windows::io::AsRawHandle};
    use theseus_host::services::readiness::*;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn PeekNamedPipe(
            pipe: *mut c_void,
            buffer: *mut c_void,
            size: u32,
            read: *mut u32,
            available: *mut u32,
            left: *mut u32,
        ) -> i32;
    }
    let fixture =
        IntegrationFixture::supplied_manager(ManagerKind::WindowsScm, Fault::None).unwrap();
    let (manifest, _) = fixture.native_input().unwrap();
    let v2::ServiceAuthority::Scm { preflight, .. } = manifest
        .service_authority
        .expect("service authority missing")
    else {
        panic!("SCM required")
    };
    let helper = Sid::new(preflight.helper_sid).unwrap();
    let endpoint = NamedPipe::new(preflight.pipe.clone()).unwrap();
    let mut channel = LocalChallenge::bind(endpoint, helper.clone()).unwrap();
    // The actual test process has already been measured against supplied helper SID.
    // Retain its real native client HANDLE; no token creation or impersonation.
    let client = std::thread::spawn(move || {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(preflight.pipe)
            .unwrap()
    });
    let expectation = ReadinessExpectation {
        artifact: ArtifactRole::Target,
        artifact_length: 1,
        artifact_sha256: [1; 32],
        version_literal: "target\n".into(),
        data_sha256: [2; 32],
        configuration_digest: [2; 32],
    };
    let challenge = ReadinessChallenge {
        protocol: ReadyProtocol::TheseusFixtureReadyV1,
        lease: "fixture-original-lease".into(),
        slot: "service".into(),
        resource_binding_digest: [3; 32],
        start_or_activation_operation: "fixture-original-start".into(),
        action_digest: [4; 32],
        nonce: [5; 16],
        expectation_digest: [6; 32],
    };
    let peer = PipePeerExpectation {
        sid: Sid::new("S-1-5-18".into()).unwrap(),
        original_pid: std::process::id(),
        cooperative_instance: "operator-original-system-service-instance".into(),
        instance_binding_digest: [7; 32],
    };
    let observed = channel
        .probe(
            &challenge,
            &expectation,
            &peer,
            Instant::now() + Duration::from_secs(30),
            &AtomicBool::new(false),
        )
        .unwrap();
    let client = client.join().unwrap();
    assert_eq!(observed.state, ReadinessState::Failed);
    assert_eq!(observed.failure, Some(ReadinessFailure::WrongPeer));
    assert_eq!(observed.peer_sid, Some(helper));
    assert!(observed.response.is_none() && !observed.authenticated_instance);
    let mut available = 0;
    assert_ne!(
        unsafe {
            PeekNamedPipe(
                client.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        },
        0
    );
    assert_eq!(
        available, 0,
        "principal rejection precedes challenge disclosure"
    );
    assert!(channel.is_revoked());
    channel.cleanup().unwrap();
    drop(client);
    fixture.finish_native_fixture().unwrap();
}
