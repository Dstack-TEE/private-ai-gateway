use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registration {
    pub executable: PathBuf,
    pub command_path: PathBuf,
    pub installed: bool,
    pub on_path: bool,
}

pub fn status() -> Result<Registration, String> {
    platform::status()
}

pub fn install(directory: Option<PathBuf>) -> Result<Registration, String> {
    platform::install(directory)
}

pub fn uninstall(directory: Option<PathBuf>) -> Result<Registration, String> {
    platform::uninstall(directory)
}

fn current_executable() -> Result<PathBuf, String> {
    let executable = std::env::current_exe().map_err(|_| "Cannot locate pap".to_string())?;
    executable
        .canonicalize()
        .map_err(|_| "Cannot resolve the pap executable path".to_string())
}

#[cfg(unix)]
mod platform {
    use std::env;
    use std::fs::{self, DirBuilder, File};
    use std::io::ErrorKind;
    use std::os::unix::fs::{symlink, DirBuilderExt, MetadataExt, PermissionsExt};
    use std::path::{Component, Path, PathBuf};

    use super::{current_executable, Registration};

    #[derive(Debug)]
    enum CommandState {
        Missing,
        ManagedLink,
        Executable,
    }

    pub(super) fn status() -> Result<Registration, String> {
        let executable = current_executable()?;
        let directory = managed_system_directory(&executable).unwrap_or(default_directory()?);
        inspect(&executable, &directory, env::var_os("PATH").as_deref())
    }

    pub(super) fn install(directory: Option<PathBuf>) -> Result<Registration, String> {
        let executable = current_executable()?;
        if directory.is_none() {
            if let Some(directory) = managed_system_directory(&executable) {
                return inspect(&executable, &directory, env::var_os("PATH").as_deref());
            }
        }
        let directory = prepare_directory(directory)?;
        let command_path = directory.join("pap");

        match command_state(&executable, &command_path)? {
            CommandState::Missing => {
                symlink(&executable, &command_path).map_err(|error| {
                    if error.kind() == ErrorKind::AlreadyExists {
                        "The pap command path changed while it was being registered".to_string()
                    } else if error.kind() == ErrorKind::PermissionDenied {
                        format!(
                            "Cannot register pap in {} without an authorized installer",
                            directory.display()
                        )
                    } else {
                        format!("Cannot register pap in {}", directory.display())
                    }
                })?;
                sync_directory(&directory)?;
            }
            CommandState::ManagedLink | CommandState::Executable => {}
        }

        inspect(&executable, &directory, env::var_os("PATH").as_deref())
    }

    pub(super) fn uninstall(directory: Option<PathBuf>) -> Result<Registration, String> {
        let executable = current_executable()?;
        if directory.is_none() && managed_system_directory(&executable).is_some() {
            return Err(
                "The pap command is owned by a system package; uninstall the package to remove it"
                    .to_string(),
            );
        }
        let directory = resolve_directory(directory)?;
        let command_path = directory.join("pap");

        match command_state(&executable, &command_path)? {
            CommandState::Missing => {}
            CommandState::ManagedLink => {
                fs::remove_file(&command_path)
                    .map_err(|_| format!("Cannot remove {}", command_path.display()))?;
                sync_directory(&directory)?;
            }
            CommandState::Executable => {
                return Err(format!(
                    "{} is the pap executable itself and must be removed by its package or installer",
                    command_path.display()
                ));
            }
        }

        inspect(&executable, &directory, env::var_os("PATH").as_deref())
    }

    fn inspect(
        executable: &Path,
        directory: &Path,
        path_value: Option<&std::ffi::OsStr>,
    ) -> Result<Registration, String> {
        let command_path = directory.join("pap");
        let installed = !matches!(
            command_state(executable, &command_path)?,
            CommandState::Missing
        );
        Ok(Registration {
            executable: executable.to_path_buf(),
            command_path,
            installed,
            on_path: installed && path_contains(directory, path_value),
        })
    }

    fn command_state(executable: &Path, command_path: &Path) -> Result<CommandState, String> {
        let metadata = match fs::symlink_metadata(command_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(CommandState::Missing),
            Err(_) => return Err(format!("Cannot inspect {}", command_path.display())),
        };

        if metadata.file_type().is_symlink() {
            let target = command_path.canonicalize().map_err(|_| {
                format!(
                    "Refusing to replace broken or inaccessible link {}",
                    command_path.display()
                )
            })?;
            return if target == executable {
                Ok(CommandState::ManagedLink)
            } else {
                Err(format!(
                    "Refusing to replace unrelated command {}",
                    command_path.display()
                ))
            };
        }

        let target = command_path
            .canonicalize()
            .map_err(|_| format!("Cannot resolve {}", command_path.display()))?;
        if target == executable {
            Ok(CommandState::Executable)
        } else {
            Err(format!(
                "Refusing to replace unrelated command {}",
                command_path.display()
            ))
        }
    }

    fn prepare_directory(directory: Option<PathBuf>) -> Result<PathBuf, String> {
        if let Some(directory) = directory {
            return validate_custom_directory(directory);
        }

        let directory = default_directory()?;
        if !directory.exists() {
            let mut builder = DirBuilder::new();
            builder.recursive(true).mode(0o755);
            builder
                .create(&directory)
                .map_err(|_| format!("Cannot create {}", directory.display()))?;
        }
        validate_owned_directory(&directory)
    }

    fn resolve_directory(directory: Option<PathBuf>) -> Result<PathBuf, String> {
        match directory {
            Some(directory) => validate_custom_directory(directory),
            None => default_directory(),
        }
    }

    fn validate_custom_directory(directory: PathBuf) -> Result<PathBuf, String> {
        if !directory.is_absolute()
            || directory
                .components()
                .any(|part| part == Component::ParentDir)
        {
            return Err(
                "The pap command directory must be an absolute normalized path".to_string(),
            );
        }
        let directory = validate_owned_directory(&directory)?;
        let home = home_directory()?
            .canonicalize()
            .map_err(|_| "Cannot resolve the current user's home directory".to_string())?;
        if directory.starts_with(&home) || allowed_system_directory(&directory) {
            Ok(directory)
        } else {
            Err("The pap command directory must be inside the current user's home".to_string())
        }
    }

    fn validate_owned_directory(directory: &Path) -> Result<PathBuf, String> {
        let directory = directory
            .canonicalize()
            .map_err(|_| format!("Cannot resolve command directory {}", directory.display()))?;
        let metadata = fs::metadata(&directory)
            .map_err(|_| format!("Cannot inspect command directory {}", directory.display()))?;
        if !metadata.is_dir() {
            return Err(format!("{} is not a directory", directory.display()));
        }
        if metadata.uid() != effective_user_id() {
            return Err(format!(
                "Command directory {} is not owned by the current user",
                directory.display()
            ));
        }
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(format!(
                "Command directory {} is writable by another user",
                directory.display()
            ));
        }
        Ok(directory)
    }

    fn default_directory() -> Result<PathBuf, String> {
        Ok(home_directory()?.join(".local/bin"))
    }

    fn home_directory() -> Result<PathBuf, String> {
        let home = env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| "HOME is not set".to_string())?;
        if !home.is_absolute() {
            return Err("HOME must be an absolute path".to_string());
        }
        Ok(home)
    }

    #[cfg(target_os = "macos")]
    fn allowed_system_directory(directory: &Path) -> bool {
        directory == Path::new("/usr/local/bin")
    }

    #[cfg(not(target_os = "macos"))]
    fn allowed_system_directory(_directory: &Path) -> bool {
        false
    }

    #[cfg(target_os = "macos")]
    fn managed_system_directory(executable: &Path) -> Option<PathBuf> {
        let directory = PathBuf::from("/usr/local/bin");
        match command_state(executable, &directory.join("pap")) {
            Ok(CommandState::ManagedLink | CommandState::Executable) => Some(directory),
            Ok(CommandState::Missing) | Err(_) => None,
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn managed_system_directory(executable: &Path) -> Option<PathBuf> {
        let directory = PathBuf::from("/usr/bin");
        match command_state(executable, &directory.join("pap")) {
            Ok(CommandState::ManagedLink | CommandState::Executable) => Some(directory),
            Ok(CommandState::Missing) | Err(_) => None,
        }
    }

    fn path_contains(directory: &Path, path_value: Option<&std::ffi::OsStr>) -> bool {
        path_value
            .map(env::split_paths)
            .into_iter()
            .flatten()
            .any(|entry| match entry.canonicalize() {
                Ok(entry) => entry == directory,
                Err(_) => entry == directory,
            })
    }

    fn sync_directory(directory: &Path) -> Result<(), String> {
        File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(|_| format!("Cannot persist changes in {}", directory.display()))
    }

    fn effective_user_id() -> u32 {
        unsafe extern "C" {
            fn geteuid() -> u32;
        }
        // SAFETY: geteuid takes no arguments and has no failure mode.
        unsafe { geteuid() }
    }

    #[cfg(test)]
    mod tests {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        use tempfile::tempdir;

        use super::{command_state, inspect, sync_directory, CommandState};

        fn executable(root: &std::path::Path) -> std::path::PathBuf {
            let executable = root.join("runtime/pap");
            fs::create_dir_all(executable.parent().unwrap()).unwrap();
            fs::write(&executable, b"pap").unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            executable.canonicalize().unwrap()
        }

        #[test]
        fn managed_link_round_trip_preserves_the_target() {
            let root = tempdir().unwrap();
            let executable = executable(root.path());
            let directory = root.path().join("bin");
            fs::create_dir(&directory).unwrap();
            std::os::unix::fs::symlink(&executable, directory.join("pap")).unwrap();
            sync_directory(&directory).unwrap();

            let registration = inspect(
                &executable,
                &directory.canonicalize().unwrap(),
                Some(directory.as_os_str()),
            )
            .unwrap();
            assert!(registration.installed);
            assert!(registration.on_path);
            assert!(matches!(
                command_state(&executable, &directory.join("pap")).unwrap(),
                CommandState::ManagedLink
            ));
        }

        #[test]
        fn unrelated_command_is_never_claimed() {
            let root = tempdir().unwrap();
            let executable = executable(root.path());
            let directory = root.path().join("bin");
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join("pap"), b"other").unwrap();

            let error = inspect(&executable, &directory, None).unwrap_err();
            assert!(error.contains("Refusing to replace unrelated command"));
            assert_eq!(fs::read(directory.join("pap")).unwrap(), b"other");
        }

        #[test]
        fn broken_link_is_never_replaced() {
            let root = tempdir().unwrap();
            let executable = executable(root.path());
            let command = root.path().join("pap");
            std::os::unix::fs::symlink(root.path().join("missing"), &command).unwrap();

            let error = command_state(&executable, &command).unwrap_err();
            assert!(error.contains("broken or inaccessible link"));
            assert!(fs::symlink_metadata(command)
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::env;
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};
    use std::ptr;

    use windows_sys::Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS},
        System::{
            Environment::ExpandEnvironmentStringsW,
            Registry::{
                RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
                RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE,
                REG_EXPAND_SZ, REG_SZ,
            },
        },
        UI::WindowsAndMessaging::{
            SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
        },
    };

    use super::{current_executable, Registration};

    const ENVIRONMENT_KEY: &str = "Environment";
    const PATH_VALUE: &str = "Path";
    const OWNERSHIP_KEY: &str = r"Software\Private AI Proxy\CLI";
    const OWNERSHIP_VALUE: &str = "OwnedPath";

    pub(super) fn status() -> Result<Registration, String> {
        let executable = current_executable()?;
        registration(executable, None)
    }

    pub(super) fn install(directory: Option<PathBuf>) -> Result<Registration, String> {
        let executable = current_executable()?;
        let directory = executable_directory(&executable, directory)?;
        reject_path_conflict(&executable)?;
        let (path_value, value_type) = read_user_path()?;
        if path_entries(&path_value)
            .iter()
            .any(|entry| same_path_text(entry, &directory))
        {
            return registration(executable, Some(directory));
        }

        if let Some(owner) = read_ownership()? {
            if !same_path_text(&owner, &directory) {
                return Err(format!(
                    "Another pap installation owns the PATH registration at {owner}"
                ));
            }
        }

        let directory_text = path_text(&directory)?;
        let updated = if path_value.is_empty() {
            directory_text.clone()
        } else if path_value.ends_with(';') {
            format!("{path_value}{directory_text}")
        } else {
            format!("{path_value};{directory_text}")
        };
        write_user_path(&updated, value_type)?;
        if let Err(error) = write_ownership(&directory_text) {
            write_user_path(&path_value, value_type).map_err(|_| {
                "Cannot record or roll back pap PATH registration ownership".to_string()
            })?;
            return Err(error);
        }
        broadcast_environment_change();
        registration(executable, Some(directory))
    }

    pub(super) fn uninstall(directory: Option<PathBuf>) -> Result<Registration, String> {
        let executable = current_executable()?;
        let directory = executable_directory(&executable, directory)?;
        let Some(owner) = read_ownership()? else {
            return registration(executable, Some(directory));
        };
        if !same_path_text(&owner, &directory) {
            return registration(executable, Some(directory));
        }

        let (path_value, value_type) = read_user_path()?;
        let mut entries = path_entries(&path_value);
        if let Some(index) = entries
            .iter()
            .rposition(|entry| same_path_text(entry, &directory))
        {
            entries.remove(index);
            let updated = entries.join(";");
            write_user_path(&updated, value_type)?;
            if let Err(error) = delete_ownership() {
                write_user_path(&path_value, value_type).map_err(|_| {
                    "Cannot remove or roll back pap PATH registration ownership".to_string()
                })?;
                return Err(error);
            }
        } else {
            delete_ownership()?;
        }
        broadcast_environment_change();
        registration(executable, Some(directory))
    }

    fn registration(
        executable: PathBuf,
        directory: Option<PathBuf>,
    ) -> Result<Registration, String> {
        let directory = executable_directory(&executable, directory)?;
        let command_path = directory.join("pap.exe");
        let (path_value, _) = read_user_path()?;
        let installed = path_entries(&path_value)
            .iter()
            .any(|entry| same_path_text(entry, &directory));
        Ok(Registration {
            executable: PathBuf::from(path_text(&executable)?),
            command_path: PathBuf::from(path_text(&command_path)?),
            installed,
            on_path: installed && resolves_from_process_path(&executable),
        })
    }

    fn executable_directory(
        executable: &Path,
        requested: Option<PathBuf>,
    ) -> Result<PathBuf, String> {
        let actual = executable
            .parent()
            .ok_or_else(|| "Cannot locate the pap executable directory".to_string())?
            .to_path_buf();
        let Some(requested) = requested else {
            return Ok(actual);
        };
        let requested = requested
            .canonicalize()
            .map_err(|_| "Cannot resolve the requested pap command directory".to_string())?;
        let candidate = requested.join("pap.exe");
        let candidate = candidate.canonicalize().map_err(|_| {
            "The requested command directory does not contain this pap executable".to_string()
        })?;
        if candidate != executable {
            return Err(
                "The requested command directory contains a different pap executable".to_string(),
            );
        }
        Ok(requested)
    }

    fn reject_path_conflict(executable: &Path) -> Result<(), String> {
        if let Some(found) = first_process_path_command("pap.exe") {
            let found = found
                .canonicalize()
                .map_err(|_| "Cannot resolve the pap command already on PATH".to_string())?;
            if found != executable {
                return Err(format!(
                    "A different pap executable is already on PATH at {}",
                    found.display()
                ));
            }
        }
        Ok(())
    }

    fn resolves_from_process_path(executable: &Path) -> bool {
        first_process_path_command("pap.exe")
            .and_then(|path| path.canonicalize().ok())
            .is_some_and(|path| path == executable)
    }

    fn first_process_path_command(name: &str) -> Option<PathBuf> {
        env::var_os("PATH")
            .into_iter()
            .flat_map(|value| env::split_paths(&value).collect::<Vec<_>>())
            .map(|directory| directory.join(name))
            .find(|candidate| fs::metadata(candidate).is_ok_and(|metadata| metadata.is_file()))
    }

    fn read_user_path() -> Result<(String, u32), String> {
        Ok(read_registry_string(ENVIRONMENT_KEY, PATH_VALUE, false)?
            .unwrap_or_else(|| (String::new(), REG_EXPAND_SZ)))
    }

    fn read_ownership() -> Result<Option<String>, String> {
        Ok(read_registry_string(OWNERSHIP_KEY, OWNERSHIP_VALUE, true)?.map(|(value, _)| value))
    }

    fn read_registry_string(
        sub_key: &str,
        value_name: &str,
        missing_key_allowed: bool,
    ) -> Result<Option<(String, u32)>, String> {
        let Some(key) = open_registry_key(sub_key, KEY_QUERY_VALUE, missing_key_allowed)? else {
            return Ok(None);
        };
        let value_name = wide(value_name);
        let mut value_type = REG_EXPAND_SZ;
        let mut bytes = 0_u32;
        // SAFETY: the key and output pointers are valid; a null data pointer queries size.
        let queried = unsafe {
            RegQueryValueExW(
                key.0,
                value_name.as_ptr(),
                ptr::null_mut(),
                &mut value_type,
                ptr::null_mut(),
                &mut bytes,
            )
        };
        if queried == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if queried != ERROR_SUCCESS && queried != ERROR_MORE_DATA {
            return Err("Cannot read pap registration settings".to_string());
        }
        if value_type != REG_SZ && value_type != REG_EXPAND_SZ {
            return Err("A pap registration setting has an unsupported registry type".to_string());
        }
        let mut buffer = vec![0_u16; (bytes as usize).div_ceil(2).max(1)];
        // SAFETY: the buffer has the byte capacity reported by the first query.
        let queried = unsafe {
            RegQueryValueExW(
                key.0,
                value_name.as_ptr(),
                ptr::null_mut(),
                &mut value_type,
                buffer.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if queried != ERROR_SUCCESS {
            return Err("Cannot read pap registration settings".to_string());
        }
        let length = buffer
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(buffer.len());
        Ok(Some((
            String::from_utf16_lossy(&buffer[..length]),
            value_type,
        )))
    }

    fn write_user_path(value: &str, value_type: u32) -> Result<(), String> {
        write_registry_string(ENVIRONMENT_KEY, PATH_VALUE, value, value_type, false)
            .map_err(|_| "Cannot update the current user's PATH".to_string())
    }

    fn write_ownership(value: &str) -> Result<(), String> {
        write_registry_string(OWNERSHIP_KEY, OWNERSHIP_VALUE, value, REG_SZ, true)
            .map_err(|_| "Cannot record pap PATH registration ownership".to_string())
    }

    fn write_registry_string(
        sub_key: &str,
        value_name: &str,
        value: &str,
        value_type: u32,
        create: bool,
    ) -> Result<(), String> {
        let key = if create {
            create_registry_key(sub_key)?
        } else {
            open_registry_key(sub_key, KEY_QUERY_VALUE | KEY_SET_VALUE, false)?
                .ok_or_else(|| "Registry key is missing".to_string())?
        };
        let value_name = wide(value_name);
        let data = wide(value);
        let byte_length = data
            .len()
            .checked_mul(2)
            .and_then(|length| u32::try_from(length).ok())
            .ok_or_else(|| "The current user's PATH is too large".to_string())?;
        // SAFETY: the UTF-16 buffer includes its terminator and remains valid for the call.
        let written = unsafe {
            RegSetValueExW(
                key.0,
                value_name.as_ptr(),
                0,
                value_type,
                data.as_ptr().cast(),
                byte_length,
            )
        };
        if written != ERROR_SUCCESS {
            return Err("Cannot update pap registration settings".to_string());
        }
        Ok(())
    }

    fn delete_ownership() -> Result<(), String> {
        let Some(key) = open_registry_key(OWNERSHIP_KEY, KEY_SET_VALUE, true)? else {
            return Ok(());
        };
        let value_name = wide(OWNERSHIP_VALUE);
        // SAFETY: the key and null-terminated value name remain valid for the call.
        let deleted = unsafe { RegDeleteValueW(key.0, value_name.as_ptr()) };
        if deleted != ERROR_SUCCESS && deleted != ERROR_FILE_NOT_FOUND {
            return Err("Cannot remove pap PATH registration ownership".to_string());
        }
        Ok(())
    }

    fn open_registry_key(
        sub_key: &str,
        access: u32,
        missing_allowed: bool,
    ) -> Result<Option<RegistryKey>, String> {
        let sub_key = wide(sub_key);
        let mut key: HKEY = ptr::null_mut();
        // SAFETY: pointers are valid for the duration of the Windows registry call.
        let opened =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub_key.as_ptr(), 0, access, &mut key) };
        if opened == ERROR_FILE_NOT_FOUND && missing_allowed {
            return Ok(None);
        }
        if opened != ERROR_SUCCESS {
            return Err("Cannot open pap registration settings".to_string());
        }
        Ok(Some(RegistryKey(key)))
    }

    fn create_registry_key(sub_key: &str) -> Result<RegistryKey, String> {
        let sub_key = wide(sub_key);
        let mut key: HKEY = ptr::null_mut();
        // SAFETY: pointers are valid and optional class/security/disposition parameters are null.
        let created = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                sub_key.as_ptr(),
                0,
                ptr::null(),
                0,
                KEY_QUERY_VALUE | KEY_SET_VALUE,
                ptr::null(),
                &mut key,
                ptr::null_mut(),
            )
        };
        if created != ERROR_SUCCESS {
            return Err("Cannot create pap registration settings".to_string());
        }
        Ok(RegistryKey(key))
    }

    fn path_entries(value: &str) -> Vec<String> {
        value.split(';').map(ToOwned::to_owned).collect()
    }

    fn same_path_text(entry: &str, directory: &Path) -> bool {
        normalize_path_text(&expand_environment(entry))
            == normalize_path_text(&path_text_lossy(directory))
    }

    fn normalize_path_text(value: &str) -> String {
        let value = value.trim().trim_matches('"').replace('/', "\\");
        let value = match value.strip_prefix(r"\\?\UNC\") {
            Some(unc) => format!(r"\\{unc}"),
            None => value.strip_prefix(r"\\?\").unwrap_or(&value).to_string(),
        };
        value.trim_end_matches('\\').to_lowercase()
    }

    fn expand_environment(value: &str) -> String {
        let source = wide(value);
        // SAFETY: the source is a valid null-terminated UTF-16 string.
        let required = unsafe { ExpandEnvironmentStringsW(source.as_ptr(), ptr::null_mut(), 0) };
        if required == 0 {
            return value.to_string();
        }
        let mut expanded = vec![0_u16; required as usize];
        // SAFETY: the destination buffer has the size returned by the first call.
        let written =
            unsafe { ExpandEnvironmentStringsW(source.as_ptr(), expanded.as_mut_ptr(), required) };
        if written == 0 || written > required {
            return value.to_string();
        }
        let length = expanded
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(expanded.len());
        OsString::from_wide(&expanded[..length])
            .to_string_lossy()
            .into_owned()
    }

    fn path_text(path: &Path) -> Result<String, String> {
        let value = path
            .to_str()
            .ok_or("The pap executable path is not valid Unicode")?;
        let value = match value.strip_prefix(r"\\?\UNC\") {
            Some(unc) => format!(r"\\{unc}"),
            None => value.strip_prefix(r"\\?\").unwrap_or(value).to_string(),
        };
        if value.contains(';') {
            return Err("The pap executable directory cannot contain a semicolon".to_string());
        }
        Ok(value)
    }

    fn path_text_lossy(path: &Path) -> String {
        path.as_os_str().to_string_lossy().into_owned()
    }

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain(Some(0)).collect()
    }

    fn broadcast_environment_change() {
        let environment = wide("Environment");
        let mut result = 0_usize;
        // SAFETY: HWND_BROADCAST and WM_SETTINGCHANGE follow the documented environment update contract.
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                environment.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                5_000,
                &mut result,
            );
        }
    }

    struct RegistryKey(HKEY);

    impl Drop for RegistryKey {
        fn drop(&mut self) {
            // SAFETY: the handle was returned by RegOpenKeyExW and is closed once here.
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{normalize_path_text, path_entries, path_text};

        #[test]
        fn path_entries_are_compared_without_case_or_separator_noise() {
            assert_eq!(
                normalize_path_text(r#""C:/Users/Alice/PAP/""#),
                normalize_path_text(r"c:\users\alice\pap")
            );
            assert_eq!(
                path_text(std::path::Path::new(r"\\?\D:\Tools\pap.exe")).unwrap(),
                r"D:\Tools\pap.exe"
            );
            assert_eq!(
                path_text(std::path::Path::new(r"\\?\UNC\server\tools\pap.exe")).unwrap(),
                r"\\server\tools\pap.exe"
            );
            assert_eq!(
                normalize_path_text(r"\\?\UNC\server\tools"),
                normalize_path_text(r"\\server\tools")
            );
        }

        #[test]
        fn path_split_preserves_unowned_entries_exactly() {
            assert_eq!(
                path_entries(r" C:\One ; ;%LOCALAPPDATA%\Two "),
                [r" C:\One ", " ", r"%LOCALAPPDATA%\Two "]
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::Registration;

    #[test]
    fn registration_uses_the_cli_json_contract() {
        let value = serde_json::to_value(Registration {
            executable: PathBuf::from("/opt/pap/pap"),
            command_path: PathBuf::from("/home/user/.local/bin/pap"),
            installed: true,
            on_path: false,
        })
        .unwrap();
        assert_eq!(value["commandPath"], "/home/user/.local/bin/pap");
        assert_eq!(value["installed"], true);
        assert_eq!(value["onPath"], false);
        assert!(value.get("command_path").is_none());
    }
}
