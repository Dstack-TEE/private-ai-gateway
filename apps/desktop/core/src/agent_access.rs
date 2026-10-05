use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum AgentAccessStatus {
    Authorized,
    AuthorizationRequired,
    ReauthorizationRequired,
}

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
mod mac_app_store {
    use super::AgentAccessStatus;
    use objc2::{rc::Retained, runtime::Bool};
    use objc2_foundation::{
        NSData, NSString, NSURLBookmarkCreationOptions, NSURLBookmarkResolutionOptions, NSURL,
    };
    use std::{
        ffi::{CStr, OsStr},
        fs,
        io::Write,
        os::unix::ffi::OsStrExt,
        path::{Path, PathBuf},
    };

    const BOOKMARK_FILE: &str = "agent-home.bookmark";
    const SERVICE_BOOKMARK_FILE: &str = "agent-home.service-bookmark";
    const MAX_BOOKMARK_BYTES: u64 = 1024 * 1024;

    /// A resolved Home bookmark. Apple requires each successful
    /// `startAccessingSecurityScopedResource` to be balanced by one
    /// `stopAccessingSecurityScopedResource`, so the access records whether
    /// it started and `Drop` stops only that.
    pub struct AgentHomeAccess {
        url: Retained<NSURL>,
        home: PathBuf,
        started: bool,
    }

    impl AgentHomeAccess {
        pub fn home(&self) -> &Path {
            &self.home
        }
    }

    impl Drop for AgentHomeAccess {
        fn drop(&mut self) {
            if self.started {
                unsafe { self.url.stopAccessingSecurityScopedResource() };
            }
        }
    }

    pub fn expected_home() -> Result<PathBuf, String> {
        let buffer_size = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
        let mut buffer = vec![0_u8; buffer_size.max(16_384) as usize];
        let mut password = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let status = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                password.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status != 0 || result.is_null() {
            return Err("Cannot determine the current user's Home folder".to_string());
        }
        let password = unsafe { password.assume_init() };
        if password.pw_dir.is_null() {
            return Err("Cannot determine the current user's Home folder".to_string());
        }
        let bytes = unsafe { CStr::from_ptr(password.pw_dir) }.to_bytes();
        let path = PathBuf::from(OsStr::from_bytes(bytes));
        if !path.is_absolute() {
            return Err("The current user's Home folder is invalid".to_string());
        }
        Ok(path)
    }

    /// Whether the saved bookmark still resolves to the Home folder. The app
    /// never reads the folder itself (the backend does), so the access this
    /// check starts is stopped again when it returns.
    pub fn status() -> AgentAccessStatus {
        let Ok(path) = bookmark_path() else {
            return AgentAccessStatus::ReauthorizationRequired;
        };
        if !path.exists() {
            return AgentAccessStatus::AuthorizationRequired;
        }
        match acquire_app() {
            Ok(Some(_access)) => AgentAccessStatus::Authorized,
            Ok(None) => AgentAccessStatus::AuthorizationRequired,
            Err(_) => AgentAccessStatus::ReauthorizationRequired,
        }
    }

    pub fn authorize(selected: &Path) -> Result<AgentAccessStatus, String> {
        let selected = selected
            .canonicalize()
            .map_err(|_| "Select your Home folder to allow Agent configuration".to_string())?;
        let expected = expected_home()?
            .canonicalize()
            .map_err(|_| "Cannot access the current user's Home folder".to_string())?;
        if selected != expected {
            return Err("Select your Home folder, not one of its subfolders".to_string());
        }
        save_app_bookmark(&selected)?;
        Ok(AgentAccessStatus::Authorized)
    }

    fn acquire_app() -> Result<Option<AgentHomeAccess>, String> {
        let Some(bytes) = read_bookmark(&bookmark_path()?)? else {
            return Ok(None);
        };
        let (access, stale) = resolve_bookmark(
            &bytes,
            NSURLBookmarkResolutionOptions::WithSecurityScope
                | NSURLBookmarkResolutionOptions::WithoutUI,
            true,
        )?;
        if stale {
            persist_app_bookmark(&access.url)?;
        }
        Ok(Some(access))
    }

    pub fn acquire_for_service() -> Result<Option<AgentHomeAccess>, String> {
        let service_path = service_bookmark_path()?;
        let Some(bytes) = read_bookmark(&service_path)? else {
            if bookmark_path()?.exists() {
                return Err("Agent Home access was not prepared for the backend".to_string());
            }
            return Ok(None);
        };
        // The implicit scope is conferred on the process resolving the
        // bookmark, which starts (and on drop stops) access like any other
        // security-scoped URL.
        let (access, _) =
            resolve_bookmark(&bytes, NSURLBookmarkResolutionOptions::WithoutUI, false)?;
        Ok(Some(access))
    }

    /// Resolve `bytes` and start accessing the URL. `require_start` is set
    /// for the app's explicit security-scoped bookmark, which grants nothing
    /// unless the start succeeds.
    fn resolve_bookmark(
        bytes: &[u8],
        options: NSURLBookmarkResolutionOptions,
        require_start: bool,
    ) -> Result<(AgentHomeAccess, bool), String> {
        let bookmark = NSData::with_bytes(bytes);
        let mut stale = Bool::NO;
        let url = unsafe {
            NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
                &bookmark, options, None, &mut stale,
            )
        }
        .map_err(|_| "Agent Home access is no longer valid".to_string())?;
        if !url.isFileURL() {
            return Err("Agent Home access is invalid".to_string());
        }
        let home = url
            .path()
            .map(|path| PathBuf::from(path.to_string()))
            .ok_or_else(|| "Agent Home access is invalid".to_string())?;
        let started = unsafe { url.startAccessingSecurityScopedResource() };
        if require_start && !started {
            return Err("Agent Home access is no longer valid".to_string());
        }
        let access = AgentHomeAccess { url, home, started };
        let expected = expected_home()?
            .canonicalize()
            .map_err(|_| "Cannot access the current user's Home folder".to_string())?;
        if access.home().canonicalize().ok().as_ref() != Some(&expected) {
            return Err(
                "Agent Home access does not point to the current user's Home folder".into(),
            );
        }
        Ok((access, stale.as_bool()))
    }

    pub fn prepare_for_service() -> Result<(), String> {
        let access = match acquire_app() {
            Ok(Some(access)) => access,
            Ok(None) => return clear_service_bookmark(),
            Err(error) => {
                let _ = clear_service_bookmark();
                return Err(error);
            }
        };
        let persisted = persist_service_bookmark(&access.url);
        // The app's access was only needed to create the bookmark.
        drop(access);
        if let Err(error) = persisted {
            let _ = clear_service_bookmark();
            return Err(error);
        }
        Ok(())
    }

    fn bookmark_path() -> Result<PathBuf, String> {
        Ok(crate::paths::app_data_dir()?.join(BOOKMARK_FILE))
    }

    fn service_bookmark_path() -> Result<PathBuf, String> {
        Ok(crate::paths::app_data_dir()?.join(SERVICE_BOOKMARK_FILE))
    }

    fn save_app_bookmark(home: &Path) -> Result<(), String> {
        let home = home
            .to_str()
            .ok_or_else(|| "The Home folder path is not valid Unicode".to_string())?;
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(home), true);
        persist_app_bookmark(&url)
    }

    fn persist_app_bookmark(url: &NSURL) -> Result<(), String> {
        let bookmark = url
            .bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
                NSURLBookmarkCreationOptions::WithSecurityScope,
                None,
                None,
            )
            .map_err(|_| "Agent Home access could not be saved".to_string())?;
        write_private(&bookmark_path()?, &bookmark.to_vec())
    }

    fn persist_service_bookmark(url: &NSURL) -> Result<(), String> {
        // A bookmark created without security-scope options carries an
        // implicit ephemeral security scope that "confers access to the
        // resource to any other process that resolves the bookmark" and is
        // "valid until reboot at the latest"
        // (https://developer.apple.com/documentation/foundation/nsurl/bookmarkcreationoptions/withoutimplicitsecurityscope).
        // Apple DTS names this implicit security-scoped bookmark as the way
        // to pass access between processes
        // (https://developer.apple.com/forums/thread/678819). The app
        // regenerates it from its persistent app-scoped bookmark before every
        // backend launch because the implicit scope does not survive a reboot.
        let bookmark = url
            .bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
                NSURLBookmarkCreationOptions::empty(),
                None,
                None,
            )
            .map_err(|_| "Agent Home access could not be shared with the backend".to_string())?;
        write_private(&service_bookmark_path()?, &bookmark.to_vec())
    }

    fn read_bookmark(path: &Path) -> Result<Option<Vec<u8>>, String> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("Agent Home access could not be loaded".to_string()),
        };
        if bytes.is_empty() || bytes.len() as u64 > MAX_BOOKMARK_BYTES {
            return Err("Agent Home access is invalid".to_string());
        }
        Ok(Some(bytes))
    }

    fn clear_service_bookmark() -> Result<(), String> {
        match fs::remove_file(service_bookmark_path()?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("Agent Home backend access could not be cleared".to_string()),
        }
    }

    fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
        let directory = path
            .parent()
            .ok_or_else(|| "The Agent access path has no parent directory".to_string())?;
        crate::private_fs::create_private_dir(directory)
            .map_err(|_| "Agent Home access could not be saved".to_string())?;
        if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
            return Err("Agent Home access could not be saved".to_string());
        }
        crate::private_fs::publish(path, crate::private_fs::Publish::Replace, |file| {
            file.write_all(bytes)
        })
        .map_err(|_| "Agent Home access could not be saved".to_string())
    }
}

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
pub use mac_app_store::AgentHomeAccess;

pub fn status() -> AgentAccessStatus {
    #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
    {
        mac_app_store::status()
    }
    #[cfg(not(all(target_os = "macos", feature = "mac-app-store")))]
    {
        AgentAccessStatus::Authorized
    }
}

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
pub fn expected_home() -> Result<std::path::PathBuf, String> {
    mac_app_store::expected_home()
}

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
pub fn authorize(path: &std::path::Path) -> Result<AgentAccessStatus, String> {
    mac_app_store::authorize(path)
}

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
pub fn acquire_for_service() -> Result<Option<AgentHomeAccess>, String> {
    mac_app_store::acquire_for_service()
}

#[cfg(all(target_os = "macos", feature = "mac-app-store"))]
pub fn prepare_for_service() -> Result<(), String> {
    mac_app_store::prepare_for_service()
}
