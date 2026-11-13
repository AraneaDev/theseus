use super::*;
#[test]
fn delete_flag_reads_only_the_exact_validated_service_key() {
    for name in ["", ".", "..", "a\\b", "a/b", "..\\Tcpip", "x\0"] {
        assert_eq!(delete_flag_observation(name), NameObservation::Unknown(0));
    }
    // A real absent key is an error, never Present or MarkedForDeletion.
    assert_eq!(
        delete_flag_observation("TheseusAbsentDeleteFlagProbe_7f3a"),
        NameObservation::Unknown(2)
    );
}
#[test]
fn released_runtime_pin_admits_exactly_one_new_pid_then_detects_changes() {
    let status = |state, process_id| ServiceStatus {
        service_type: 0x10,
        state,
        controls_accepted: 1,
        win32_exit_code: 0,
        service_specific_exit_code: 0,
        checkpoint: 0,
        wait_hint_ms: 0,
        process_id,
        service_flags: 0,
    };
    let pin = Cell::new(RuntimePin::Open);
    assert!(!pinned_pid_changed(&pin, status(4, 8656)));
    // A restart that is not released (no admitted transition) is a change.
    assert!(pinned_pid_changed(&pin, status(4, 9001)));
    // Stopped carries no live PID and never pins or changes.
    assert!(!pinned_pid_changed(&pin, status(1, 0)));
    // Admitted SUT transition releases the pin: the new PID is pinned once,
    assert_eq!(pin.replace(RuntimePin::Open), RuntimePin::Pinned(8656));
    assert!(!pinned_pid_changed(&pin, status(4, 9001)));
    // a running admission keeps that pin,
    close_stopped_interval(&pin, Some(status(4, 9001)));
    assert_eq!(pin.get(), RuntimePin::Pinned(9001));
    // and any later unadmitted restart is still detected.
    assert!(pinned_pid_changed(&pin, status(4, 9100)));
}
#[test]
fn admitted_transition_left_stopped_flags_any_later_foreign_start() {
    let status = |state, process_id| ServiceStatus {
        service_type: 0x10,
        state,
        controls_accepted: 1,
        win32_exit_code: 0,
        service_specific_exit_code: 0,
        checkpoint: 0,
        wait_hint_ms: 0,
        process_id,
        service_flags: 0,
    };
    for stopped in [1, 3] {
        let pin = Cell::new(RuntimePin::Open);
        assert!(!pinned_pid_changed(&pin, status(stopped, 0)));
        close_stopped_interval(&pin, Some(status(stopped, 0)));
        assert_eq!(pin.get(), RuntimePin::Closed);
        assert!(!pinned_pid_changed(&pin, status(1, 0)));
        // A start the helper did not perform is a foreign runtime change.
        assert!(pinned_pid_changed(&pin, status(4, 7000)));
        // Only the helper's own Start reopens the interval.
        pin.set(RuntimePin::Open);
        assert!(!pinned_pid_changed(&pin, status(4, 7000)));
        assert_eq!(pin.get(), RuntimePin::Pinned(7000));
    }
    // A transition still starting the service keeps the interval open.
    let pin = Cell::new(RuntimePin::Open);
    close_stopped_interval(&pin, Some(status(2, 0)));
    assert_eq!(pin.get(), RuntimePin::Open);
    assert!(!pinned_pid_changed(&pin, status(4, 7100)));
    // An unavailable status never closes it either.
    let pin = Cell::new(RuntimePin::Open);
    close_stopped_interval(&pin, None);
    assert_eq!(pin.get(), RuntimePin::Open);
}
#[test]
fn qualified_registration_starts_pinned_closed_or_open_by_runtime_state() {
    let status = |state, process_id| ServiceStatus {
        service_type: 0x10,
        state,
        controls_accepted: 1,
        win32_exit_code: 0,
        service_specific_exit_code: 0,
        checkpoint: 0,
        wait_hint_ms: 0,
        process_id,
        service_flags: 0,
    };
    assert_eq!(
        initial_runtime_pin(Some(status(4, 5000))).get(),
        RuntimePin::Pinned(5000)
    );
    // A stopped qualified service flags a start the helper did not perform.
    let pin = initial_runtime_pin(Some(status(1, 0)));
    assert_eq!(pin.get(), RuntimePin::Closed);
    assert!(pinned_pid_changed(&pin, status(4, 5100)));
    assert_eq!(
        initial_runtime_pin(Some(status(3, 0))).get(),
        RuntimePin::Closed
    );
    // A start already pending at qualification pins its own first PID.
    let pin = initial_runtime_pin(Some(status(2, 0)));
    assert_eq!(pin.get(), RuntimePin::Open);
    assert!(!pinned_pid_changed(&pin, status(4, 5200)));
    assert_eq!(initial_runtime_pin(None).get(), RuntimePin::Open);
}
#[test]
fn platform_version_parser_is_canonical_and_never_selects_a_default() {
    assert_eq!(
        parse_platform_version("10.0.26100").unwrap(),
        [10, 0, 26100]
    );
    for invalid in [
        "",
        "10",
        "10.0",
        "10.0.26100.extra",
        "10.00.26100",
        "10.0.-1",
        "10.0.4294967296",
        " 10.0.26100",
    ] {
        assert!(parse_platform_version(invalid).is_err());
    }
    assert_eq!(size_of::<NativeVersionInfo>(), 276);
}
