//! Bounded pre-acquisition CLI checks only; not runtime/native pipe qualification.
use std::{
    ffi::OsString,
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn invoke(args: &[OsString]) -> (std::process::ExitStatus, Vec<u8>, Vec<u8>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_theseus-native-helper"))
        .args(args)
        .env("RUST_BACKTRACE", "full")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let mut killed = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() >= Duration::from_secs(5) && !killed {
            child.kill().unwrap();
            killed = true;
        }
        if started.elapsed() >= Duration::from_secs(6) {
            std::process::exit(2)
        } // child owner remains alive; no unbounded wait/detach.
        std::thread::sleep(Duration::from_millis(1));
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .take(1025)
        .read_to_end(&mut stdout)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .take(1025)
        .read_to_end(&mut stderr)
        .unwrap();
    assert!(!killed, "helper exceeded bounded pre-acquisition deadline");
    (status, stdout, stderr)
}
#[test]
fn invalid_cli_and_private_config_emit_only_fixed_diagnostic_before_acquisition() {
    let root = tempfile::tempdir().unwrap();
    let mut random = [0; 16];
    theseus_host::random_bytes(&mut random).unwrap();
    let nonce = random
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let path = root.path().join(format!("PRIVATE_{nonce}.json"));
    std::fs::write(&path, format!("{{\"PRIVATE_{nonce}\":\"\\ud800\"}}")).unwrap();
    for args in [
        vec![],
        vec!["serve".into()],
        vec!["serve".into(), "--config".into(), "relative".into()],
        vec![
            "serve".into(),
            "--config".into(),
            path.clone().into_os_string(),
        ],
        vec![
            "serve".into(),
            "--config".into(),
            root.path()
                .join(format!("MISSING_{nonce}"))
                .into_os_string(),
        ],
        vec![
            "serve".into(),
            "--config".into(),
            path.into_os_string(),
            "--config".into(),
        ],
    ] {
        let (status, stdout, stderr) = invoke(&args);
        assert_eq!(status.code(), Some(2));
        assert!(stdout.is_empty());
        assert_eq!(stderr, b"helper: invalid invocation\n");
        assert!(stderr.len() <= 128);
    }
}
