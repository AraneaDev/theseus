//! Pure protocol-to-Host endpoint grammar, without creating or opening any pipe.
use theseus_host::services::readiness::NamedPipe;
use theseus_native_protocol::v2::{LocalEndpoint, broker::validate_endpoint};
#[test]
fn desktop_named_pipe_wire_is_the_exact_host_local_path() {
    for path in [
        r"\\.\pipe\theseus.original-ready".to_owned(),
        format!(r"\\.\pipe\theseus.{}", "a".repeat(64)),
    ] {
        NamedPipe::new(path.clone()).unwrap();
        validate_endpoint(&LocalEndpoint::NamedPipe { path })
            .expect("a declared full Host path must reach the same actual pipe");
    }
    for path in [
        "original-ready".to_owned(),
        r"\\server\pipe\theseus.remote".into(),
        r"\\.\pipe\arbitrary".into(),
        r"\\.\pipe\theseus.".into(),
        r"\\.\pipe\theseus.bad/name".into(),
        format!(r"\\.\pipe\theseus.{}", "a".repeat(65)),
    ] {
        assert!(NamedPipe::new(path.clone()).is_err());
        assert!(validate_endpoint(&LocalEndpoint::NamedPipe { path }).is_err());
    }
}
