#[cfg(unix)]
mod unix {
    use super::super::*;
    use crate::{
        desktop::session::BrokerEndpoint,
        principal::current_principal,
        services::readiness::UnixPath,
        stdio::{ReadStep, WriteStep},
    };
    use std::{
        fs,
        os::unix::fs::{FileTypeExt, PermissionsExt},
        time::{Duration, Instant},
    };
    fn fixture() -> (tempfile::TempDir, BrokerEndpoint, PeerCeiling) {
        let dir = tempfile::Builder::new()
            .prefix("tb-")
            .tempdir_in(fs::canonicalize("/tmp").unwrap())
            .unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint = BrokerEndpoint::Unix(
            UnixPath::new(dir.path().join("broker.sock").to_str().unwrap().into()).unwrap(),
        );
        let ceiling = PeerCeiling {
            principal: current_principal().unwrap(),
            session_id: None,
        };
        (dir, endpoint, ceiling)
    }
    fn pair(
        endpoint: BrokerEndpoint,
        ceiling: PeerCeiling,
    ) -> (LocalListener, AuthenticatedStream, AuthenticatedStream) {
        let mut listener =
            LocalListener::bind(endpoint.clone(), ceiling.clone(), ceiling.clone()).unwrap();
        let mut client = AuthenticatedStream::connect(endpoint, ceiling.clone(), ceiling).unwrap();
        let mut server = None;
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            let connected = client.poll_connected().unwrap();
            if server.is_none() {
                server = listener.try_accept().unwrap();
            }
            if connected && server.is_some() {
                break;
            }
            assert!(Instant::now() < until);
            std::thread::yield_now();
        }
        (listener, client, server.unwrap())
    }
    // Catches authenticating declared JSON instead of the connected kernel peer.
    #[test]
    fn broker_uds_authenticates_explicit_kernel_peer_and_transfers_bounded_bytes() {
        let (dir, endpoint, ceiling) = fixture();
        let (mut listener, mut client, mut server) = pair(endpoint, ceiling.clone());
        assert_eq!(client.identity().unwrap().principal, ceiling.principal);
        assert_eq!(server.identity().unwrap().principal, ceiling.principal);
        assert!(
            fs::symlink_metadata(dir.path().join("broker.sock"))
                .unwrap()
                .file_type()
                .is_socket()
        );
        let bytes = b"explicit broker frame bytes";
        assert!(
            matches!(client.try_write(bytes).unwrap(), WriteStep::Bytes(n) if n == bytes.len())
        );
        let mut output = [0u8; 4096];
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            match server.try_read(&mut output).unwrap() {
                ReadStep::Bytes(n) => {
                    assert_eq!(&output[..n], bytes);
                    break;
                }
                ReadStep::Pending => assert!(Instant::now() < until),
                ReadStep::Eof => panic!("unexpected EOF"),
            }
        }
        client.request_stop();
        server.request_stop();
        listener.request_stop();
        assert!(client.poll_settle().unwrap());
        assert!(server.poll_settle().unwrap());
        assert!(listener.poll_settle().unwrap());
        assert!(!dir.path().join("broker.sock").exists());
    }
    #[test]
    fn broker_uds_wrong_peer_cannot_send_a_protocol_frame() {
        let (_dir, endpoint, ceiling) = fixture();
        let mut listener =
            LocalListener::bind(endpoint.clone(), ceiling.clone(), ceiling.clone()).unwrap();
        let mut wrong = ceiling.clone();
        let crate::principal::CurrentPrincipal::Posix { effective_uid, .. } = &mut wrong.principal
        else {
            unreachable!()
        };
        *effective_uid = effective_uid.wrapping_add(1);
        let mut client = AuthenticatedStream::connect(endpoint, ceiling, wrong).unwrap();
        assert!(client.poll_connected().is_err());
        assert!(client.try_write(b"must never be sent").is_err());
        let mut server = listener.try_accept().unwrap().unwrap();
        let mut bytes = [0; 4096];
        assert!(!matches!(
            server.try_read(&mut bytes).unwrap(),
            ReadStep::Bytes(_)
        ));
        client.request_stop();
        server.request_stop();
        listener.request_stop();
        assert!(client.poll_settle().unwrap());
        assert!(server.poll_settle().unwrap());
        assert!(listener.poll_settle().unwrap());
    }
    #[test]
    fn broker_uds_never_replaces_existing_or_substituted_endpoint() {
        let (dir, endpoint, ceiling) = fixture();
        let mut listener =
            LocalListener::bind(endpoint.clone(), ceiling.clone(), ceiling.clone()).unwrap();
        assert!(LocalListener::bind(endpoint, ceiling.clone(), ceiling).is_err());
        let path = dir.path().join("broker.sock");
        fs::remove_file(&path).unwrap();
        let _replacement = std::os::unix::net::UnixListener::bind(&path).unwrap();
        listener.request_stop();
        assert!(listener.poll_settle().is_err());
        assert!(path.exists());
    }
    #[test]
    fn broker_uds_missing_or_shared_parent_is_refused_without_provisioning() {
        let (dir, _, ceiling) = fixture();
        let endpoint = BrokerEndpoint::Unix(
            UnixPath::new(
                dir.path()
                    .join("missing/broker.sock")
                    .to_str()
                    .unwrap()
                    .into(),
            )
            .unwrap(),
        );
        assert!(LocalListener::bind(endpoint, ceiling.clone(), ceiling.clone()).is_err());
        assert!(!dir.path().join("missing").exists());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let endpoint = BrokerEndpoint::Unix(
            UnixPath::new(dir.path().join("broker.sock").to_str().unwrap().into()).unwrap(),
        );
        assert!(LocalListener::bind(endpoint, ceiling.clone(), ceiling).is_err());
        assert!(!dir.path().join("broker.sock").exists());
    }
}
