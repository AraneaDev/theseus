use std::{
    io,
    mem::{size_of, zeroed},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::null_mut,
};
use windows_sys::Win32::{
    Foundation::ERROR_NO_TOKEN,
    Security::*,
    Storage::FileSystem::FILE_ALL_ACCESS,
    System::{SystemServices::SECURITY_DESCRIPTOR_REVISION, Threading::*},
};

/// The descriptor points into `acl`'s stable allocation, never into this moving struct.
/// ACEs copy the SIDs, so token query storage need not outlive construction.
pub(crate) struct PrivateSecurity {
    descriptor: SECURITY_DESCRIPTOR,
    _acl: Vec<usize>,
}
impl PrivateSecurity {
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: All outputs use aligned owned storage; successful real token handles
        // are immediately owned. Pseudo current-thread/process handles are borrowed.
        unsafe {
            let mut raw = null_mut();
            if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut raw) == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
                    return Err(error);
                }
                checked(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw))?;
            }
            let token = OwnedHandle::from_raw_handle(raw);
            let mut bytes = 0;
            GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut bytes);
            if bytes < size_of::<TOKEN_USER>() as u32 {
                return Err(io::Error::last_os_error());
            }
            let mut user = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
            checked(GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                user.as_mut_ptr().cast(),
                bytes,
                &mut bytes,
            ))?;
            let user_sid = (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid;
            let mut system = [0usize; 9];
            let mut system_size = size_of_val(&system) as u32;
            checked(CreateWellKnownSid(
                WinLocalSystemSid,
                null_mut(),
                system.as_mut_ptr().cast(),
                &mut system_size,
            ))?;
            let system_sid = system.as_mut_ptr().cast();
            let sid_offset = std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
            let acl_bytes = size_of::<ACL>()
                + 2 * sid_offset
                + GetLengthSid(user_sid) as usize
                + GetLengthSid(system_sid) as usize;
            let acl_bytes = u16::try_from(acl_bytes)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "ACL too large"))?;
            let mut acl = vec![0usize; (acl_bytes as usize).div_ceil(size_of::<usize>())];
            let acl_ptr = acl.as_mut_ptr().cast::<ACL>();
            checked(InitializeAcl(acl_ptr, acl_bytes as u32, ACL_REVISION))?;
            checked(AddAccessAllowedAceEx(
                acl_ptr,
                ACL_REVISION,
                0,
                FILE_ALL_ACCESS,
                user_sid,
            ))?;
            checked(AddAccessAllowedAceEx(
                acl_ptr,
                ACL_REVISION,
                0,
                FILE_ALL_ACCESS,
                system_sid,
            ))?;
            let mut descriptor: SECURITY_DESCRIPTOR = zeroed();
            let descriptor_ptr = (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast();
            checked(InitializeSecurityDescriptor(
                descriptor_ptr,
                SECURITY_DESCRIPTOR_REVISION,
            ))?;
            checked(SetSecurityDescriptorDacl(descriptor_ptr, 1, acl_ptr, 0))?;
            checked(SetSecurityDescriptorControl(
                descriptor_ptr,
                SE_DACL_PROTECTED,
                SE_DACL_PROTECTED,
            ))?;
            Ok(Self {
                descriptor,
                _acl: acl,
            })
        }
    }
    pub(crate) fn as_ptr(&mut self) -> *const SECURITY_DESCRIPTOR {
        &self.descriptor
    }
}
fn checked(value: i32) -> io::Result<()> {
    if value == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
