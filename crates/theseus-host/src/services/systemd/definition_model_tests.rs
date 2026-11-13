use super::*;
#[test]
fn model_refuses_hidden_exec_and_dependency_configuration() {
    let base = "[Unit]\nDescription=Fixture\nDefaultDependencies=no\n[Service]\nType=simple\nExecStart=/fixture/service\nWorkingDirectory=/fixture\nRestart=no\n";
    assert!(validate_definition_source(base.as_bytes()).is_ok());
    for extra in [
        "ExecStartPre=/unadmitted",
        "EnvironmentFile=/unadmitted",
        "Restart=always",
        "User=other",
        "Sockets=other.socket",
        "Type=forking",
        "ExecStart=/second",
        "[Install]\nWantedBy=default.target",
        "[Unit]\nRequires=unadmitted.service",
    ] {
        assert!(
            validate_definition_source(format!("{base}{extra}\n").as_bytes()).is_err(),
            "accepted {extra}"
        );
    }
}
#[test]
fn fixture_renderer_quoted_absolute_command_is_supported_without_prefix_escape() {
    let definition=b"[Unit]\nDescription=Theseus fixture\nDefaultDependencies=no\n[Service]\nType=simple\nRestart=no\nExecStart=\"/fixture/service\" \"service\" \"/fixture/ready.sock\"\nWorkingDirectory=/fixture\nEnvironment=\"VALUE=fixture value\"\n";
    assert!(validate_definition_source(definition).is_ok());
    for prefix in ["+", "!", "@", "-", ":"] {
        let text = format!("[Service]\nExecStart={prefix}/fixture/service\n");
        assert!(validate_definition_source(text.as_bytes()).is_err());
    }
}
#[test]
fn working_directory_is_admitted_only_as_the_literal_a_real_manager_reads() {
    // CI10 real systemd keeps quotes in path settings, so a quoted
    // WorkingDirectory would name a different, relative directory.
    let base = "[Unit]\nDescription=Theseus fixture\nDefaultDependencies=no\n[Service]\nType=simple\nRestart=no\nExecStart=\"/fixture/service\" \"service\"\n";
    assert!(
        validate_definition_source(
            format!("{base}WorkingDirectory=/fixture/work dir\n").as_bytes()
        )
        .is_ok()
    );
    for value in [
        "\"/fixture\"",
        "'/fixture'",
        "fixture",
        "/fix\"ture",
        "/fixture/%h",
        "/fixture/$HOME",
        "-/fixture",
        "~/x",
        "",
    ] {
        assert!(
            validate_definition_source(format!("{base}WorkingDirectory={value}\n").as_bytes())
                .is_err(),
            "{value}"
        );
    }
}
#[test]
fn cooperative_deadline_and_cancel_have_no_default_timeout_fallback() {
    let cancelled = AtomicBool::new(true);
    assert_eq!(
        remaining(Instant::now() + Duration::from_secs(1), &cancelled)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    let cancel = AtomicBool::new(false);
    assert_eq!(
        remaining(Instant::now() - Duration::from_secs(1), &cancel)
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert!(remaining(Instant::now() + Duration::from_secs(31), &cancel).is_err());
    assert!(remaining(Instant::now() + Duration::from_secs(1), &cancel).unwrap() > 0);
}
