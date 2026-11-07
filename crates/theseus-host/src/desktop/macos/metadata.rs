//! Direct CoreFoundation immutable parsing: no AppKit/session interaction here.
use super::{BundleMetadata, RetainedBundle, invalid};
use std::{
    ffi::{c_char, c_void},
    io, ptr,
};
type Cf = *const c_void;
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataCreate(allocator: Cf, bytes: *const u8, length: isize) -> Cf;
    fn CFPropertyListCreateWithData(
        allocator: Cf,
        data: Cf,
        options: usize,
        format: *mut isize,
        error: *mut Cf,
    ) -> Cf;
    fn CFRelease(value: Cf);
    fn CFGetTypeID(value: Cf) -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFStringGetLength(string: Cf) -> isize;
    fn CFDictionaryGetCount(dictionary: Cf) -> isize;
    fn CFDictionaryGetKeysAndValues(dictionary: Cf, keys: *mut Cf, values: *mut Cf);
    fn CFStringGetCString(string: Cf, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
}
struct OwnedCf(Cf);
impl Drop for OwnedCf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}
fn string(value: Cf) -> io::Result<String> {
    if value.is_null() || unsafe { CFGetTypeID(value) != CFStringGetTypeID() } {
        return Err(invalid("bundle metadata field must be a native string"));
    }
    let mut buffer = [0u8; 1025];
    if unsafe {
        CFStringGetCString(
            value,
            buffer.as_mut_ptr().cast(),
            buffer.len() as isize,
            0x08000100,
        )
    } == 0
    {
        return Err(invalid("native bundle metadata string exceeds bound"));
    }
    let length = buffer
        .iter()
        .position(|v| *v == 0)
        .ok_or_else(|| invalid("unterminated native string"))?;
    let decoded = String::from_utf8(buffer[..length].to_vec())
        .map_err(|_| invalid("native bundle string is not UTF-8"))?;
    if decoded.encode_utf16().count() as isize != unsafe { CFStringGetLength(value) } {
        return Err(invalid("native bundle string contains embedded NUL"));
    }
    Ok(decoded)
}
/// Parse only bounded XML/binary property dictionaries with a closed string-key
/// profile. No property-list-supplied environment, URL handlers or launch modes.
pub fn native_metadata(bytes: &[u8]) -> io::Result<BundleMetadata> {
    if bytes.is_empty() || bytes.len() > 65536 {
        return Err(invalid("Info.plist exceeds metadata bound"));
    }
    let data = unsafe { CFDataCreate(ptr::null(), bytes.as_ptr(), bytes.len() as isize) };
    if data.is_null() {
        return Err(io::Error::other("CFDataCreate failed"));
    }
    let data = OwnedCf(data);
    let mut format = 0;
    let mut error = ptr::null();
    let plist =
        unsafe { CFPropertyListCreateWithData(ptr::null(), data.0, 0, &mut format, &mut error) };
    let _error = (!error.is_null()).then(|| OwnedCf(error));
    if plist.is_null() {
        return Err(invalid("native property-list parser refused Info.plist"));
    }
    let plist = OwnedCf(plist);
    if !matches!(format, 100 | 200) || unsafe { CFGetTypeID(plist.0) != CFDictionaryGetTypeID() } {
        return Err(invalid(
            "Info.plist must be an XML/binary native dictionary",
        ));
    }
    let count = unsafe { CFDictionaryGetCount(plist.0) };
    if !(3..=16).contains(&count) {
        return Err(invalid("bundle metadata field count outside profile"));
    }
    let mut keys = [ptr::null(); 16];
    let mut values = [ptr::null(); 16];
    unsafe { CFDictionaryGetKeysAndValues(plist.0, keys.as_mut_ptr(), values.as_mut_ptr()) };
    let mut metadata = BundleMetadata {
        identifier: String::new(),
        executable: String::new(),
        package_type: String::new(),
        extra_keys: Vec::new(),
    };
    for index in 0..count as usize {
        let key = string(keys[index])?;
        let value = string(values[index])?;
        match key.as_str() {
            "CFBundleIdentifier" => metadata.identifier = value,
            "CFBundleExecutable" => metadata.executable = value,
            "CFBundlePackageType" => metadata.package_type = value,
            "CFBundleName"
            | "CFBundleVersion"
            | "CFBundleShortVersionString"
            | "CFBundleInfoDictionaryVersion"
            | "LSMinimumSystemVersion" => {}
            _ => metadata.extra_keys.push(key),
        }
    }
    if !metadata.extra_keys.is_empty() {
        return Err(invalid(
            "Info.plist contains unsupported native launch policy",
        ));
    }
    Ok(metadata)
}
/// Opaque native-admitted exact bundle. Raw report bytes/model metadata cannot
/// construct this value; the original root/file owners remain held inside it.
pub struct AdmittedBundle {
    pub(super) retained: RetainedBundle,
}
impl AdmittedBundle {
    pub fn admit(retained: RetainedBundle) -> io::Result<Self> {
        let metadata = native_metadata(&retained.info_bytes()?)?;
        retained.profile().admit_metadata(&metadata)?;
        retained.revalidate()?;
        Ok(Self { retained })
    }
    pub fn profile(&self) -> &super::BundleProfile {
        self.retained.profile()
    }
    pub fn revalidate(&self) -> io::Result<()> {
        self.retained.revalidate()
    }
}
