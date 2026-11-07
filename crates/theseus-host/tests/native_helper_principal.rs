use theseus_host::principal::{CurrentPrincipal, current_principal};

// Catches real-versus-effective identity confusion and fabricated/default IDs.
#[cfg(unix)]
#[test]
fn current_principal_matches_native_effective_ids() {
    let principal = current_principal().expect("query current principal");
    // SAFETY: These native getters take no pointers and have no failure mode.
    let expected = unsafe {
        CurrentPrincipal::Posix {
            effective_uid: libc::geteuid(),
            effective_gid: libc::getegid(),
        }
    };
    assert_eq!(principal, expected);
}

// Independent native observation uses Windows' own SID string conversion.
#[cfg(windows)]
#[test]
fn current_principal_matches_native_token_sid() {
    use std::{
        mem::size_of,
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        ptr::null_mut,
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_NO_TOKEN, LocalFree},
        Security::{Authorization::ConvertSidToStringSidW, *},
        System::Threading::*,
    };
    struct NativeString(*mut u16);
    impl Drop for NativeString {
        fn drop(&mut self) {
            // SAFETY: ConvertSidToStringSidW transfers a LocalFree allocation.
            unsafe {
                LocalFree(self.0.cast());
            }
        }
    }
    unsafe {
        let mut raw = null_mut();
        if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut raw) == 0 {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(ERROR_NO_TOKEN as i32)
            );
            assert_ne!(
                OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw),
                0
            );
        }
        let token = OwnedHandle::from_raw_handle(raw);
        let mut bytes = 0;
        assert_eq!(
            GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut bytes),
            0
        );
        assert!(bytes as usize >= size_of::<TOKEN_USER>());
        assert!(bytes <= 4096);
        let mut storage = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
        assert_ne!(
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                storage.as_mut_ptr().cast(),
                bytes,
                &mut bytes
            ),
            0
        );
        let sid = (*storage.as_ptr().cast::<TOKEN_USER>()).User.Sid;
        let mut text = null_mut();
        assert_ne!(ConvertSidToStringSidW(sid, &mut text), 0);
        let text_owner = NativeString(text);
        // Native API owns a terminated SID string; bound the observation too.
        let length = (0..=256)
            .find(|&index| *text.add(index) == 0)
            .expect("bounded SID");
        let expected = String::from_utf16(std::slice::from_raw_parts(text, length));
        drop(text_owner);
        let expected = expected.unwrap();
        assert_eq!(
            current_principal().unwrap(),
            CurrentPrincipal::Windows {
                token_user_sid: expected
            }
        );
    }
}
