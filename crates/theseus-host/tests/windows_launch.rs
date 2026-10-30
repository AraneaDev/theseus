#![cfg(windows)]
use std::fs::File;
use std::time::{Duration, Instant};
use theseus_host::process::{ChildEnvironment, NativeCommand, OwnedChild};

fn command(args: &[&str]) -> NativeCommand {
    NativeCommand {
        program: env!("CARGO_BIN_EXE_theseus-host-fixture").into(),
        args: args.iter().map(Into::into).collect(),
        cwd: None,
        environment: ChildEnvironment::Inherit,
    }
}
fn wait_exit(child: &mut OwnedChild) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !child.exited_reserved().unwrap() {
        assert!(Instant::now() < deadline, "native process did not exit");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn resumed_fixture_marker_and_nested_job_launch_succeed() {
    let root = tempfile::tempdir().unwrap();
    for mode in ["launch-marker", "nested-launch"] {
        let marker = root.path().join(mode);
        let mut cmd = command(&[mode]);
        cmd.args.push(marker.clone().into());
        let mut child = OwnedChild::spawn_files(&cmd, None, None, None).unwrap();
        wait_exit(&mut child);
        assert!(child.reap_and_release().unwrap().success());
        assert_eq!(std::fs::read(marker).unwrap(), b"native fixture ran");
    }
}

#[test]
fn declared_path_search_uses_native_exe_and_never_a_batch_fallback() {
    let root = tempfile::tempdir().unwrap();
    let exe = root.path().join("isolated-tool.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_theseus-host-fixture"), exe).unwrap();
    let marker = root.path().join("marker");
    let mut cmd = command(&["launch-marker"]);
    cmd.program = "isolated-tool".into();
    cmd.args.push(marker.clone().into());
    cmd.environment = ChildEnvironment::Replace(vec![("Path".into(), root.path().into())]);
    let mut child = OwnedChild::spawn_files(&cmd, None, None, None).unwrap();
    wait_exit(&mut child);
    assert!(child.reap_and_release().unwrap().success());
    assert_eq!(std::fs::read(marker).unwrap(), b"native fixture ran");
    std::fs::write(root.path().join("batch-only.cmd"), "@echo unsafe").unwrap();
    cmd.program = "batch-only".into();
    assert!(OwnedChild::spawn_files(&cmd, None, None, None).is_err());
}

#[test]
fn child_inherits_only_stdio_allowlist() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    let root = tempfile::tempdir().unwrap();
    let held = File::create(root.path().join("must-not-inherit")).unwrap();
    // An explicitly inheritable unrelated handle catches missing HANDLE_LIST.
    unsafe {
        assert_ne!(
            SetHandleInformation(
                held.as_raw_handle(),
                HANDLE_FLAG_INHERIT,
                HANDLE_FLAG_INHERIT
            ),
            0
        );
    }
    let mut cmd = command(&["probe-handle"]);
    cmd.args
        .push((held.as_raw_handle() as usize).to_string().into());
    cmd.args.push("must-not-inherit".into());
    let mut child = OwnedChild::spawn_files(&cmd, None, None, None).unwrap();
    wait_exit(&mut child);
    assert!(child.reap_and_release().unwrap().success());
}

#[test]
fn exited_leader_drop_kills_descendant_with_inherited_file() {
    let root = tempfile::tempdir().unwrap();
    let ready = root.path().join("ready");
    let late = root.path().join("late");
    let mut cmd = command(&["descendant-marker"]);
    cmd.args.extend([ready.clone().into(), late.clone().into()]);
    let mut child = OwnedChild::spawn_files(&cmd, None, None, None).unwrap();
    wait_exit(&mut child);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    for _ in 0..10 {
        assert!(child.exited_reserved().unwrap());
    }
    drop(child);
    std::thread::sleep(Duration::from_millis(700));
    assert!(!late.exists(), "descendant survived owned job closure");
}

#[test]
fn kernel_closure_of_last_job_handle_kills_nested_descendant() {
    let root = tempfile::tempdir().unwrap();
    let ready = root.path().join("ready");
    let late = root.path().join("late");
    let mut cmd = command(&["close-job-owner"]);
    cmd.args.extend([ready.clone().into(), late.clone().into()]);
    let mut child = OwnedChild::spawn_files(&cmd, None, None, None).unwrap();
    wait_exit(&mut child);
    assert_eq!(std::fs::read(ready).unwrap(), b"native fixture ran");
    // Keep our outer job open while the inner job's kernel close kills the child.
    std::thread::sleep(Duration::from_millis(700));
    assert!(
        !late.exists(),
        "last job close did not kill nested descendant"
    );
    assert!(child.reap_and_release().unwrap().success());
}

#[test]
fn startup_and_process_records_match_native_x64_binding_layout() {
    use std::mem::{align_of, offset_of, size_of};
    use windows_sys::Win32::System::Threading::{
        PROCESS_INFORMATION, STARTUPINFOEXW, STARTUPINFOW,
    };
    assert_eq!(
        (size_of::<STARTUPINFOW>(), align_of::<STARTUPINFOW>()),
        (104, 8)
    );
    assert_eq!(offset_of!(STARTUPINFOW, hStdInput), 80);
    assert_eq!(offset_of!(STARTUPINFOW, hStdOutput), 88);
    assert_eq!(offset_of!(STARTUPINFOW, hStdError), 96);
    assert_eq!(
        (size_of::<STARTUPINFOEXW>(), align_of::<STARTUPINFOEXW>()),
        (112, 8)
    );
    assert_eq!(offset_of!(STARTUPINFOEXW, lpAttributeList), 104);
    assert_eq!(
        (
            size_of::<PROCESS_INFORMATION>(),
            align_of::<PROCESS_INFORMATION>()
        ),
        (24, 8)
    );
    assert_eq!(offset_of!(PROCESS_INFORMATION, hProcess), 0);
    assert_eq!(offset_of!(PROCESS_INFORMATION, hThread), 8);
}
