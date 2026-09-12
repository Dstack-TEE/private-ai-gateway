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
    let executable =
        std::env::current_exe().map_err(|_| "Cannot locate private-ai-proxy".to_string())?;
    let executable = executable
        .canonicalize()
        .map_err(|_| "Cannot resolve the private-ai-proxy executable path".to_string())?;
    Ok(executable)
}

#[cfg(any(windows, test))]
mod windows_alias {
    use std::fs::{self, OpenOptions};
    use std::io::{self, Write};
    use std::path::Path;

    const SCRIPT: &[u8] = b"@echo off\r\n\"%~dp0private-ai-proxy.exe\" %*\r\n";

    pub(super) fn install(executable: &Path) -> Result<(), String> {
        reject_legacy_executable(executable)?;
        let alias = executable.with_file_name("pap.cmd");
        match OpenOptions::new().write(true).create_new(true).open(&alias) {
            Ok(mut file) => {
                let written = file.write_all(SCRIPT).and_then(|()| file.sync_all());
                drop(file);
                if written.is_err() {
                    fs::remove_file(&alias)
                        .map_err(|_| "Cannot clean up the incomplete pap.cmd alias".to_string())?;
                    return Err("Cannot write the pap.cmd alias".to_string());
                }
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if matches(&alias)? {
                    Ok(())
                } else {
                    Err("Refusing to replace an unrelated pap.cmd".to_string())
                }
            }
            Err(_) => Err("Cannot create the pap.cmd alias".to_string()),
        }
    }

    pub(super) fn uninstall(executable: &Path) -> Result<(), String> {
        let alias = executable.with_file_name("pap.cmd");
        match fs::symlink_metadata(&alias) {
            Ok(_) if matches(&alias)? => {
                fs::remove_file(alias).map_err(|_| "Cannot remove the pap.cmd alias".to_string())
            }
            Ok(_) => Err("Refusing to remove an unrelated pap.cmd".to_string()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("Cannot inspect the pap.cmd alias".to_string()),
        }
    }

    pub(super) fn reject_legacy_executable(executable: &Path) -> Result<(), String> {
        match fs::symlink_metadata(executable.with_file_name("pap.exe")) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err("A legacy pap.exe shadows the pap alias; remove the old installation or extract this release into a clean directory".to_string()),
            Err(_) => Err("Cannot inspect the legacy pap.exe command".to_string()),
        }
    }

    pub(super) fn matches(alias: &Path) -> Result<bool, String> {
        fs::read(alias)
            .map(|bytes| bytes == SCRIPT)
            .map_err(|_| "Cannot verify the pap.cmd alias".to_string())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn registration_accepts_only_the_matching_alias() {
            let root = tempfile::tempdir().unwrap();
            let executable = root.path().join("private-ai-proxy.exe");
            let alias = root.path().join("pap.cmd");
            fs::write(&executable, b"cli").unwrap();
            install(&executable).unwrap();
            install(&executable).unwrap();
            assert_eq!(fs::read(&alias).unwrap(), SCRIPT);
            uninstall(&executable).unwrap();
            assert!(!alias.exists());
            assert!(executable.exists());
            fs::write(&alias, b"other").unwrap();
            assert!(install(&executable).is_err());
            assert!(uninstall(&executable).is_err());
            assert_eq!(fs::read(alias).unwrap(), b"other");
            let legacy = executable.with_file_name("pap.exe");
            fs::write(&legacy, b"old cli").unwrap();
            assert!(reject_legacy_executable(&executable).is_err());
            assert!(install(&executable).is_err());
            assert_eq!(fs::read(legacy).unwrap(), b"old cli");
        }
    }
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
        LegacyLink(PathBuf),
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
        register_commands(&executable, &directory)?;

        inspect(&executable, &directory, env::var_os("PATH").as_deref())
    }

    pub(super) fn uninstall(directory: Option<PathBuf>) -> Result<Registration, String> {
        let executable = current_executable()?;
        if directory.is_none() && managed_system_directory(&executable).is_some() {
            return Err(
                "The private-ai-proxy command is owned by a system package; uninstall the package to remove it"
                    .to_string(),
            );
        }
        let directory = resolve_directory(directory)?;
        let commands = command_states(&executable, &directory)?;
        for (path, state) in &commands {
            if matches!(state, CommandState::Executable) {
                return Err(format!(
                    "{} is the executable itself and must be removed by its package or installer",
                    path.display()
                ));
            }
        }
        for (path, state) in commands {
            if matches!(
                state,
                CommandState::ManagedLink | CommandState::LegacyLink(_)
            ) {
                fs::remove_file(&path).map_err(|_| format!("Cannot remove {}", path.display()))?;
            }
        }
        if directory.exists() {
            sync_directory(&directory)?;
        }

        inspect(&executable, &directory, env::var_os("PATH").as_deref())
    }

    fn inspect(
        executable: &Path,
        directory: &Path,
        path_value: Option<&std::ffi::OsStr>,
    ) -> Result<Registration, String> {
        let command_path = directory.join("private-ai-proxy");
        let installed = command_states(executable, directory)?
            .iter()
            .all(|(_, state)| {
                matches!(state, CommandState::ManagedLink | CommandState::Executable)
            });
        Ok(Registration {
            executable: executable.to_path_buf(),
            command_path,
            installed,
            on_path: installed && path_contains(directory, path_value),
        })
    }

    fn command_states(
        executable: &Path,
        directory: &Path,
    ) -> Result<Vec<(PathBuf, CommandState)>, String> {
        ["private-ai-proxy", "pap"]
            .into_iter()
            .map(|name| {
                let path = directory.join(name);
                command_state(executable, &path).map(|state| (path, state))
            })
            .collect()
    }

    fn register_commands(executable: &Path, directory: &Path) -> Result<(), String> {
        // Check both names before changing either, including an existing legacy pap link.
        let commands = command_states(executable, directory)?;
        let mut created: Vec<PathBuf> = Vec::new();
        for (path, state) in commands {
            let previous = match state {
                CommandState::Missing => None,
                CommandState::LegacyLink(target) => {
                    fs::remove_file(&path)
                        .map_err(|_| format!("Cannot update {}", path.display()))?;
                    Some(target)
                }
                CommandState::ManagedLink | CommandState::Executable => continue,
            };
            if symlink(executable, &path).is_err() {
                if let Some(target) = previous {
                    symlink(target, &path)
                        .map_err(|_| format!("Cannot restore {}", path.display()))?;
                }
                for path in created {
                    fs::remove_file(&path).map_err(|_| {
                        format!("Cannot roll back registration of {}", path.display())
                    })?;
                }
                return Err(format!("Cannot register {}", path.display()));
            }
            created.push(path);
        }
        sync_directory(directory)
    }

    fn command_state(executable: &Path, command_path: &Path) -> Result<CommandState, String> {
        let metadata = match fs::symlink_metadata(command_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(CommandState::Missing),
            Err(_) => return Err(format!("Cannot inspect {}", command_path.display())),
        };

        if metadata.file_type().is_symlink() {
            // A renamed bundle leaves the old pap link dangling beside its replacement.
            // Only that exact missing sibling is ours; arbitrary broken links stay untouched.
            if command_path.file_name().is_some_and(|name| name == "pap") {
                let target = fs::read_link(command_path)
                    .map_err(|_| format!("Cannot read {}", command_path.display()))?;
                let resolved = if target.is_absolute() {
                    target.clone()
                } else {
                    command_path
                        .parent()
                        .unwrap_or(Path::new("."))
                        .join(&target)
                };
                if resolved.file_name().is_some_and(|name| name == "pap")
                    && resolved
                        .parent()
                        .and_then(|parent| parent.canonicalize().ok())
                        == executable.parent().map(Path::to_path_buf)
                    && fs::symlink_metadata(&resolved)
                        .is_err_and(|error| error.kind() == ErrorKind::NotFound)
                {
                    return Ok(CommandState::LegacyLink(target));
                }
            }
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
        #[cfg(target_os = "linux")]
        if executable == Path::new("/usr/bin/private-ai-proxy")
            && target == Path::new("/usr/bin/pap")
            && metadata.permissions().mode() & 0o111 != 0
            && fs::read(&target)
                .is_ok_and(|bytes| bytes == b"#!/bin/sh\nexec /usr/bin/private-ai-proxy \"$@\"\n")
        {
            return Ok(CommandState::Executable);
        }
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
                "The private-ai-proxy command directory must be an absolute normalized path"
                    .to_string(),
            );
        }
        let directory = validate_owned_directory(&directory)?;
        let home = home_directory()?
            .canonicalize()
            .map_err(|_| "Cannot resolve the current user's home directory".to_string())?;
        if directory.starts_with(&home) || allowed_system_directory(&directory) {
            Ok(directory)
        } else {
            Err(
                "The private-ai-proxy command directory must be inside the current user's home"
                    .to_string(),
            )
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
        match command_state(executable, &directory.join("private-ai-proxy")) {
            Ok(CommandState::ManagedLink | CommandState::Executable) => Some(directory),
            Ok(CommandState::Missing | CommandState::LegacyLink(_)) | Err(_) => None,
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn managed_system_directory(executable: &Path) -> Option<PathBuf> {
        let directory = PathBuf::from("/usr/bin");
        match command_state(executable, &directory.join("private-ai-proxy")) {
            Ok(CommandState::ManagedLink | CommandState::Executable) => Some(directory),
            Ok(CommandState::Missing | CommandState::LegacyLink(_)) | Err(_) => None,
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

        use super::{command_state, inspect, register_commands, CommandState};

        fn executable(root: &std::path::Path) -> std::path::PathBuf {
            let executable = root.join("runtime/private-ai-proxy");
            fs::create_dir_all(executable.parent().unwrap()).unwrap();
            fs::write(&executable, b"private-ai-proxy").unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            executable.canonicalize().unwrap()
        }

        #[test]
        fn managed_link_round_trip_preserves_the_target() {
            let root = tempdir().unwrap();
            let executable = executable(root.path());
            let directory = root.path().join("bin");
            fs::create_dir(&directory).unwrap();
            std::os::unix::fs::symlink(executable.with_file_name("pap"), directory.join("pap"))
                .unwrap();
            register_commands(&executable, &directory).unwrap();
            register_commands(&executable, &directory).unwrap();

            let registration = inspect(
                &executable,
                &directory.canonicalize().unwrap(),
                Some(directory.as_os_str()),
            )
            .unwrap();
            assert!(registration.installed);
            assert!(registration.on_path);
            assert_eq!(
                registration.command_path,
                directory.canonicalize().unwrap().join("private-ai-proxy")
            );
            assert_eq!(directory.join("pap").canonicalize().unwrap(), executable);
            assert!(matches!(
                command_state(&executable, &directory.join("private-ai-proxy")).unwrap(),
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

            let error = register_commands(&executable, &directory).unwrap_err();
            assert!(error.contains("Refusing to replace unrelated command"));
            assert_eq!(fs::read(directory.join("pap")).unwrap(), b"other");
            assert!(!directory.join("private-ai-proxy").exists());
        }

        #[test]
        fn broken_link_is_never_replaced() {
            let root = tempdir().unwrap();
            let executable = executable(root.path());
            let command = root.path().join("private-ai-proxy");
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

    use super::{current_executable, windows_alias, Registration};

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
        windows_alias::install(&executable)?;
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
                    "Another private-ai-proxy installation owns the PATH registration at {owner}"
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
                "Cannot record or roll back private-ai-proxy PATH registration ownership"
                    .to_string()
            })?;
            return Err(error);
        }
        broadcast_environment_change();
        registration(executable, Some(directory))
    }

    pub(super) fn uninstall(directory: Option<PathBuf>) -> Result<Registration, String> {
        let executable = current_executable()?;
        let directory = executable_directory(&executable, directory)?;
        windows_alias::uninstall(&executable)?;
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
                    "Cannot remove or roll back private-ai-proxy PATH registration ownership"
                        .to_string()
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
        let command_path = directory.join("private-ai-proxy.exe");
        windows_alias::reject_legacy_executable(&executable)?;
        let (path_value, _) = read_user_path()?;
        let installed = path_entries(&path_value)
            .iter()
            .any(|entry| same_path_text(entry, &directory))
            && windows_alias::matches(&directory.join("pap.cmd")).unwrap_or(false);
        Ok(Registration {
            executable: PathBuf::from(path_text(&executable)?),
            command_path: PathBuf::from(path_text(&command_path)?),
            installed,
            on_path: installed
                && resolves_from_process_path(&executable)
                && reject_path_conflict(&executable).is_ok(),
        })
    }

    fn executable_directory(
        executable: &Path,
        requested: Option<PathBuf>,
    ) -> Result<PathBuf, String> {
        let actual = executable
            .parent()
            .ok_or_else(|| "Cannot locate the private-ai-proxy executable directory".to_string())?
            .to_path_buf();
        let Some(requested) = requested else {
            return Ok(actual);
        };
        let requested = requested.canonicalize().map_err(|_| {
            "Cannot resolve the requested private-ai-proxy command directory".to_string()
        })?;
        let candidate = requested.join("private-ai-proxy.exe");
        let candidate = candidate.canonicalize().map_err(|_| {
            "The requested command directory does not contain this private-ai-proxy executable"
                .to_string()
        })?;
        if candidate != executable {
            return Err(
                "The requested command directory contains a different private-ai-proxy executable"
                    .to_string(),
            );
        }
        Ok(requested)
    }

    fn reject_path_conflict(executable: &Path) -> Result<(), String> {
        if let Some(found) = first_process_path_command("private-ai-proxy.exe") {
            let found = found.canonicalize().map_err(|_| {
                "Cannot resolve the private-ai-proxy command already on PATH".to_string()
            })?;
            if found != executable {
                return Err(format!(
                    "A different private-ai-proxy executable is already on PATH at {}",
                    found.display()
                ));
            }
        }
        if let Some(found) = first_process_path_command("pap.exe") {
            if found.parent().and_then(|path| path.canonicalize().ok())
                != executable.parent().map(Path::to_path_buf)
            {
                return Err(format!(
                    "An unrelated pap.exe is already on PATH at {}",
                    found.display()
                ));
            }
        }
        if let Some(found) = first_process_path_command("pap.cmd") {
            if found.parent().and_then(|path| path.canonicalize().ok())
                != executable.parent().map(Path::to_path_buf)
                || !windows_alias::matches(&found)?
            {
                return Err(format!(
                    "An unrelated pap.cmd is already on PATH at {}",
                    found.display()
                ));
            }
        }
        Ok(())
    }

    fn resolves_from_process_path(executable: &Path) -> bool {
        first_process_path_command("private-ai-proxy.exe")
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
            return Err("Cannot read private-ai-proxy registration settings".to_string());
        }
        if value_type != REG_SZ && value_type != REG_EXPAND_SZ {
            return Err(
                "A private-ai-proxy registration setting has an unsupported registry type"
                    .to_string(),
            );
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
            return Err("Cannot read private-ai-proxy registration settings".to_string());
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
            .map_err(|_| "Cannot record private-ai-proxy PATH registration ownership".to_string())
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
            return Err("Cannot update private-ai-proxy registration settings".to_string());
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
            return Err("Cannot remove private-ai-proxy PATH registration ownership".to_string());
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
            return Err("Cannot open private-ai-proxy registration settings".to_string());
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
            return Err("Cannot create private-ai-proxy registration settings".to_string());
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
            .ok_or("The private-ai-proxy executable path is not valid Unicode")?;
        let value = match value.strip_prefix(r"\\?\UNC\") {
            Some(unc) => format!(r"\\{unc}"),
            None => value.strip_prefix(r"\\?\").unwrap_or(value).to_string(),
        };
        if value.contains(';') {
            return Err(
                "The private-ai-proxy executable directory cannot contain a semicolon".to_string(),
            );
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
                normalize_path_text(r#""C:/Users/Alice/Private-AI-Proxy/""#),
                normalize_path_text(r"c:\users\alice\private-ai-proxy")
            );
            assert_eq!(
                path_text(std::path::Path::new(r"\\?\D:\Tools\private-ai-proxy.exe")).unwrap(),
                r"D:\Tools\private-ai-proxy.exe"
            );
            assert_eq!(
                path_text(std::path::Path::new(
                    r"\\?\UNC\server\tools\private-ai-proxy.exe"
                ))
                .unwrap(),
                r"\\server\tools\private-ai-proxy.exe"
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
            executable: PathBuf::from("/opt/private-ai-proxy/private-ai-proxy"),
            command_path: PathBuf::from("/home/user/.local/bin/private-ai-proxy"),
            installed: true,
            on_path: false,
        })
        .unwrap();
        assert_eq!(
            value["commandPath"],
            "/home/user/.local/bin/private-ai-proxy"
        );
        assert_eq!(value["installed"], true);
        assert_eq!(value["onPath"], false);
        assert!(value.get("command_path").is_none());
    }
}
