use std::time::{Duration, Instant};
use theseus_host::process::{InputMode, NativeCommand, OwnedChild};

fn heartbeat_advances(
    path: &std::path::Path,
    before: u64,
    deadline: Instant,
) -> std::io::Result<bool> {
    while Instant::now() < deadline {
        if std::fs::metadata(path)?.len() > before && Instant::now() < deadline {
            return Ok(true);
        }
        std::thread::sleep(
            Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    Ok(false)
}

#[test]
fn retained_heartbeat_oracle_requires_live_growth_within_original_deadline() -> std::io::Result<()>
{
    let temp = tempfile::tempdir()?;
    let ready = temp.path().join("ready");
    let heartbeat = temp.path().join("heartbeat");
    let mut command = NativeCommand::new(env!("CARGO_BIN_EXE_theseus-host-fixture"));
    command
        .arg("ready-hold")
        .arg(&ready)
        .arg(&heartbeat)
        .arg("30000");
    let mut acquired = OwnedChild::spawn(&command, InputMode::Pipe).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let growth_deadline = deadline - Duration::from_millis(500);
    while !ready.exists() || !heartbeat.exists() {
        assert!(Instant::now() < growth_deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let before = std::fs::metadata(&heartbeat)?.len();
    let live = heartbeat_advances(&heartbeat, before, growth_deadline)?;
    let expired = heartbeat_advances(&heartbeat, before, Instant::now())?;
    acquired.child.terminate_owned()?;
    while !acquired.child.exited_reserved()? {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    // Observe a new baseline only after native exit, while the child is still owned.
    let stopped_deadline =
        (Instant::now() + Duration::from_millis(80)).min(deadline - Duration::from_millis(200));
    assert!(
        Instant::now() < stopped_deadline,
        "stopped control needs an unexpired observation window"
    );
    let stopped = heartbeat_advances(
        &heartbeat,
        std::fs::metadata(&heartbeat)?.len(),
        stopped_deadline,
    )?;
    while acquired.child.try_reap_and_release()?.is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    acquired.stdout.cancel_and_complete()?;
    acquired.stderr.cancel_and_complete()?;
    acquired.stdin.as_mut().unwrap().close()?;
    assert!(Instant::now() < deadline);
    assert!(live, "oracle must observe actual live native growth");
    assert!(
        !expired,
        "an expired observation deadline must not be restarted"
    );
    assert!(
        !stopped,
        "an exited but retained native child cannot count as live"
    );
    Ok(())
}

#[test]
fn borrowed_reap_keeps_owner_until_confirmed_exit_and_replays_status() {
    let mut command = NativeCommand::new(env!("CARGO_BIN_EXE_theseus-host-fixture"));
    command.args(["hold-pipes", "30000"]);
    let acquired = OwnedChild::spawn(&command, InputMode::Null).unwrap();
    let mut child = acquired.child;
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.try_reap_and_release().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(child.try_reap_and_release().unwrap(), Some(status));
    assert!(child.into_cleanup().try_complete().unwrap());
}

#[test]
fn transferred_live_child_guard_retains_authority_until_native_cleanup_finishes() {
    let temp = tempfile::tempdir().unwrap();
    let ready = temp.path().join("ready");
    let heartbeat = temp.path().join("heartbeat");
    let mut command = NativeCommand::new(env!("CARGO_BIN_EXE_theseus-host-fixture"));
    command
        .arg("ready-hold")
        .arg(&ready)
        .arg(&heartbeat)
        .arg("30000");
    let mut acquired = OwnedChild::spawn(&command, InputMode::Pipe).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    // Readiness and actual growth share the original deadline, with 500ms left
    // for native cleanup and the unchanged post-cleanup quiet window.
    let growth_deadline = deadline - Duration::from_millis(500);
    while !ready.exists() || !heartbeat.exists() {
        assert!(Instant::now() < growth_deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let before = std::fs::metadata(&heartbeat).unwrap().len();
    assert!(heartbeat_advances(&heartbeat, before, growth_deadline).unwrap());
    let mut owner = acquired.child.into_cleanup();
    while !owner.try_complete().unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    acquired.stdout.cancel_and_complete().unwrap();
    acquired.stderr.cancel_and_complete().unwrap();
    acquired.stdin.as_mut().unwrap().close().unwrap();
    let after = std::fs::metadata(&heartbeat).unwrap().len();
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(std::fs::metadata(&heartbeat).unwrap().len(), after);
    assert!(owner.try_complete().unwrap());
    assert!(Instant::now() < deadline);
}

#[test]
fn confirmed_native_exit_code_is_not_confused_with_windows_still_active() {
    let mut command = NativeCommand::new(env!("CARGO_BIN_EXE_theseus-host-fixture"));
    command.args(["exit", "259"]);
    let acquired = OwnedChild::spawn(&command, InputMode::Null).unwrap();
    let mut child = acquired.child;
    let deadline = Instant::now() + Duration::from_secs(3);
    while !child.exited_reserved().unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let status = child.try_reap_and_release().unwrap().unwrap();
    assert_eq!(status.code(), Some(if cfg!(windows) { 259 } else { 3 }));
    assert_eq!(child.try_reap_and_release().unwrap(), Some(status));
}
