//! Read-only Windows helper inspection. Policy remains in the parent module.
//! GetSecurityInfo owns one LocalFree allocation; owner/DACL/ACE pointers borrow it:
//! https://learn.microsoft.com/windows/win32/api/aclapi/nf-aclapi-getsecurityinfo
//! https://learn.microsoft.com/windows/win32/api/securitybaseapi/nf-securitybaseapi-getace
//! TokenUser contains a SID inside the caller's GetTokenInformation buffer:
//! https://learn.microsoft.com/windows/win32/api/securitybaseapi/nf-securitybaseapi-gettokeninformation
//! Handle locality/reparse checks and allocated SID strings follow:
//! https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-createfilew
//! https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-getfinalpathnamebyhandlew
//! https://learn.microsoft.com/windows/win32/api/sddl/nf-sddl-convertsidtostringsidw

use super::{validate_windows_acl, WindowsAce, WindowsAcl};
use std::{
    ffi::{c_void, OsString},
    mem::{offset_of, size_of},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Component, Path, Prefix},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{
        GetLastError, LocalFree, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, INVALID_HANDLE_VALUE,
    },
    Security::{
        Authorization::{ConvertSidToStringSidW, GetSecurityInfo, SE_FILE_OBJECT},
        GetAce, GetLengthSid, GetSecurityDescriptorDacl, GetSecurityDescriptorLength,
        GetTokenInformation, IsValidAcl, IsValidSecurityDescriptor, IsValidSid, TokenUser,
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
        PSID, TOKEN_QUERY, TOKEN_USER,
    },
    Storage::FileSystem::{
        CreateFileW, GetDriveTypeW, GetFileInformationByHandle, GetFileType,
        GetFinalPathNameByHandleW, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK, OPEN_EXISTING,
        READ_CONTROL,
    },
    System::{
        Threading::{GetCurrentProcess, OpenProcessToken},
        WindowsProgramming::{DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOVABLE},
    },
};

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // Only successful APIs documented to return LocalAlloc storage construct this owner.
        unsafe { LocalFree(self.0) };
    }
}

fn wide(path: &Path) -> Result<Vec<u16>, String> {
    let mut text: Vec<_> = path.as_os_str().encode_wide().collect();
    if text.contains(&0) || text.len() >= 32768 {
        return Err("The OpenClaw helper path cannot be inspected".into());
    }
    text.push(0);
    Ok(text)
}

fn local_drive(path: &Path) -> Result<(), String> {
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return Err("The OpenClaw helper requires an absolute local drive path".into());
    };
    let drive = match prefix.kind() {
        Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) if path.is_absolute() => drive,
        // Reject UNC, device namespaces and relative paths before opening anything.
        _ => return Err("The OpenClaw helper requires an absolute local drive path".into()),
    };
    let root = [u16::from(drive), b':' as u16, b'\\' as u16, 0];
    // A NUL-terminated drive root; unknown, absent and network drives all fail closed.
    if !matches!(
        unsafe { GetDriveTypeW(root.as_ptr()) },
        DRIVE_FIXED | DRIVE_REMOVABLE | DRIVE_CDROM | DRIVE_RAMDISK
    ) {
        return Err("The OpenClaw helper must be on a local drive".into());
    }
    Ok(())
}

pub(super) fn validate_helper(path: &Path) -> Result<(), String> {
    local_drive(path)?;
    let path = wide(path)?;
    // OPEN_REPARSE_POINT opens the final link itself. No write access or privilege changes.
    let raw = unsafe {
        CreateFileW(
            path.as_ptr(),
            READ_CONTROL | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE || raw.is_null() {
        return Err("Cannot open the OpenClaw helper for ACL inspection".into());
    }
    // CreateFileW transferred this non-pseudo handle to us; close it on every exit.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileType(handle.as_raw_handle()) } != FILE_TYPE_DISK
        || unsafe { GetFileInformationByHandle(handle.as_raw_handle(), &mut info) } == 0
        || info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
    {
        return Err("The OpenClaw helper must be a regular file, not a reparse point".into());
    }
    // Inspect the opened object, not a second path lookup (parents may be junctions).
    let mut final_path = vec![0u16; 32768];
    let written = unsafe {
        GetFinalPathNameByHandleW(
            handle.as_raw_handle(),
            final_path.as_mut_ptr(),
            final_path.len() as u32,
            0,
        )
    } as usize;
    if written == 0 || written >= final_path.len() {
        return Err("Cannot verify the OpenClaw helper's local path".into());
    }
    local_drive(Path::new(&OsString::from_wide(&final_path[..written])))?;
    validate_windows_acl(&read_acl(&handle)?)
}

// Return the readable tail of an API-owned allocation, checking before every cast.
fn remaining(
    base: *const c_void,
    length: usize,
    pointer: *const c_void,
    minimum: usize,
) -> Result<usize, String> {
    (pointer as usize)
        .checked_sub(base as usize)
        .and_then(|offset| length.checked_sub(offset))
        .filter(|available| !pointer.is_null() && *available >= minimum)
        .ok_or_else(|| "The OpenClaw helper has invalid security data bounds".into())
}

// SAFETY: sid points to at least available readable bytes for the duration of this call.
unsafe fn sid_text(sid: PSID, available: usize) -> Result<String, String> {
    if sid.is_null() || available < 8 {
        return Err("The OpenClaw helper has an invalid SID".into());
    }
    // A SID has an eight-byte header and SubAuthorityCount four-byte subauthorities.
    let length = 8 + 4 * usize::from(unsafe { *sid.cast::<u8>().add(1) });
    if length > available
        || unsafe { IsValidSid(sid) } == 0
        || unsafe { GetLengthSid(sid) } as usize != length
    {
        return Err("The OpenClaw helper has an invalid SID".into());
    }
    let mut text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 || text.is_null() {
        return Err("Cannot identify an OpenClaw helper SID".into());
    }
    let _text = LocalAllocation(text.cast());
    // ConvertSidToStringSidW guarantees allocated, NUL-terminated UTF-16 output.
    // A valid SID string fits within 184 characters (15 subauthorities).
    for length in 0..=184 {
        if unsafe { *text.add(length) } == 0 {
            return String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
                .map_err(|_| "Cannot decode an OpenClaw helper SID".into());
        }
    }
    Err("The OpenClaw helper SID exceeds its bounds".into())
}

fn current_user_sid() -> Result<String, String> {
    let mut raw = null_mut();
    // GetCurrentProcess is borrowed; only the returned process token is owned.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 || raw.is_null()
    {
        return Err("Cannot inspect the current Windows user".into());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut needed = 0;
    if unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut needed) }
        != 0
        || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER
        || !(size_of::<TOKEN_USER>()..=65536).contains(&(needed as usize))
    {
        return Err("Cannot size the current Windows user token".into());
    }
    // usize storage aligns TOKEN_USER and its embedded SID; the API gets its byte capacity.
    let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    let capacity = buffer.len() * size_of::<usize>();
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            capacity as u32,
            &mut needed,
        )
    } == 0
        || !(size_of::<TOKEN_USER>()..=capacity).contains(&(needed as usize))
    {
        return Err("Cannot read the current Windows user token".into());
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let available = remaining(buffer.as_ptr().cast(), needed as usize, user.User.Sid, 8)?;
    unsafe { sid_text(user.User.Sid, available) }
}

fn read_acl(handle: &OwnedHandle) -> Result<WindowsAcl, String> {
    let (mut owner, mut dacl, mut descriptor) = (null_mut(), null_mut(), null_mut());
    let status = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS || descriptor.is_null() {
        return Err("Cannot read the OpenClaw helper owner and DACL".into());
    }
    let descriptor = LocalAllocation(descriptor);
    // All pointers below borrow this successful API allocation, retained until parsing ends.
    if unsafe { IsValidSecurityDescriptor(descriptor.0) } == 0 {
        return Err("The OpenClaw helper security descriptor is invalid".into());
    }
    let length = unsafe { GetSecurityDescriptorLength(descriptor.0) } as usize;
    let owner_available = remaining(descriptor.0, length, owner, 8)?;
    let owner_sid = unsafe { sid_text(owner, owner_available) }?;
    let (mut present, mut defaulted, mut checked_dacl) = (0, 0, null_mut());
    if unsafe {
        GetSecurityDescriptorDacl(
            descriptor.0,
            &mut present,
            &mut checked_dacl,
            &mut defaulted,
        )
    } == 0
        || present == 0
        || dacl.is_null()
        || dacl != checked_dacl
    {
        return Err("The OpenClaw helper requires a present, non-null DACL".into());
    }
    let available = remaining(descriptor.0, length, dacl.cast(), size_of::<ACL>())?;
    let header = unsafe { dacl.read_unaligned() };
    let acl_size = usize::from(header.AclSize);
    if acl_size < size_of::<ACL>()
        || acl_size > available
        || header.AceCount > 256
        || unsafe { IsValidAcl(dacl) } == 0
    {
        return Err("The OpenClaw helper DACL is invalid or too large".into());
    }
    let mut aces = Vec::with_capacity(header.AceCount.into());
    for index in 0..u32::from(header.AceCount) {
        let mut raw = null_mut();
        if unsafe { GetAce(dacl, index, &mut raw) } == 0 {
            return Err("Cannot read an OpenClaw helper ACL entry".into());
        }
        let available = remaining(dacl.cast(), acl_size, raw, size_of::<ACE_HEADER>())?;
        let header = unsafe { raw.cast::<ACE_HEADER>().read_unaligned() };
        let sid_offset = offset_of!(ACCESS_ALLOWED_ACE, SidStart);
        if !matches!(header.AceType, 0 | 1)
            || usize::from(header.AceSize) > available
            || usize::from(header.AceSize) < sid_offset + 8
        {
            return Err("The OpenClaw helper has an unsupported ACL entry".into());
        }
        // Basic allow/deny ACEs share Mask/SidStart layout. The SID stays inside AceSize.
        let ace = unsafe { raw.cast::<ACCESS_ALLOWED_ACE>().read_unaligned() };
        let sid = unsafe { raw.cast::<u8>().add(sid_offset) }.cast();
        aces.push(WindowsAce {
            sid: unsafe { sid_text(sid, usize::from(header.AceSize) - sid_offset) }?,
            kind: header.AceType,
            flags: header.AceFlags,
            mask: ace.Mask,
        });
    }
    Ok(WindowsAcl {
        owner_sid,
        current_user_sid: current_user_sid()?,
        local: true,
        dacl_present: true,
        aces,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::{
        Security::{
            Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SECURITY_ATTRIBUTES,
        },
        Storage::FileSystem::{CREATE_NEW, FILE_ATTRIBUTE_NORMAL},
    };

    fn fixture(path: &Path, sddl: &str) {
        let text: Vec<_> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut descriptor = null_mut();
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    1,
                    &mut descriptor,
                    null_mut(),
                )
            },
            0
        );
        let descriptor = LocalAllocation(descriptor);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        let path = wide(path).unwrap();
        // Set ACLs only at creation of a new file inside this test's owned temp directory.
        let raw = unsafe {
            CreateFileW(
                path.as_ptr(),
                READ_CONTROL,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                &attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        };
        assert_ne!(
            raw,
            INVALID_HANDLE_VALUE,
            "fixture creation failed: {}",
            unsafe { GetLastError() }
        );
        assert!(!raw.is_null());
        drop(unsafe { OwnedHandle::from_raw_handle(raw) });
    }

    #[test]
    fn native_file_acls_allow_private_and_read_only_but_refuse_foreign_write_and_null() {
        let temp = tempfile::tempdir().unwrap();
        let current = current_user_sid().unwrap();
        let private = format!("(A;;FA;;;{current})");
        // FW includes READ_CONTROL. Deny only FILE_WRITE_DATA when exercising
        // policy on a readable ACL; separately deny the inspector's read rights.
        for (name, acl, allowed) in [
            ("private", format!("D:P{private}"), true),
            (
                "system-admin",
                format!("D:P{private}(A;;FA;;;SY)(A;;FA;;;BA)"),
                true,
            ),
            ("read", format!("D:P{private}(A;;FR;;;WD)"), true),
            ("write", format!("D:P{private}(A;;FW;;;WD)"), false),
            ("deny", format!("D:P(D;;0x00000002;;;WD){private}"), true),
            (
                "deny-inspection",
                format!("D:P(D;;FRFW;;;WD){private}"),
                false,
            ),
            (
                "deny-and-allow",
                format!("D:P(D;;0x00000002;;;WD){private}(A;;0x00000002;;;WD)"),
                false,
            ),
            ("null", "D:NO_ACCESS_CONTROL".into(), false),
        ] {
            let path = temp.path().join(name);
            fixture(&path, &acl);
            let result = validate_helper(&path);
            assert_eq!(result.is_ok(), allowed, "{name}: {result:?}");
            if name == "deny-and-allow" {
                assert!(
                    result
                        .as_ref()
                        .is_err_and(|error| error.contains("permits another")),
                    "{result:?}"
                );
            }
            if name == "deny-inspection" {
                assert!(
                    result
                        .as_ref()
                        .is_err_and(|error| error.contains("Cannot open")),
                    "{result:?}"
                );
            }
        }
    }

    #[test]
    fn native_file_checks_refuse_directories_reparse_points_and_missing_files() {
        let temp = tempfile::tempdir().unwrap();
        assert!(validate_helper(temp.path()).is_err());
        assert!(validate_helper(&temp.path().join("absent")).is_err());
        let target = temp.path().join("target");
        fixture(
            &target,
            &format!("D:P(A;;FA;;;{})", current_user_sid().unwrap()),
        );
        let link = temp.path().join("link");
        std::os::windows::fs::symlink_file(&target, &link).unwrap();
        assert!(validate_helper(&link).is_err());
    }

    #[test]
    fn unsupported_namespaces_are_rejected_before_opening() {
        for path in [
            r"\\pag-test.invalid\share\helper.exe",
            r"\\?\UNC\pag-test.invalid\share\helper.exe",
            r"\\.\pipe\pag-helper",
            r"C:relative",
            "relative",
        ] {
            assert!(validate_helper(Path::new(path)).is_err());
        }
    }
}
