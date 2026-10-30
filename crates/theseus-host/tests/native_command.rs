use std::ffi::OsString;
use std::fs::File;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use theseus_host::process::{
    ChildEnvironment, HostCleanup, LaunchPhase, NativeCommand, OwnedChild,
};

fn command(args: &[&str]) -> NativeCommand {
    NativeCommand {
        program: env!("CARGO_BIN_EXE_theseus-host-fixture").into(),
        args: args.iter().map(OsString::from).collect(),
        cwd: None,
        environment: ChildEnvironment::Inherit,
    }
}

fn capture(command: &NativeCommand) -> (std::process::ExitStatus, Vec<u8>, Vec<u8>) {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("out");
    let err = root.path().join("err");
    let mut child = OwnedChild::spawn_files(
        command,
        None,
        Some(File::create(&out).unwrap()),
        Some(File::create(&err).unwrap()),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !child.exited_reserved().unwrap() {
        assert!(Instant::now() < deadline, "fixture failed to exit");
        std::thread::sleep(Duration::from_millis(2));
    }
    // In particular Darwin must repeatedly observe without consuming wait status.
    for _ in 0..3 {
        assert!(child.exited_reserved().unwrap());
    }
    child.terminate_owned().unwrap();
    let status = child.reap_and_release().unwrap();
    (
        status,
        std::fs::read(out).unwrap(),
        std::fs::read(err).unwrap(),
    )
}

fn std_capture(command: &NativeCommand) -> std::process::Output {
    let mut reference = Command::new(&command.program);
    reference.args(&command.args).stdin(Stdio::null());
    if let Some(cwd) = &command.cwd {
        reference.current_dir(cwd);
    }
    if let ChildEnvironment::Replace(env) = &command.environment {
        reference.env_clear().envs(env.iter().cloned());
    }
    reference.output().unwrap()
}

fn parity(command: &NativeCommand) {
    let expected = std_capture(command);
    let (status, stdout, stderr) = capture(command);
    assert_eq!(status.code(), expected.status.code());
    assert_eq!(stdout, expected.stdout);
    assert_eq!(stderr, expected.stderr);
}

#[test]
fn argv_matches_independent_std_launch() {
    for args in [
        vec![],
        vec![""],
        vec!["a b", "tab\there"],
        vec!["quote\"", "tail\\"],
        vec!["λ", "C:\\space path\\"],
        vec!["a\\\"b", "a\\\\\"b", "a\\\\\\\"b", "\\\\\\"],
    ] {
        let mut cmd = command(&["echo-argv"]);
        cmd.args.extend(args.into_iter().map(OsString::from));
        parity(&cmd);
    }
    let mut cmd = command(&["echo-argv"]);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        cmd.args.push(OsString::from_vec(vec![0xff, b'x']));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        cmd.args.push(OsString::from_wide(&[0xd800, b'x' as u16]));
    }
    parity(&cmd);
}

#[test]
fn replacement_environment_empty_values_and_collisions_match_std() {
    for env in [
        vec![],
        vec![("THESEUS_COMMAND_VALUE", "")],
        vec![
            ("THESEUS_COMMAND_VALUE", "λ = x"),
            ("THESEUS_COMMAND_VALUE", "last"),
        ],
        vec![("PATH", "isolated-upper"), ("Path", "isolated-mixed")],
    ] {
        for key in ["THESEUS_COMMAND_VALUE", "PATH", "Path"] {
            let mut cmd = command(&["echo-env", key]);
            cmd.environment = ChildEnvironment::Replace(
                env.iter()
                    .map(|(k, v)| ((*k).into(), (*v).into()))
                    .collect(),
            );
            parity(&cmd);
        }
    }
    // Only explicitly named inherited values are echoed; no ambient secrets dumped.
    parity(&command(&["echo-env", "PATH"]));
}

#[test]
fn cwd_and_executable_path_with_spaces_match_std() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("space λ directory");
    std::fs::create_dir(&dir).unwrap();
    let fixture = dir.join(if cfg!(windows) {
        "native fixture.exe"
    } else {
        "native fixture"
    });
    // Avoid opening an executable for writing concurrently with other tests'
    // forks: a fork can transiently retain that writer and cause ETXTBSY.
    #[cfg(unix)]
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_theseus-host-fixture"), &fixture).unwrap();
    #[cfg(windows)]
    std::fs::copy(env!("CARGO_BIN_EXE_theseus-host-fixture"), &fixture).unwrap();
    let mut cmd = command(&["echo-cwd"]);
    cmd.program = fixture.into();
    cmd.cwd = Some(dir);
    parity(&cmd);
}

#[test]
fn malformed_command_is_never_created() {
    let mut cases = Vec::new();
    let mut cmd = command(&["echo-argv", "bad\0arg"]);
    cases.push(cmd);
    cmd = command(&[]);
    cmd.program = "bad\0program".into();
    cases.push(cmd);
    cmd = command(&[]);
    cmd.environment = ChildEnvironment::Replace(vec![("bad=name".into(), "x".into())]);
    cases.push(cmd);
    cmd = command(&[]);
    cmd.environment = ChildEnvironment::Replace(vec![("key".into(), "bad\0value".into())]);
    cases.push(cmd);
    cmd = command(&[]);
    cmd.cwd = Some(std::path::PathBuf::from("bad\0cwd"));
    cases.push(cmd);
    cmd = command(&[]);
    cmd.program = "unsupported.cmd".into();
    cases.push(cmd);
    cmd = command(&[]);
    cmd.program = "unsupported.BAT".into();
    cases.push(cmd);
    for cmd in cases {
        let failure = OwnedChild::spawn_files(&cmd, None, None, None).unwrap_err();
        assert_eq!(failure.launch, LaunchPhase::NeverCreated);
        assert!(matches!(failure.cleanup, HostCleanup::Complete));
        assert_eq!(failure.error.kind(), std::io::ErrorKind::InvalidInput);
    }
}

#[test]
fn exited_leader_retains_identity_until_group_cleanup_and_single_reap() {
    let mut child = OwnedChild::spawn_files(&command(&["echo-argv"]), None, None, None).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !child.exited_reserved().unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    for _ in 0..10 {
        assert!(child.exited_reserved().unwrap());
    }
    child.terminate_owned().unwrap();
    assert!(child.reap_and_release().unwrap().success());
}

#[test]
fn file_input_delivers_exact_bytes_and_closes_at_eof() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("input");
    let output = root.path().join("output");
    let bytes = [0, 255, 10, 13, 65, 66];
    std::fs::write(&input, bytes).unwrap();
    let mut child = OwnedChild::spawn_files(
        &command(&["read-until-eof", "6"]),
        Some(File::open(input).unwrap()),
        Some(File::create(&output).unwrap()),
        None,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !child.exited_reserved().unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(child.reap_and_release().unwrap().success());
    assert_eq!(std::fs::read(output).unwrap(), bytes);
}
