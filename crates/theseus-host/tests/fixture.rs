use std::ffi::OsString;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn fixture() -> Command {
    Command::new(env!("CARGO_BIN_EXE_theseus-host-fixture"))
}

fn records(mut bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        assert!(bytes.len() >= 8, "truncated length prefix");
        let len = u64::from_le_bytes(bytes[..8].try_into().unwrap()) as usize;
        bytes = &bytes[8..];
        assert!(len <= bytes.len(), "truncated payload");
        result.push(bytes[..len].to_vec());
        bytes = &bytes[len..];
    }
    result
}

#[test]
fn argv_preserves_empty_unicode_quotes_and_native_bytes() {
    let mut args = vec![
        OsString::from(""),
        OsString::from("a b"),
        OsString::from("\"x\\"),
        OsString::from("é文"),
        OsString::from("$HOME; echo unsafe"),
    ];
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        args.push(OsString::from_vec(vec![0xff, b'x']));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        args.push(OsString::from_wide(&[0xd800, b'x' as u16]));
    }
    let output = fixture().arg("echo-argv").args(&args).output().unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert!(output.stderr.is_empty());
    let expected: Vec<Vec<u8>> = args
        .iter()
        .map(|arg| arg.as_encoded_bytes().to_vec())
        .collect();
    assert_eq!(records(&output.stdout), expected);
    // Independently pin the wire width/endianness and empty-argument framing.
    let output = fixture().args(["echo-argv", "", "abc"]).output().unwrap();
    assert_eq!(output.stdout, b"\0\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0abc");
}

#[test]
fn environment_returns_only_the_named_native_value() {
    let output = fixture()
        .args(["echo-env", "THESEUS_FIXTURE_VALUE"])
        .env("THESEUS_FIXTURE_VALUE", "a = 文")
        .env("THESEUS_FIXTURE_SECRET", "must-not-appear")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(records(&output.stdout), vec![b"a = \xe6\x96\x87".to_vec()]);
    assert!(output.stderr.is_empty());
    let empty = fixture()
        .args(["echo-env", "THESEUS_FIXTURE_VALUE"])
        .env("THESEUS_FIXTURE_VALUE", "")
        .output()
        .unwrap();
    assert!(empty.status.success());
    assert_eq!(empty.stdout, [0; 8]);
    let missing = fixture()
        .args(["echo-env", "THESEUS_FIXTURE_VALUE"])
        .env_remove("THESEUS_FIXTURE_VALUE")
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
}

#[test]
fn byte_echo_does_not_translate_binary_data() {
    let output = fixture()
        .args(["echo-bytes", "000aff0d8041"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, [0, 10, 255, 13, 128, 65]);
    assert!(output.stderr.is_empty());
}

#[test]
fn flood_has_exact_counts_on_both_streams_past_pipe_capacity() {
    for (args, out_len, err_len) in [
        (vec!["flood", "131072"], 131072, 131072),
        (vec!["flood", "65537", "131073"], 65537, 131073),
        (vec!["flood", "0"], 0, 0),
    ] {
        let output = fixture().args(args).output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, vec![b'O'; out_len]);
        assert_eq!(output.stderr, vec![b'E'; err_len]);
    }
}

#[test]
fn input_is_echoed_only_after_eof_and_respects_limit() {
    let mut child = fixture()
        .args(["read-until-eof", "6"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(&[0, 255, 10, 13, 65, 66]).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    assert!(child.try_wait().unwrap().is_none(), "must wait for EOF");
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, [0, 255, 10, 13, 65, 66]);

    let mut child = fixture()
        .args(["read-until-eof", "1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"ab").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "over-limit input is not partially echoed"
    );
}

#[test]
fn holding_and_ignoring_input_are_explicitly_time_bounded() {
    for mode in ["hold-pipes", "ignore-stdin"] {
        let start = Instant::now();
        let output = fixture()
            .args([mode, "80"])
            .stdin(Stdio::piped())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(start.elapsed() >= Duration::from_millis(80));
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn descendant_outlives_root_and_releases_inherited_pipes() {
    let start = Instant::now();
    let mut child = fixture()
        .args(["descendant", "2000"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    // The root returns without waiting; its native descendant owns the pipe ends.
    let root_deadline = start + Duration::from_millis(1500);
    let root_status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= root_deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("root waited for its descendant");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(root_status.success());
    assert!(start.elapsed() < Duration::from_millis(1500));
    let mut bytes = Vec::new();
    stdout.read_to_end(&mut bytes).unwrap();
    assert!(start.elapsed() >= Duration::from_millis(2000));
    let pid: u32 = std::str::from_utf8(&bytes).unwrap().trim().parse().unwrap();
    assert_ne!(pid, 0);
    let mut errors = Vec::new();
    stderr.read_to_end(&mut errors).unwrap();
    assert!(errors.is_empty());
    // Both inherited streams reach EOF: the bounded descendant released them.
}

#[test]
fn malformed_commands_fail_without_output() {
    for args in [
        vec![],
        vec!["unknown"],
        vec!["echo-env"],
        vec!["echo-env", "a=b"],
        vec!["echo-bytes", "0"],
        vec!["echo-bytes", "zz"],
        vec!["flood", "-1"],
        vec!["flood", "67108865"],
        vec!["hold-pipes", "60001"],
        vec!["ignore-stdin", "no"],
        vec!["descendant", "0", "extra"],
        vec!["read-until-eof", "67108865"],
    ] {
        let output = fixture().args(&args).output().unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
    }
}
