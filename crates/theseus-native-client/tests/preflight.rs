use theseus_host::process::{ChildEnvironment, NativeCommand};
use theseus_native_client::{Client, ClientError, ExpectedHelper};
use theseus_native_helper_store::StoreBinding;
use theseus_native_protocol::v2::*;
fn expected() -> ExpectedHelper {
    ExpectedHelper {
        profile: theseus_native_protocol::v2::HelperProfile::Fixture,
        binding: StoreBinding {
            binding: BindingId::from_bytes([1; 16]),
            manifest_digest: Digest::from_bytes([2; 32]),
            workspace_marker: WorkspaceMarker::from_bytes([3; 16]),
            os: Os::Linux,
        },
        programs: ProgramList::new(vec![ProgramId::new("probe").unwrap()]).unwrap(),
    }
}
#[test]
fn implicit_environment_is_rejected_before_any_spawn() {
    let Err(mut failure) = Client::start(NativeCommand::new("/does-not-exist"), expected()) else {
        panic!("implicit environment admitted")
    };
    assert_eq!(failure.error(), &ClientError::InvalidCommand);
    assert!(failure.is_settled());
    assert!(failure.poll_settle().unwrap());
}
#[test]
fn original_native_spawn_error_is_borrowed_until_settled() {
    let mut command = NativeCommand::new(if cfg!(windows) {
        "C:\\theseus-no-such-client-fixture.exe"
    } else {
        "/theseus-no-such-client-fixture"
    });
    command.environment = ChildEnvironment::Replace(vec![]);
    let Err(mut failure) = Client::start(command, expected()) else {
        panic!("missing executable spawned")
    };
    assert_eq!(failure.error(), &ClientError::Spawn);
    assert!(
        failure
            .failure()
            .unwrap()
            .source()
            .unwrap()
            .is::<theseus_host::process::SpawnFailure>()
    );
    failure.request_stop();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !failure.poll_settle().unwrap() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(
        failure
            .failure()
            .unwrap()
            .source()
            .unwrap()
            .is::<theseus_host::process::SpawnFailure>()
    );
}
