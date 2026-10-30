#![cfg(windows)]
use std::{
    fs::File,
    io::{self, Write},
    mem::{align_of, offset_of, size_of},
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr::{null, null_mut},
};
use theseus_host::fs::{HostDir, RelativePath, RootPolicy, fingerprint};
use windows_sys::{
    Wdk::{Foundation::OBJECT_ATTRIBUTES, System::SystemServices::FILE_FS_DEVICE_INFORMATION},
    Win32::{
        Foundation::*,
        Security::{
            Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
            *,
        },
        Storage::FileSystem::*,
        System::{IO::IO_STATUS_BLOCK, Threading::*},
    },
};

fn checked(ok: i32) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[test]
fn filesystem_abi_matches_x64_msvc() {
    assert_eq!(
        (size_of::<UNICODE_STRING>(), align_of::<UNICODE_STRING>()),
        (16, 8)
    );
    assert_eq!(
        (
            size_of::<OBJECT_ATTRIBUTES>(),
            align_of::<OBJECT_ATTRIBUTES>()
        ),
        (48, 8)
    );
    assert_eq!(offset_of!(OBJECT_ATTRIBUTES, SecurityDescriptor), 32);
    assert_eq!(
        (size_of::<IO_STATUS_BLOCK>(), align_of::<IO_STATUS_BLOCK>()),
        (16, 8)
    );
    assert_eq!(offset_of!(IO_STATUS_BLOCK, Information), 8);
    assert_eq!(
        (size_of::<FILE_ID_INFO>(), align_of::<FILE_ID_INFO>()),
        (24, 8)
    );
    assert_eq!(size_of::<FILE_ID_128>(), 16);
    assert_eq!(offset_of!(FILE_ID_INFO, FileId), 8);
    assert_eq!(size_of::<FILE_BASIC_INFO>(), 40);
    assert_eq!(offset_of!(FILE_BASIC_INFO, ChangeTime), 24);
    assert_eq!(size_of::<FILE_STANDARD_INFO>(), 24);
    assert_eq!(offset_of!(FILE_STANDARD_INFO, Directory), 21);
    assert_eq!(size_of::<FILE_ATTRIBUTE_TAG_INFO>(), 8);
    assert_eq!(size_of::<SECURITY_DESCRIPTOR>(), 40);
    assert_eq!(offset_of!(SECURITY_DESCRIPTOR, Dacl), 32);
    assert_eq!(size_of::<ACL>(), 8);
    assert_eq!(offset_of!(ACCESS_ALLOWED_ACE, SidStart), 8);
    assert_eq!(size_of::<TOKEN_USER>(), 16);
    assert_eq!(size_of::<FILE_FS_DEVICE_INFORMATION>(), 8);
}

fn token_user(token: &OwnedHandle) -> io::Result<Vec<usize>> {
    unsafe {
        let mut bytes = 0;
        GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut bytes);
        let mut storage = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
        checked(GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            storage.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        ))?;
        Ok(storage)
    }
}
fn effective_token() -> io::Result<OwnedHandle> {
    unsafe {
        let mut raw = null_mut();
        if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut raw) == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
                return Err(error);
            }
            checked(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw))?;
        }
        Ok(OwnedHandle::from_raw_handle(raw))
    }
}
fn assert_private(file: &File) -> io::Result<()> {
    // Query the kernel's installed descriptor, not the constructor's input.
    unsafe {
        let mut descriptor = null_mut();
        let mut acl = null_mut();
        let status = GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut acl,
            null_mut(),
            &mut descriptor,
        );
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        struct FreeDescriptor(PSECURITY_DESCRIPTOR);
        impl Drop for FreeDescriptor {
            fn drop(&mut self) {
                unsafe {
                    LocalFree(self.0);
                }
            }
        }
        let _descriptor = FreeDescriptor(descriptor);
        let mut control = 0;
        let mut revision = 0;
        checked(GetSecurityDescriptorControl(
            descriptor,
            &mut control,
            &mut revision,
        ))?;
        assert_ne!(control & SE_DACL_PROTECTED, 0);
        assert!(!acl.is_null());
        assert_eq!((*acl).AceCount, 2);
        let user = token_user(&effective_token()?)?;
        let user_sid = (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid;
        let mut system = [0usize; 9];
        let mut system_bytes = size_of_val(&system) as u32;
        checked(CreateWellKnownSid(
            WinLocalSystemSid,
            null_mut(),
            system.as_mut_ptr().cast(),
            &mut system_bytes,
        ))?;
        let mut saw_user = false;
        let mut saw_system = false;
        for index in 0..2 {
            let mut raw_ace = null_mut();
            checked(GetAce(acl, index, &mut raw_ace))?;
            let ace = &*raw_ace.cast::<ACCESS_ALLOWED_ACE>();
            assert_eq!(ace.Header.AceType, 0); // ACCESS_ALLOWED_ACE_TYPE
            assert_eq!(ace.Header.AceFlags, 0); // neither inherited nor inherit-only
            assert_eq!(ace.Mask, FILE_ALL_ACCESS);
            let sid = (raw_ace.cast::<u8>())
                .add(offset_of!(ACCESS_ALLOWED_ACE, SidStart))
                .cast();
            if EqualSid(sid, user_sid) != 0 {
                saw_user = true;
            } else if EqualSid(sid, system.as_mut_ptr().cast()) != 0 {
                saw_system = true;
            } else {
                panic!("unexpected broad-grant SID in private DACL");
            }
        }
        assert!(saw_user && saw_system);
    }
    Ok(())
}
fn directory_file(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}
fn exercise_private_creation(parent: &Path) -> io::Result<()> {
    let root = HostDir::create_private(&parent.join("private"))?;
    let mut file = root.create_file(&RelativePath::parse("nested/evidence")?)?;
    file.write_all(b"private bytes")?;
    file.sync_all()?;
    assert_private(&file)?;
    assert_private(&directory_file(&parent.join("private"))?)?;
    assert_private(&directory_file(&parent.join("private/nested"))?)?;
    let reopened = root.open_file(&RelativePath::parse("nested/evidence")?)?;
    assert_eq!(fingerprint(&reopened)?.len, 13);
    Ok(())
}
#[test]
fn installed_dacl_is_protected_user_and_system_only() -> io::Result<()> {
    let temp = tempfile::tempdir()?;
    exercise_private_creation(temp.path())
}

#[test]
fn creation_works_with_admin_groups_disabled_and_privileges_removed() -> io::Result<()> {
    let temp = tempfile::tempdir()?;
    unsafe {
        let mut process_token = null_mut();
        checked(OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY,
            &mut process_token,
        ))?;
        let process_token = OwnedHandle::from_raw_handle(process_token);
        let mut restricted = null_mut();
        checked(CreateRestrictedToken(
            process_token.as_raw_handle(),
            DISABLE_MAX_PRIVILEGE | LUA_TOKEN,
            0,
            null(),
            0,
            null(),
            0,
            null(),
            &mut restricted,
        ))?;
        let restricted = OwnedHandle::from_raw_handle(restricted);
        checked(ImpersonateLoggedOnUser(restricted.as_raw_handle()))?;
        struct Revert;
        impl Drop for Revert {
            fn drop(&mut self) {
                assert_ne!(unsafe { RevertToSelf() }, 0);
            }
        }
        let _revert = Revert;
        let mut admin = [0usize; 9];
        let mut admin_len = size_of_val(&admin) as u32;
        checked(CreateWellKnownSid(
            WinBuiltinAdministratorsSid,
            null_mut(),
            admin.as_mut_ptr().cast(),
            &mut admin_len,
        ))?;
        let mut member = 1;
        checked(CheckTokenMembership(
            null_mut(),
            admin.as_mut_ptr().cast(),
            &mut member,
        ))?;
        assert_eq!(
            member, 0,
            "test must actually run without enabled administrator membership"
        );
        exercise_private_creation(temp.path())?;
    }
    Ok(())
}

fn junction(link: &Path, target: &Path) -> io::Result<()> {
    let result = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()?;
    if !result.status.success() {
        return Err(io::Error::other(format!(
            "junction creation failed: {}",
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    Ok(())
}
#[test]
fn junctions_and_substituted_ancestors_are_rejected() -> io::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("root");
    let root = HostDir::create_private(&path)?;
    root.create_child("child".as_ref())?;
    junction(&temp.path().join("alias"), &path)?;
    for alias in [temp.path().join("alias"), temp.path().join("alias/")] {
        assert!(HostDir::open(&alias, RootPolicy::LeafNoFollow).is_err());
    }
    HostDir::open(&temp.path().join("alias/child"), RootPolicy::LeafNoFollow)?;
    HostDir::create_private(&temp.path().join("alias/new"))?;
    assert!(path.join("new").is_dir());
    assert!(
        HostDir::open(
            &temp.path().join("alias/child"),
            RootPolicy::AllComponentsNoFollow
        )
        .is_err()
    );
    std::fs::rename(path.join("child"), path.join("old"))?;
    junction(&path.join("child"), temp.path())?;
    assert!(
        root.create_file(&RelativePath::parse("child/escape")?)
            .is_err()
    );
    assert!(root.open_child(&RelativePath::parse("child")?).is_err());
    assert!(!temp.path().join("escape").exists());
    Ok(())
}

fn native_identity(file: &File) -> io::Result<FILE_ID_INFO> {
    let mut info = FILE_ID_INFO::default();
    checked(unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    })?;
    Ok(info)
}
fn basic_info(file: &File) -> io::Result<FILE_BASIC_INFO> {
    let mut info = FILE_BASIC_INFO::default();
    checked(unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            (&mut info as *mut FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    })?;
    Ok(info)
}
#[test]
fn identity_distinguishes_equal_bytes_size_and_all_timestamps() -> io::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = HostDir::create_private(&temp.path().join("run"))?;
    let name = RelativePath::parse("evidence")?;
    let mut old = root.create_file(&name)?;
    old.write_all(b"identical")?;
    old.sync_all()?;
    let original_id = native_identity(&old)?;
    std::fs::rename(
        temp.path().join("run/evidence"),
        temp.path().join("run/old"),
    )?;
    let mut new = root.create_file(&name)?;
    new.write_all(b"identical")?;
    new.sync_all()?;
    // Match creation/access/write/change stamps, so only native identity can
    // distinguish the two equal-size files. Query back the kernel result.
    let old_basic = basic_info(&old)?;
    checked(unsafe {
        SetFileInformationByHandle(
            new.as_raw_handle(),
            FileBasicInfo,
            (&old_basic as *const FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    })?;
    let new_basic = basic_info(&new)?;
    assert_eq!(
        (
            old_basic.CreationTime,
            old_basic.LastAccessTime,
            old_basic.LastWriteTime,
            old_basic.ChangeTime
        ),
        (
            new_basic.CreationTime,
            new_basic.LastAccessTime,
            new_basic.LastWriteTime,
            new_basic.ChangeTime
        )
    );
    assert_eq!(old.metadata()?.len(), new.metadata()?.len());
    let retained_id = native_identity(&old)?;
    assert_eq!(
        original_id.VolumeSerialNumber,
        retained_id.VolumeSerialNumber
    );
    assert_eq!(original_id.FileId.Identifier, retained_id.FileId.Identifier);
    assert_ne!(
        original_id.FileId.Identifier,
        native_identity(&new)?.FileId.Identifier
    );
    assert_ne!(fingerprint(&old)?, fingerprint(&root.open_file(&name)?)?);
    Ok(())
}
