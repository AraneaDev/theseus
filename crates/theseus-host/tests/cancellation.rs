use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use theseus_host::cancel::CancellationRegistration;

#[test]
fn registration_drop_releases_owned_flag_on_repeated_installation() {
    for _ in 0..32 {
        let flag = Arc::new(AtomicBool::new(false));
        let weak = Arc::downgrade(&flag);
        let registration = CancellationRegistration::install(flag.clone()).unwrap();
        assert!(!flag.load(Ordering::SeqCst));
        drop(flag);
        assert!(weak.upgrade().is_some());
        drop(registration);
        assert!(
            weak.upgrade().is_none(),
            "registration must release its callback state"
        );
    }
}

#[test]
fn actual_control_event_only_reaches_live_registrations_in_owned_fixture() {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_theseus-host-fixture"));
    command.arg("cancellation-event");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;
        command.creation_flags(CREATE_NEW_CONSOLE);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"32 owned control events: live=true dropped=false\n"
    );
}
