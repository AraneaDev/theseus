//! Read-only services.exe DeleteFlag registry probe.
use super::*;

/// Read-only DeleteFlag of exactly this validated service name. DeleteService
/// makes services.exe set REG_DWORD DeleteFlag=1 on the record it keeps while
/// handles remain open. Opens with KEY_QUERY_VALUE only; never creates/writes.
pub(super) fn delete_flag_observation(service_name: &str) -> NameObservation {
    // The authority already admits only [A-Za-z0-9._-], max 128, no leading
    // '-'. Refuse anything that could leave the exact Services\<name> key.
    if service_name.is_empty()
        || service_name.len() > 128
        || matches!(service_name, "." | "..")
        || !service_name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        return NameObservation::Unknown(0);
    }
    let path = wide_name(&format!(
        "SYSTEM\\CurrentControlSet\\Services\\{service_name}"
    ));
    let mut key: Handle = ptr::null_mut();
    // SAFETY: nul-terminated owned path; out-pointer is a local.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE as Handle,
            path.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        )
    };
    if opened != 0 {
        return NameObservation::Unknown(opened as u32);
    }
    let value = wide_name("DeleteFlag");
    let (mut kind, mut data, mut size) = (0u32, 0u32, size_of::<u32>() as u32);
    // SAFETY: open key, nul-terminated name, 4-byte local buffer and size.
    let queried = unsafe {
        RegQueryValueExW(
            key,
            value.as_ptr(),
            ptr::null_mut(),
            &mut kind,
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    // SAFETY: closes exactly the key opened above, on every path.
    let closed = unsafe { RegCloseKey(key) };
    if closed != 0 {
        return NameObservation::Unknown(closed as u32);
    }
    match queried {
        0 if kind == REG_DWORD && size == 4 && data == 1 => NameObservation::MarkedForDeletion,
        0 => NameObservation::Unknown(0),
        ERROR_FILE_NOT_FOUND => NameObservation::Present,
        code => NameObservation::Unknown(code as u32),
    }
}
