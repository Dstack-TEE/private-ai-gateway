//! Read-only Windows helper inspection. Policy remains in the parent module;
//! the owner and DACL are read by `desktop_core::windows_acl`.
//! Handle locality/reparse checks follow:
//! https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-createfilew
//! https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-getfinalpathnamebyhandlew

use super::{validate_windows_acl, WindowsAce, WindowsAcl};
use std::{
    ffi::OsString,
    os::windows::{
        ffi::OsStringExt,
        io::{AsRawHandle, OwnedHandle},
    },
    path::{Component, Path, Prefix},
};
use windows_sys::Win32::{
    Storage::FileSystem::{
        GetDriveTypeW, GetFileInformationByHandle, GetFileType, GetFinalPathNameByHandleW,
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_TYPE_DISK,
    },
    System::WindowsProgramming::{DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOVABLE},
};

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
    // Opens the final link itself, with no write access or privilege changes.
    let handle = desktop_core::windows_acl::open_for_inspection(path)
        .map_err(|_| "Cannot open the OpenClaw helper for ACL inspection".to_string())?;
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

fn read_acl(handle: &OwnedHandle) -> Result<WindowsAcl, String> {
    let acl = desktop_core::windows_acl::read(handle)
        .map_err(|error| format!("OpenClaw helper: {error}"))?;
    Ok(WindowsAcl {
        owner_sid: acl.owner_sid,
        current_user_sid: acl.current_user_sid,
        local: true,
        dacl_present: true,
        aces: acl
            .aces
            .into_iter()
            .map(|ace| WindowsAce {
                sid: ace.sid,
                kind: ace.kind,
                flags: ace.flags,
                mask: ace.mask,
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use desktop_core::{transport::current_user_sid, windows_acl};

    /// A new file in this test's own temporary directory with the given DACL.
    fn fixture(path: &Path, sddl: &str) {
        let mut options = std::fs::OpenOptions::new();
        // Windows accepts `create_new` only together with write access.
        options.write(true).create_new(true);
        let file = windows_acl::allow_dacl_change(&mut options)
            .open(path)
            .unwrap();
        windows_acl::set_dacl(&file, sddl).unwrap();
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
            r"\\pap-test.invalid\share\helper.exe",
            r"\\?\UNC\pap-test.invalid\share\helper.exe",
            r"\\.\pipe\pap-helper",
            r"C:relative",
            "relative",
        ] {
            assert!(validate_helper(Path::new(path)).is_err());
        }
    }
}
