//! Fixture-only exact SYSTEM/helper DACL contract. Never repairs foreign objects.
#![allow(dead_code)]
use anyhow::{Result, ensure};
#[derive(Clone, Copy)]
pub enum Kind {
    Directory,
    Executable,
    Data,
    Sidecar,
    Pipe,
}
impl Kind {
    fn masks(self) -> (u32, u32) {
        match self {
            Self::Directory => (0x1200a9, 0x1301bf),
            Self::Executable => (0x1200a9, 0x1301a9),
            Self::Data => (0x120089, 0x1301bf),
            Self::Sidecar => (0x120089, 0x130189),
            Self::Pipe => (0x12019f, 0x12019f),
        }
    }
}
pub fn validate_grants(
    helper: &str,
    kind: Kind,
    protected: bool,
    grants: &[(&str, u32, u8)],
) -> Result<()> {
    ensure!(
        protected && helper != "S-1-5-18" && grants.len() == 2,
        "protected exact two-principal fixture DACL required"
    );
    let (system, user) = kind.masks();
    ensure!(
        grants
            .iter()
            .filter(|g| **g == ("S-1-5-18", system, 0))
            .count()
            == 1
            && grants.iter().filter(|g| **g == (helper, user, 0)).count() == 1,
        "fixture DACL contains inherited, extra, missing or excessive rights"
    );
    Ok(())
}
#[cfg(windows)]
pub use native::{initialize_created_path, probe_install_paths, verify_path};
#[cfg(windows)]
mod native {
    use super::*;
    use std::{
        ffi::c_void,
        fs::{File, OpenOptions},
        io,
        os::windows::{
            fs::{MetadataExt, OpenOptionsExt},
            io::{AsRawHandle, FromRawHandle},
        },
        path::Path,
        ptr,
    };
    use theseus_host::fs::{HostDir, LeafOpenResult, RootPolicy, fingerprint};
    type Handle = *mut c_void;
    #[repr(C)]
    struct Acl {
        revision: u8,
        reserved: u8,
        size: u16,
        count: u16,
        reserved2: u16,
    }
    #[repr(C)]
    struct SecurityAttributes {
        length: u32,
        descriptor: *mut c_void,
        inherit: i32,
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn GetSecurityInfo(
            handle: Handle,
            kind: u32,
            information: u32,
            owner: *mut *mut c_void,
            group: *mut *mut c_void,
            dacl: *mut *mut Acl,
            sacl: *mut *mut c_void,
            descriptor: *mut *mut c_void,
        ) -> u32;
        fn GetSecurityDescriptorControl(
            descriptor: *const c_void,
            control: *mut u16,
            revision: *mut u32,
        ) -> i32;
        fn GetAce(acl: *const Acl, index: u32, ace: *mut *mut c_void) -> i32;
        fn ConvertSidToStringSidW(sid: *const c_void, text: *mut *mut u16) -> i32;
        fn IsValidSid(sid: *const c_void) -> i32;
        fn GetLengthSid(sid: *const c_void) -> u32;
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text: *const u16,
            revision: u32,
            descriptor: *mut *mut c_void,
            length: *mut u32,
        ) -> i32;
        fn GetSecurityDescriptorDacl(
            descriptor: *const c_void,
            present: *mut i32,
            dacl: *mut *mut Acl,
            defaulted: *mut i32,
        ) -> i32;
        fn SetSecurityInfo(
            handle: Handle,
            kind: u32,
            information: u32,
            owner: *const c_void,
            group: *const c_void,
            dacl: *const Acl,
            sacl: *const c_void,
        ) -> u32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandleEx(
            handle: Handle,
            class: u32,
            information: *mut c_void,
            size: u32,
        ) -> i32;
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
        fn CreateNamedPipeW(
            name: *const u16,
            access: u32,
            mode: u32,
            instances: u32,
            output: u32,
            input: u32,
            timeout: u32,
            security: *const SecurityAttributes,
        ) -> Handle;
    }
    struct Allocation(*mut c_void);
    impl Drop for Allocation {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }
    fn check(value: i32) -> Result<()> {
        if value == 0 {
            Err(io::Error::last_os_error().into())
        } else {
            Ok(())
        }
    }
    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }
    fn descriptor(helper: &str, kind: Kind) -> Result<Allocation> {
        let (system, user) = kind.masks();
        let text = wide(&format!(
            "D:P(A;;0x{system:08x};;;S-1-5-18)(A;;0x{user:08x};;;{helper})"
        ));
        let mut raw = ptr::null_mut();
        check(unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut raw,
                ptr::null_mut(),
            )
        })?;
        Ok(Allocation(raw))
    }
    fn verify_handle(file: &File, helper: &str, kind: Kind) -> Result<()> {
        let mut acl = ptr::null_mut();
        let mut sd = ptr::null_mut();
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                1,
                4,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut acl,
                ptr::null_mut(),
                &mut sd,
            )
        };
        ensure!(
            status == 0,
            "native exact object DACL query failed: {status}"
        );
        let sd = Allocation(sd);
        let mut control = 0;
        let mut revision = 0;
        check(unsafe { GetSecurityDescriptorControl(sd.0, &mut control, &mut revision) })?;
        ensure!(!acl.is_null(), "null DACL forbidden");
        let header = unsafe { &*acl };
        ensure!(
            header.count == 2 && header.size >= 8,
            "bounded exact two-ACE DACL required"
        );
        let start = acl as usize;
        let end = start + usize::from(header.size);
        let mut owned = Vec::new();
        for index in 0..2 {
            let mut ace = ptr::null_mut();
            check(unsafe { GetAce(acl, index, &mut ace) })?;
            let position = ace as usize;
            ensure!(
                position >= start + 8 && position + 8 <= end,
                "ACE outside original ACL"
            );
            let bytes = ace.cast::<u8>();
            let size = unsafe { u16::from_le_bytes([*bytes.add(2), *bytes.add(3)]) } as usize;
            ensure!(
                unsafe { *bytes } == 0 && size >= 16 && position + size <= end,
                "only bounded ACCESS_ALLOWED_ACE admitted"
            );
            let flags = unsafe { *bytes.add(1) };
            let mask = unsafe { ptr::read_unaligned(bytes.add(4).cast::<u32>()) };
            let sid = unsafe { bytes.add(8) }.cast::<c_void>();
            // Bound the SID's declared subauthorities before any native SID access.
            let sid_size = 8 + 4 * usize::from(unsafe { *bytes.add(9) });
            ensure!(sid_size <= size - 8, "SID exceeds ACE storage");
            check(unsafe { IsValidSid(sid) })?;
            ensure!(
                unsafe { GetLengthSid(sid) } as usize == sid_size,
                "SID native length differs"
            );
            let mut text = ptr::null_mut();
            check(unsafe { ConvertSidToStringSidW(sid, &mut text) })?;
            let allocation = Allocation(text.cast());
            let mut length = 0;
            while length <= 256 && unsafe { *text.add(length) } != 0 {
                length += 1;
            }
            ensure!(length <= 256, "bounded SID string required");
            owned.push((
                String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })?,
                mask,
                flags,
            ));
            drop(allocation);
        }
        let grants = owned
            .iter()
            .map(|(s, m, f)| (s.as_str(), *m, *f))
            .collect::<Vec<_>>();
        validate_grants(helper, kind, control & 0x1000 != 0, &grants)
    }
    fn open(path: &Path, writable_acl: bool) -> Result<File> {
        let file = OpenOptions::new()
            .access_mode(0x20080 | if writable_acl { 0x40000 } else { 0 })
            .share_mode(1 | 2 | 4)
            .custom_flags(0x00200000 | 0x02000000)
            .open(path)?;
        ensure!(
            file.metadata()?.file_attributes() & 0x400 == 0,
            "reparse object cannot supply exact ACL authority"
        );
        Ok(file)
    }
    fn identity(file: &File) -> Result<[u8; 24]> {
        #[repr(C)]
        struct Id {
            volume: u64,
            id: [u8; 16],
        }
        let mut value = Id {
            volume: 0,
            id: [0; 16],
        };
        check(unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                18,
                (&mut value as *mut Id).cast(),
                std::mem::size_of::<Id>() as u32,
            )
        })?;
        let mut bytes = [0; 24];
        bytes[..8].copy_from_slice(&value.volume.to_le_bytes());
        bytes[8..].copy_from_slice(&value.id);
        Ok(bytes)
    }
    pub fn verify_path(path: &Path, helper: &str, kind: Kind) -> Result<()> {
        let parent = HostDir::open(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("exact parent required"))?,
            RootPolicy::AllComponentsNoFollow,
        )?;
        let file = open(path, false)?;
        verify_handle(&file, helper, kind)?;
        let current = HostDir::open(
            path.parent().expect("parent"),
            RootPolicy::AllComponentsNoFollow,
        )?;
        ensure!(
            parent.same_directory_object(&current)?
                && identity(&file)? == identity(&open(path, false)?)?,
            "ACL object or parent changed during preflight"
        );
        Ok(())
    }
    /// Only called immediately after exclusive fixture creation, before registration.
    pub fn initialize_created_path(path: &Path, helper: &str, kind: Kind) -> Result<()> {
        let parent = HostDir::open(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("exact parent required"))?,
            RootPolicy::AllComponentsNoFollow,
        )?;
        let original = if !matches!(kind, Kind::Directory) {
            match parent.open_leaf_classified(path.file_name().expect("leaf")) {
                LeafOpenResult::Opened(file) => Some(file),
                other => anyhow::bail!("created fixture file unavailable: {other:?}"),
            }
        } else {
            None
        };
        let file = open(path, true)?;
        if let Some(original) = &original {
            ensure!(
                fingerprint(original)? == fingerprint(&file)?,
                "created file replaced before ACL initialization"
            );
        }
        let sd = descriptor(helper, kind)?;
        let mut present = 0;
        let mut defaulted = 0;
        let mut acl = ptr::null_mut();
        check(unsafe { GetSecurityDescriptorDacl(sd.0, &mut present, &mut acl, &mut defaulted) })?;
        ensure!(present != 0 && !acl.is_null(), "fixture DACL missing");
        let status = unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                1,
                4 | 0x80000000,
                ptr::null(),
                ptr::null(),
                acl,
                ptr::null(),
            )
        };
        ensure!(
            status == 0,
            "created fixture DACL initialization failed: {status}"
        );
        verify_handle(&file, helper, kind)?;
        // DACL changes can affect mutable metadata; verify stable volume/file ID.
        let after = open(path, false)?;
        ensure!(
            identity(&file)? == identity(&after)?,
            "created fixture object replaced"
        );
        Ok(())
    }
    pub fn probe_install_paths(
        preflight: &theseus_native_protocol::v2::ScmPreflight,
    ) -> Result<()> {
        for (path, kind) in [
            (&preflight.executable, Kind::Executable),
            (&preflight.definition, Kind::Sidecar),
            (&preflight.data, Kind::Data),
        ] {
            let path = Path::new(path);
            let file = OpenOptions::new().write(true).create_new(true).open(path)?;
            drop(file);
            initialize_created_path(path, &preflight.helper_sid, kind)?;
            verify_path(path, &preflight.helper_sid, kind)?;
            let parent = HostDir::open(
                path.parent().expect("parent"),
                RootPolicy::AllComponentsNoFollow,
            )?;
            let LeafOpenResult::Opened(original) =
                parent.open_leaf_classified(path.file_name().expect("leaf"))
            else {
                anyhow::bail!("exact preflight probe lost")
            };
            parent.remove_created_leaf(path.file_name().expect("leaf"), &original)?;
        }
        let sd = descriptor(&preflight.helper_sid, Kind::Pipe)?;
        let attributes = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            descriptor: sd.0,
            inherit: 0,
        };
        // Exact local name, first instance, byte stream and native reject-remote flag.
        let name = wide(&preflight.pipe);
        let raw = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                3 | 0x00080000 | 0x40000000,
                8,
                1,
                4096,
                4096,
                0,
                &attributes,
            )
        };
        ensure!(
            raw as isize != -1,
            "exact private reject-remote pipe preflight failed: {}",
            io::Error::last_os_error()
        );
        let file = unsafe { File::from_raw_handle(raw) };
        verify_handle(&file, &preflight.helper_sid, Kind::Pipe)?;
        drop(file);
        Ok(())
    }
}
