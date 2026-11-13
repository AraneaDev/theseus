//! RtlGetVersion platform version verification.
use super::*;

#[repr(C)]
pub(super) struct NativeVersionInfo {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    service_pack: [u16; 128],
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlGetVersion(version: *mut NativeVersionInfo) -> i32;
}
pub(super) fn parse_platform_version(expected: &str) -> io::Result<[u32; 3]> {
    if expected.len() > 35 {
        return Err(invalid(
            "SCM platform qualification requires canonical major.minor.build",
        ));
    }
    let parts: Vec<&str> = expected.split('.').collect();
    if parts.len() != 3 {
        return Err(invalid(
            "SCM platform qualification requires canonical major.minor.build",
        ));
    }
    let mut values = [0; 3];
    for (part, value) in parts.into_iter().zip(&mut values) {
        if part.is_empty()
            || (part.len() > 1 && part.starts_with('0'))
            || !part.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(invalid(
                "SCM platform qualification has a noncanonical version component",
            ));
        }
        *value = part
            .parse()
            .map_err(|_| invalid("SCM platform qualification component overflow"))?;
    }
    Ok(values)
}
/// Verify the actual local Windows version/build before any SCM connection.
/// A receipt string or process compatibility manifest is not a native version
/// measurement. RtlGetVersion returns an NTSTATUS, never a Win32 error domain.
pub fn verify_platform_version(expected: &str) -> io::Result<()> {
    let expected = parse_platform_version(expected)?;
    let mut version = NativeVersionInfo {
        size: size_of::<NativeVersionInfo>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        service_pack: [0; 128],
    };
    // SAFETY: initialized SDK-compatible RTL_OSVERSIONINFOW storage of exactly
    // the advertised size remains alive and exclusively borrowed for the call.
    let status = unsafe { RtlGetVersion(&mut version) };
    if status < 0 {
        return Err(io::Error::other(format!(
            "RtlGetVersion NTSTATUS 0x{:08x}",
            status as u32
        )));
    }
    if version.size != size_of::<NativeVersionInfo>() as u32
        || version.platform != 2
        || expected != [version.major, version.minor, version.build]
    {
        return Err(invalid(
            "actual Windows version/build differs from supplied SCM qualification",
        ));
    }
    Ok(())
}
