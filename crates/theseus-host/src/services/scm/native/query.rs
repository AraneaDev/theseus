//! Retained-handle configuration and status queries.
use super::*;

pub(super) fn original_raw(o: &OriginalRegistration) -> io::Result<Handle> {
    o.handle
        .borrow()
        .as_ref()
        .map(|h| h.raw)
        .ok_or_else(|| invalid("original SCM handle already closed; reopen forbidden"))
}
pub(super) fn wide_name(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
pub(super) fn receipt(s: &str) -> io::Result<()> {
    if s.is_empty() || s.len() > 4096 || s.chars().any(char::is_control) {
        Err(invalid("missing original SUT receipt"))
    } else {
        Ok(())
    }
}
pub(super) fn preflight(deadline: Instant, cancel: &AtomicBool) -> io::Result<()> {
    let now = Instant::now();
    if cancel.load(Ordering::Relaxed) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "SCM operation cancelled",
        ));
    }
    if now >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "SCM operation deadline",
        ));
    }
    if deadline.duration_since(now) > std::time::Duration::from_secs(30) {
        return Err(invalid("SCM deadline exceeds 30 seconds"));
    }
    Ok(())
}
pub(super) fn query_status(raw: Handle, errors: &Errors) -> io::Result<ServiceStatus> {
    let mut output = ProcessStatus::default();
    let mut needed = 0;
    let ok = unsafe {
        QueryServiceStatusEx(
            raw,
            0,
            (&mut output as *mut ProcessStatus).cast(),
            size_of::<ProcessStatus>() as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        let error = last("QueryServiceStatusEx");
        return Err(keep_error(errors, error));
    }
    Ok(ServiceStatus {
        service_type: output.service_type,
        state: output.state,
        controls_accepted: output.controls,
        win32_exit_code: output.exit,
        service_specific_exit_code: output.specific_exit,
        checkpoint: output.checkpoint,
        wait_hint_ms: output.wait_hint,
        process_id: output.pid,
        service_flags: output.flags,
    })
}
fn config_buffer(raw: Handle, level: Option<u32>, errors: &Errors) -> io::Result<NativeBuffer> {
    // Windows documents a maximum 8KiB service configuration output. Allocate a
    // fixed bounded aligned block: no probe/reallocate race or hostile length.
    let mut output = NativeBuffer::new(8192)?;
    let mut needed = 0;
    let ok = match level {
        None => unsafe { QueryServiceConfigW(raw, output.ptr().cast(), output.len(), &mut needed) },
        Some(level) => unsafe {
            QueryServiceConfig2W(raw, level, output.ptr(), output.len(), &mut needed)
        },
    };
    if ok == 0 {
        let error = last(if level.is_none() {
            "QueryServiceConfigW"
        } else {
            "QueryServiceConfig2W"
        });
        if error.code == INSUFFICIENT_BUFFER && needed as usize > 8192 {
            errors.borrow_mut().push(error);
            return Err(invalid("SCM output exceeds native 8KiB bound"));
        }
        return Err(keep_error(errors, error));
    }
    Ok(output)
}
pub(super) fn query_definition(
    raw: Handle,
    errors: &Errors,
    deadline: Instant,
    cancel: &AtomicBool,
) -> io::Result<ScmDefinition> {
    preflight(deadline, cancel)?;
    let b = config_buffer(raw, None, errors)?;
    let q = b.value::<QueryConfig>()?;
    let mut d = ScmDefinition {
        command_line: b.wide(q.binary)?,
        display_name: b.wide(q.display)?,
        account: b.wide(q.account)?,
        service_type: q.service_type,
        start_type: q.start_type,
        error_control: q.error_control,
        load_order_group: b.wide(q.group)?,
        dependencies: b.multi(q.dependencies)?,
        tag_id: q.tag,
        delayed_auto_start: false,
        failure_actions: vec![],
        failure_reset_period: 0,
        failure_reboot_message: vec![],
        failure_command: vec![],
        failure_actions_on_non_crash: false,
        service_sid_type: 0,
        required_privileges: vec![],
        triggers_present: false,
        launch_protected: 0,
    };
    for level in [2, 3, 4, 5, 6, 8, 12] {
        preflight(deadline, cancel)?;
        let b = config_buffer(raw, Some(level), errors)?;
        match level {
            2 => {
                let f = b.value::<FailureActions>()?;
                d.failure_reset_period = f.reset;
                d.failure_reboot_message = b.wide(f.reboot)?;
                d.failure_command = b.wide(f.command)?;
                d.failure_actions = b
                    .array(f.actions, f.count as usize)?
                    .into_iter()
                    .map(|a| FailureAction {
                        kind: a.kind,
                        delay_ms: a.delay,
                    })
                    .collect();
            }
            3 => d.delayed_auto_start = b.value::<i32>()? != 0,
            4 => d.failure_actions_on_non_crash = b.value::<i32>()? != 0,
            5 => d.service_sid_type = b.value::<u32>()?,
            6 => {
                let p = b.value::<WidePointer>()?;
                d.required_privileges = b.multi(p.value)?;
            }
            8 => d.triggers_present = b.value::<TriggerInfo>()?.count != 0,
            12 => d.launch_protected = b.value::<u32>()?,
            _ => unreachable!(),
        }
    }
    Ok(d)
}
pub(super) fn query_observation(
    raw: Handle,
    errors: &Errors,
    deadline: Instant,
    cancel: &AtomicBool,
) -> io::Result<ServiceObservation> {
    let config = match query_definition(raw, errors, deadline, cancel) {
        Ok(c) => c,
        Err(e) if e.raw_os_error() == Some(MARKED_FOR_DELETE as i32) => {
            let mut observation = ServiceObservation::absent(RegistrationState::DeletePending);
            observation.status = Some(query_status(raw, errors)?);
            return Ok(observation);
        }
        Err(e) => return Err(e),
    };
    preflight(deadline, cancel)?;
    let status = query_status(raw, errors)?;
    // Separate native reads are not an atomic snapshot. Refuse a changing config
    // across the status observation, preserving the original handle on failure.
    if config != query_definition(raw, errors, deadline, cancel)? {
        return Err(invalid(
            "SCM configuration changed during bounded observation",
        ));
    }
    preflight(deadline, cancel)?;
    Ok(ServiceObservation::present(config, status))
}
