use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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
        fs::{self, File},
        io::Write,
        os::unix::{ffi::OsStrExt, fs::PermissionsExt},
        path::{Path, PathBuf},
        sync::{Mutex, OnceLock},
    };

    const BOOKMARK_FILE: &str = "agent-home.bookmark";
    const MAX_BOOKMARK_BYTES: u64 = 1024 * 1024;

    static ACTIVE_ACCESS: OnceLock<Mutex<Option<AgentHomeAccess>>> = OnceLock::new();

    pub struct AgentHomeAccess {
        url: Retained<NSURL>,
        home: PathBuf,
    }

    impl AgentHomeAccess {
        pub fn home(&self) -> &Path {
            &self.home
        }
    }

    impl Drop for AgentHomeAccess {
        fn drop(&mut self) {
            unsafe { self.url.stopAccessingSecurityScopedResource() };
        }
    }

    fn active_access() -> &'static Mutex<Option<AgentHomeAccess>> {
        ACTIVE_ACCESS.get_or_init(|| Mutex::new(None))
    }

    fn retain(access: Option<AgentHomeAccess>) -> bool {
        let Ok(mut active) = active_access().lock() else {
            return false;
        };
        *active = access;
        true
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

    pub fn status() -> AgentAccessStatus {
        let path = match bookmark_path() {
            Ok(path) => path,
            Err(_) => {
                retain(None);
                return AgentAccessStatus::ReauthorizationRequired;
            }
        };
        if !path.exists() {
            retain(None);
            return AgentAccessStatus::AuthorizationRequired;
        }
        match acquire() {
            Ok(Some(access)) => {
                if retain(Some(access)) {
                    AgentAccessStatus::Authorized
                } else {
                    AgentAccessStatus::ReauthorizationRequired
                }
            }
            Ok(None) => {
                retain(None);
                AgentAccessStatus::AuthorizationRequired
            }
            Err(_) => {
                retain(None);
                AgentAccessStatus::ReauthorizationRequired
            }
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
        save_bookmark(&selected)?;
        Ok(AgentAccessStatus::Authorized)
    }

    pub fn acquire() -> Result<Option<AgentHomeAccess>, String> {
        let path = bookmark_path()?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("Agent Home access could not be loaded".to_string()),
        };
        if bytes.is_empty() || bytes.len() as u64 > MAX_BOOKMARK_BYTES {
            return Err("Agent Home access is invalid".to_string());
        }
        let bookmark = NSData::with_bytes(&bytes);
        let mut stale = Bool::NO;
        let url = unsafe {
            NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
                &bookmark,
                NSURLBookmarkResolutionOptions::WithSecurityScope
                    | NSURLBookmarkResolutionOptions::WithoutUI,
                None,
                &mut stale,
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
        if !unsafe { url.startAccessingSecurityScopedResource() } {
            return Err("Agent Home access is no longer valid".to_string());
        }
        let access = AgentHomeAccess { url, home };
        let expected = expected_home()?
            .canonicalize()
            .map_err(|_| "Cannot access the current user's Home folder".to_string())?;
        if access.home().canonicalize().ok().as_ref() != Some(&expected) {
            return Err(
                "Agent Home access does not point to the current user's Home folder".into(),
            );
        }
        if stale.as_bool() {
            persist_bookmark(&access.url)?;
        }
        Ok(Some(access))
    }

    pub fn authorized_home() -> Result<PathBuf, String> {
        if status() != AgentAccessStatus::Authorized {
            return Err("Agent Home access is required".to_string());
        }
        active_access()
            .lock()
            .map_err(|_| "Agent Home access state is unavailable".to_string())?
            .as_ref()
            .map(|access| access.home().to_path_buf())
            .ok_or_else(|| "Agent Home access is unavailable".to_string())
    }

    fn bookmark_path() -> Result<PathBuf, String> {
        Ok(desktop_gateway::agents::app_data_dir()?.join(BOOKMARK_FILE))
    }

    fn save_bookmark(home: &Path) -> Result<(), String> {
        let home = home
            .to_str()
            .ok_or_else(|| "The Home folder path is not valid Unicode".to_string())?;
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(home), true);
        persist_bookmark(&url)
    }

    fn persist_bookmark(url: &NSURL) -> Result<(), String> {
        let bookmark = url
            .bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
                NSURLBookmarkCreationOptions::WithSecurityScope,
                None,
                None,
            )
            .map_err(|_| "Agent Home access could not be saved".to_string())?;
        write_private(&bookmark_path()?, &bookmark.to_vec())
    }

    fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
        let directory = path
            .parent()
            .ok_or_else(|| "The Agent access path has no parent directory".to_string())?;
        desktop_gateway::tokens::create_private_dir(directory)
            .map_err(|_| "Agent Home access could not be saved".to_string())?;
        if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
            return Err("Agent Home access could not be saved".to_string());
        }
        let mut temporary = tempfile::NamedTempFile::new_in(directory)
            .map_err(|_| "Agent Home access could not be saved".to_string())?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .and_then(|()| temporary.write_all(bytes))
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|_| "Agent Home access could not be saved".to_string())?;
        temporary
            .into_temp_path()
            .persist(path)
            .map_err(|_| "Agent Home access could not be saved".to_string())?;
        File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| "Agent Home access could not be saved".to_string())?;
        Ok(())
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
pub fn acquire() -> Result<Option<AgentHomeAccess>, String> {
    mac_app_store::acquire()
}

pub fn authorized_home() -> Result<std::path::PathBuf, String> {
    #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
    {
        mac_app_store::authorized_home()
    }
    #[cfg(not(all(target_os = "macos", feature = "mac-app-store")))]
    {
        Err("Agent Home access is unavailable on this distribution".to_string())
    }
}
