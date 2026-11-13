use super::*;
#[test]
fn closed_endpoint_rejects_dbus_grammar_and_unsafe_components() {
    for path in [
        "",
        "bus",
        "/tmp/../bus",
        "/tmp/./bus",
        "/tmp//bus",
        "/tmp/bus;unixexec:path=sh",
        "unix:path=/tmp/bus",
        "/tmp/bus,guid=00",
        "/tmp/$BUS",
        "/tmp/é",
        "/tmp/a\0b",
    ] {
        assert!(UnixPath::new(path.into()).is_err(), "accepted {path:?}");
    }
    assert!(UnixPath::new(format!("/{}", "a".repeat(100))).is_err());
    assert_eq!(
        UnixPath::new("/tmp/private/bus.sock".into())
            .unwrap()
            .address(),
        "unix:path=/tmp/private/bus.sock"
    );
}
