use std::{io, ops::Range};

fn invalid_data() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid token user information")
}

// Keep the effective-token policy independently testable without impersonation.
fn thread_token_or_process<T>(
    thread: io::Result<T>,
    process: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    #[cfg(windows)]
    use windows_sys::Win32::Foundation::ERROR_NO_TOKEN;
    #[cfg(not(windows))]
    const ERROR_NO_TOKEN: u32 = 1008; // WinError.h; test-only on non-Windows.
    match thread {
        Ok(token) => Ok(token),
        Err(error) if error.raw_os_error() == Some(ERROR_NO_TOKEN as i32) => process(),
        Err(error) => Err(error),
    }
}

// Validate integer addresses before deriving a slice from the owned allocation.
fn sid_range(base: usize, bytes: usize, pointer: usize, length: usize) -> io::Result<Range<usize>> {
    let end = base.checked_add(bytes).ok_or_else(invalid_data)?;
    let sid_end = pointer.checked_add(length).ok_or_else(invalid_data)?;
    if pointer < base || sid_end > end {
        return Err(invalid_data());
    }
    Ok(pointer - base..sid_end - base)
}

// SID's fixed header is 8 bytes and each subauthority is a little-endian DWORD.
// No native-allocated string is needed; maximum valid output is below 256 ASCII bytes.
fn sid_string(bytes: &[u8]) -> io::Result<String> {
    if bytes.len() < 8 || bytes[0] != 1 || bytes[1] > 15 {
        return Err(invalid_data());
    }
    let length = 8 + usize::from(bytes[1]) * 4;
    if bytes.len() != length {
        return Err(invalid_data());
    }
    let authority = bytes[2..8]
        .iter()
        .fold(0u64, |value, &byte| (value << 8) | u64::from(byte));
    let mut output = if authority < (1u64 << 32) {
        format!("S-1-{authority}")
    } else {
        format!("S-1-0x{authority:012x}")
    };
    for subauthority in bytes[8..].as_chunks::<4>().0 {
        let value = u32::from_le_bytes(*subauthority);
        use std::fmt::Write;
        write!(output, "-{value}").map_err(|_| invalid_data())?;
    }
    if output.len() > 256 || !output.is_ascii() {
        return Err(invalid_data());
    }
    Ok(output)
}

#[cfg(windows)]
pub(super) mod native {
    use super::*;
    use crate::principal::CurrentPrincipal;
    use std::{
        mem::size_of,
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        ptr::null_mut,
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE},
        Security::{GetTokenInformation, IsValidSid, TOKEN_QUERY, TOKEN_USER, TokenUser},
        System::Threading::{
            GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken,
        },
    };

    pub(crate) fn current_principal() -> io::Result<CurrentPrincipal> {
        // SAFETY: Pseudo handles are borrowed. Each successful native output
        // is immediately owned, noninheritable, and closed on every return path.
        let token = thread_token_or_process(
            open_token(|output| unsafe {
                OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, output)
            }),
            || {
                open_token(|output| unsafe {
                    OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, output)
                })
            },
        )?;
        Ok(CurrentPrincipal::Windows {
            token_user_sid: token_user(token.as_raw_handle())?,
        })
    }

    fn open_token(call: impl FnOnce(*mut HANDLE) -> i32) -> io::Result<OwnedHandle> {
        let mut raw = null_mut();
        if call(&mut raw) == 0 {
            return Err(io::Error::last_os_error());
        }
        if raw.is_null() {
            return Err(invalid_data());
        }
        // SAFETY: This helper only receives successful token-opening calls.
        Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
    }

    fn token_user(token: HANDLE) -> io::Result<String> {
        // Fixed, aligned, owned storage caps native size requests without allocation.
        let mut storage = [0usize; 4096 / size_of::<usize>()];
        let mut required = 0u32;
        // SAFETY: Size probe has a null buffer and zero capacity, as documented.
        let status = unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut required) };
        if status != 0 {
            return Err(invalid_data());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
            return Err(error);
        }
        let capacity = required as usize;
        if capacity < size_of::<TOKEN_USER>() || capacity > size_of_val(&storage) {
            return Err(invalid_data());
        }
        // SAFETY: Storage is aligned for TOKEN_USER, with at least required bytes.
        let status = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                storage.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        };
        if status == 0 {
            return Err(io::Error::last_os_error());
        }
        let used = required as usize;
        if used < size_of::<TOKEN_USER>() || used > capacity {
            return Err(invalid_data());
        }
        // SAFETY: Successful query wrote a complete aligned TOKEN_USER header.
        let sid = unsafe { (*storage.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        let header = sid_range(storage.as_ptr() as usize, used, sid as usize, 8)?;
        if header.start < size_of::<TOKEN_USER>() || !(sid as usize).is_multiple_of(4) {
            return Err(invalid_data());
        }
        // SAFETY: This slice covers only the successful query's initialized bytes.
        let bytes = unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), used) };
        let count = bytes[header.start + 1];
        if count > 15 {
            return Err(invalid_data());
        }
        let range = sid_range(
            storage.as_ptr() as usize,
            used,
            sid as usize,
            8 + usize::from(count) * 4,
        )?;
        // SAFETY: The complete SID, including every subauthority, is in owned
        // aligned query storage. Native validation cannot inspect beyond it.
        if unsafe { IsValidSid(sid) } == 0 {
            return Err(invalid_data());
        }
        sid_string(&bytes[range])
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn native_query_failure_preserves_underlying_error() {
            // A real process handle is not a token handle; querying it must fail.
            let error = token_user(unsafe { GetCurrentProcess() }).unwrap_err();
            assert_eq!(error.raw_os_error(), Some(6)); // ERROR_INVALID_HANDLE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_absent_thread_token_allows_process_fallback() {
        assert_eq!(
            thread_token_or_process(Ok(7), || panic!("unexpected fallback")).unwrap(),
            7
        );
        assert_eq!(
            thread_token_or_process::<u8>(Err(io::Error::from_raw_os_error(1008)), || Ok(9))
                .unwrap(),
            9
        );
        for code in [5, 6, 1346, 9999] {
            let error =
                thread_token_or_process::<u8>(Err(io::Error::from_raw_os_error(code)), || {
                    panic!("forbidden fallback")
                })
                .unwrap_err();
            assert_eq!(error.raw_os_error(), Some(code));
        }
        let error = thread_token_or_process::<u8>(Err(io::Error::from_raw_os_error(1008)), || {
            Err(io::Error::from_raw_os_error(5))
        })
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(5));
    }
    #[test]
    fn sid_serialization_has_native_numeric_order_and_bounded_output() {
        assert_eq!(
            sid_string(&[1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 32, 2, 0, 0]).unwrap(),
            "S-1-5-32-544"
        );
        assert_eq!(
            sid_string(&[1, 1, 1, 2, 3, 4, 5, 6, 255, 255, 255, 255]).unwrap(),
            "S-1-0x010203040506-4294967295"
        );
        let mut largest = vec![255; 68];
        largest[0] = 1;
        largest[1] = 15;
        let value = sid_string(&largest).unwrap();
        assert!(value.is_ascii() && value.len() <= 256);
    }
    #[test]
    fn malformed_sid_cannot_be_truncated_into_an_identity() {
        for value in [
            &[][..],
            &[1][..],
            &[2, 0, 0, 0, 0, 0, 0, 5][..],
            &[1, 16, 0, 0, 0, 0, 0, 5][..],
            &[1, 1, 0, 0, 0, 0, 0, 5][..],
        ] {
            assert_eq!(
                sid_string(value).unwrap_err().kind(),
                std::io::ErrorKind::InvalidData
            );
        }
    }
    #[test]
    fn token_pointer_must_contain_complete_sid_in_returned_storage() {
        assert_eq!(sid_range(1000, 32, 1016, 12).unwrap(), 16..28);
        for (pointer, size) in [
            (0, 8),
            (999, 8),
            (1028, 8),
            (usize::MAX, 8),
            (1016, usize::MAX),
        ] {
            assert!(sid_range(1000, 32, pointer, size).is_err());
        }
        assert!(sid_range(usize::MAX - 3, 16, usize::MAX - 2, 8).is_err());
    }
}
