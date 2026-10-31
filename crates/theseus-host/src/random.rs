//! Operating-system randomness for owned identifiers; no predictable fallback.
use std::io;
pub fn random_bytes(out: &mut [u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use io::Read;
        std::fs::File::open("/dev/urandom")?.read_exact(out)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Security::Cryptography::{
            BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
        };
        // The published API takes ULONG bytes. Chunking avoids truncating a usize.
        for chunk in out.chunks_mut(u32::MAX as usize) {
            let status = unsafe {
                BCryptGenRandom(
                    std::ptr::null_mut(),
                    chunk.as_mut_ptr(),
                    chunk.len() as u32,
                    BCRYPT_USE_SYSTEM_PREFERRED_RNG,
                )
            };
            if status < 0 {
                return Err(io::Error::from_raw_os_error(unsafe {
                    windows_sys::Win32::Foundation::RtlNtStatusToDosError(status)
                } as i32));
            }
        }
        Ok(())
    }
}
