//! Reading and setting a file's owner and DACL, for the checks and the
//! owner-only files that need them (private files in [`crate::private_fs`],
//! the OpenClaw helper inspection in agent-bridge). Policy stays with callers.
//!
//! GetSecurityInfo owns one LocalFree allocation; owner/DACL/ACE pointers borrow it:
//! https://learn.microsoft.com/windows/win32/api/aclapi/nf-aclapi-getsecurityinfo
//! https://learn.microsoft.com/windows/win32/api/securitybaseapi/nf-securitybaseapi-getace
//! Allocated SID strings and SDDL conversion:
//! https://learn.microsoft.com/windows/win32/api/sddl/nf-sddl-convertsidtostringsidw
//! https://learn.microsoft.com/windows/win32/api/sddl/nf-sddl-convertstringsecuritydescriptortosecuritydescriptorw
//! https://learn.microsoft.com/windows/win32/api/aclapi/nf-aclapi-setsecurityinfo

use std::{
    ffi::c_void,
    fs, io,
    mem::{offset_of, size_of},
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr::{null, null_mut},
};

use windows_sys::Win32::{
    Foundation::{LocalFree, ERROR_SUCCESS, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
        },
        GetAce, GetLengthSid, GetSecurityDescriptorDacl, GetSecurityDescriptorLength, IsValidAcl,
        IsValidSecurityDescriptor, IsValidSid, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
        DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        PSID,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
    },
};

/// LocalSystem and the built-in Administrators group, which Windows trusts
/// with every user's files (as OpenSSH for Windows does for private keys).
pub const TRUSTED_SIDS: [&str; 2] = ["S-1-5-18", "S-1-5-32-544"];

/// A file's owner and the basic entries of its DACL.
#[derive(Debug)]
pub struct Acl {
    pub owner_sid: String,
    pub current_user_sid: String,
    pub aces: Vec<Ace>,
}

/// A basic access-allowed (kind 0) or access-denied (kind 1) entry.
#[derive(Debug)]
pub struct Ace {
    pub sid: String,
    pub kind: u8,
    pub flags: u8,
    pub mask: u32,
}

impl Acl {
    /// Whether an allow entry grants read access to anyone but the current
    /// user and [`TRUSTED_SIDS`]. Deny entries are not credited.
    pub fn readable_by_others(&self) -> bool {
        // FILE_READ_DATA, GENERIC_ALL and GENERIC_READ; FILE_ALL_ACCESS and
        // FILE_GENERIC_READ include FILE_READ_DATA.
        const READ: u32 = 0x0000_0001 | 0x1000_0000 | 0x8000_0000;
        // INHERIT_ONLY_ACE entries do not apply to the file itself.
        const INHERIT_ONLY: u8 = 0x08;
        self.aces.iter().any(|ace| {
            ace.kind == 0
                && ace.flags & INHERIT_ONLY == 0
                && ace.mask & READ != 0
                && ace.sid != self.current_user_sid
                && !TRUSTED_SIDS.contains(&ace.sid.as_str())
        })
    }
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // Only successful APIs documented to return LocalAlloc storage construct this owner.
        unsafe { LocalFree(self.0) };
    }
}

fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut text: Vec<_> = path.as_os_str().encode_wide().collect();
    if text.contains(&0) || text.len() >= 32768 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path cannot be inspected",
        ));
    }
    text.push(0);
    Ok(text)
}

/// Opens `path` itself (never a link target) to inspect its security, with no
/// write access and no privilege changes.
pub fn open_for_inspection(path: &Path) -> io::Result<OwnedHandle> {
    let path = wide(path)?;
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
        return Err(io::Error::last_os_error());
    }
    // CreateFileW transferred this non-pseudo handle to us; close it on every exit.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// Lets a file being created through `options` have its DACL replaced
/// through the resulting handle (read, write and `WRITE_DAC` access).
pub fn allow_dacl_change(options: &mut fs::OpenOptions) -> &mut fs::OpenOptions {
    options.access_mode(GENERIC_READ | GENERIC_WRITE | WRITE_DAC)
}

/// Opens an existing `path` itself (never a link target) to replace its DACL.
pub fn open_for_dacl_change(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Gives an open file (with `WRITE_DAC` access) a protected DACL granting
/// full access to the current user and [`TRUSTED_SIDS`] only: the owner-only
/// DACL of the IPC endpoint, plus the trusted system principals.
pub fn restrict_to_current_user(file: &fs::File) -> io::Result<()> {
    let user = crate::transport::current_user_sid()?;
    set_dacl(
        file,
        &format!("D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{user})"),
    )
}

/// Replaces the DACL of an open file (with `WRITE_DAC` access) with the one
/// an SDDL string describes; `D:P` protects it from inheritance. Working on
/// the handle, not the path, means the file checked is the file changed.
pub fn set_dacl(file: &fs::File, sddl: &str) -> io::Result<()> {
    let sddl: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut descriptor = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let descriptor = LocalAllocation(descriptor);
    let (mut present, mut defaulted, mut dacl) = (0, 0, null_mut());
    if unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    if present == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the security descriptor has no DACL",
        ));
    }
    // A null DACL (`D:NO_ACCESS_CONTROL`) passes through as null.
    let status = unsafe {
        SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
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
        .ok_or_else(|| "The file has invalid security data bounds".into())
}

// SAFETY: sid points to at least available readable bytes for the duration of this call.
unsafe fn sid_text(sid: PSID, available: usize) -> Result<String, String> {
    if sid.is_null() || available < 8 {
        return Err("The file has an invalid SID".into());
    }
    // A SID has an eight-byte header and SubAuthorityCount four-byte subauthorities.
    let length = 8 + 4 * usize::from(unsafe { *sid.cast::<u8>().add(1) });
    if length > available
        || unsafe { IsValidSid(sid) } == 0
        || unsafe { GetLengthSid(sid) } as usize != length
    {
        return Err("The file has an invalid SID".into());
    }
    let mut text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 || text.is_null() {
        return Err("Cannot identify a SID of the file".into());
    }
    let _text = LocalAllocation(text.cast());
    // ConvertSidToStringSidW guarantees allocated, NUL-terminated UTF-16 output.
    // A valid SID string fits within 184 characters (15 subauthorities).
    for length in 0..=184 {
        if unsafe { *text.add(length) } == 0 {
            return String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
                .map_err(|_| "Cannot decode a SID of the file".into());
        }
    }
    Err("A SID of the file exceeds its bounds".into())
}

/// Reads the owner and the basic DACL entries of an open file. A missing or
/// null DACL and entries other than basic allow/deny fail closed.
pub fn read(handle: &OwnedHandle) -> Result<Acl, String> {
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
        return Err("Cannot read the file owner and DACL".into());
    }
    let descriptor = LocalAllocation(descriptor);
    // All pointers below borrow this successful API allocation, retained until parsing ends.
    if unsafe { IsValidSecurityDescriptor(descriptor.0) } == 0 {
        return Err("The file security descriptor is invalid".into());
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
        return Err("The file requires a present, non-null DACL".into());
    }
    let available = remaining(descriptor.0, length, dacl.cast(), size_of::<ACL>())?;
    let header = unsafe { dacl.read_unaligned() };
    let acl_size = usize::from(header.AclSize);
    if acl_size < size_of::<ACL>()
        || acl_size > available
        || header.AceCount > 256
        || unsafe { IsValidAcl(dacl) } == 0
    {
        return Err("The file DACL is invalid or too large".into());
    }
    let mut aces = Vec::with_capacity(header.AceCount.into());
    for index in 0..u32::from(header.AceCount) {
        let mut raw = null_mut();
        if unsafe { GetAce(dacl, index, &mut raw) } == 0 {
            return Err("Cannot read an ACL entry of the file".into());
        }
        let available = remaining(dacl.cast(), acl_size, raw, size_of::<ACE_HEADER>())?;
        let header = unsafe { raw.cast::<ACE_HEADER>().read_unaligned() };
        let sid_offset = offset_of!(ACCESS_ALLOWED_ACE, SidStart);
        if !matches!(header.AceType, 0 | 1)
            || usize::from(header.AceSize) > available
            || usize::from(header.AceSize) < sid_offset + 8
        {
            return Err("The file has an unsupported ACL entry".into());
        }
        // Basic allow/deny ACEs share Mask/SidStart layout. The SID stays inside AceSize.
        let ace = unsafe { raw.cast::<ACCESS_ALLOWED_ACE>().read_unaligned() };
        let sid = unsafe { raw.cast::<u8>().add(sid_offset) }.cast();
        aces.push(Ace {
            sid: unsafe { sid_text(sid, usize::from(header.AceSize) - sid_offset) }?,
            kind: header.AceType,
            flags: header.AceFlags,
            mask: ace.Mask,
        });
    }
    Ok(Acl {
        owner_sid,
        current_user_sid: crate::transport::current_user_sid()
            .map_err(|_| "Cannot inspect the current Windows user".to_string())?,
        aces,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricted_files_are_readable_only_by_the_user_and_trusted_principals() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("credentials.toml");
        std::fs::write(&path, "").unwrap();
        restrict_to_current_user(&open_for_dacl_change(&path).unwrap()).unwrap();
        let acl = read(&open_for_inspection(&path).unwrap()).unwrap();
        assert!(!acl.readable_by_others(), "{acl:?}");
        let mut sids: Vec<_> = acl.aces.iter().map(|ace| ace.sid.as_str()).collect();
        sids.sort_unstable();
        let mut expected = vec![
            acl.current_user_sid.as_str(),
            TRUSTED_SIDS[0],
            TRUSTED_SIDS[1],
        ];
        expected.sort_unstable();
        assert_eq!(sids, expected);

        let everyone = Acl {
            aces: vec![Ace {
                sid: "S-1-1-0".into(),
                kind: 0,
                flags: 0,
                mask: 0x0012_0089,
            }],
            ..acl
        };
        assert!(everyone.readable_by_others());
    }
}
