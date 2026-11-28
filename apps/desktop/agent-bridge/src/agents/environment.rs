use super::*;
use desktop_core::paths::agent_environment::{
    forwarded_sources, sensitive, AgentEnvironment, DSH_HOME, HERMES_HOME, OMP_PROFILE,
    OPENCLAW_NIX_MODE, OPENCLAW_PROFILE, PATH, PI_CODING_AGENT_DIR, PI_CONFIG_DIR, PI_PROFILE,
};
use std::{borrow::Cow, ffi::OsString, path::Component};

/// Look up configuration without changing the backend process environment.
#[derive(Clone)]
pub(crate) enum ToolEnvironment {
    Disabled,
    Process,
    Client(Arc<AgentEnvironment>),
}

impl ToolEnvironment {
    pub(super) fn enabled(&self) -> bool {
        !matches!(self, Self::Disabled)
    }

    pub(super) fn value(&self, name: &str) -> Option<OsString> {
        match self {
            Self::Disabled => None,
            Self::Client(values) if !sensitive(name) => values.get(name).map(OsString::from),
            _ => env::var_os(name),
        }
    }

    pub(super) fn path(&self, name: &str) -> Option<PathBuf> {
        if matches!(self, Self::Process) {
            return env_path(name);
        }
        self.value(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }

    pub(super) fn unicode(&self, name: &str) -> Option<String> {
        self.value(name)?.into_string().ok()
    }

    pub(super) fn search_paths(&self) -> impl Iterator<Item = PathBuf> {
        self.value(PATH)
            .filter(|_| matches!(self, Self::Client(_)))
            .into_iter()
            .flat_map(|path| trusted_search_paths(&path))
            .chain(
                env::var_os(PATH)
                    .into_iter()
                    .flat_map(|path| trusted_search_paths(&path)),
            )
    }

    pub(super) fn validate(&self, agent: Agent, home: &Path) -> Result<(), String> {
        let Self::Client(values) = self else {
            return Ok(());
        };
        for (name, value) in values.iter() {
            if !forwarded_sources(agent).any(|allowed| allowed == name) || value.contains('\0') {
                return Err("The agent environment contains an invalid variable".to_string());
            }
            if value.is_empty()
                || matches!(
                    name.as_str(),
                    PATH | OPENCLAW_NIX_MODE | OPENCLAW_PROFILE | OMP_PROFILE | PI_PROFILE
                )
            {
                continue;
            }
            let path = match name.as_str() {
                // These selectors have documented home-relative forms.
                PI_CONFIG_DIR if !Path::new(value).is_absolute() => home.join(value),
                PI_CODING_AGENT_DIR if agent == Agent::Pi => registry::pi_agent_dir(home, self),
                DSH_HOME => dsh::home_dir(home, self),
                HERMES_HOME => match nonblank(PathBuf::from(value)) {
                    Some(path) => path,
                    None => continue,
                },
                _ => PathBuf::from(value),
            };
            trusted_path(&path, true).map_err(|_| {
                format!("{name} must select an absolute path owned by you or the system and protected from public writes")
            })?;
        }
        Ok(())
    }
}

impl Projector {
    pub(super) fn for_connection(
        &self,
        requested: Option<&AgentEnvironment>,
        prior: Option<&Connection>,
    ) -> Cow<'_, Self> {
        if self.tool_env.enabled() {
            if let Some(values) =
                requested.or_else(|| prior.and_then(|record| record.options.environment.as_ref()))
            {
                let mut projector = self.clone();
                projector.tool_env = ToolEnvironment::Client(Arc::new(values.clone()));
                return Cow::Owned(projector);
            }
        }
        Cow::Borrowed(self)
    }

    pub(super) fn connection_environment(&self) -> Option<AgentEnvironment> {
        let ToolEnvironment::Client(environment) = &self.tool_env else {
            return None;
        };
        let mut environment = environment.as_ref().clone();
        if let Some(path) = environment.get_mut(PATH) {
            *path = env::join_paths(trusted_search_paths(path.as_ref()))
                .ok()
                .and_then(|path| path.into_string().ok())
                .unwrap_or_default();
        }
        Some(environment)
    }
}

fn trusted_search_paths(path: &std::ffi::OsStr) -> Vec<PathBuf> {
    env::split_paths(path)
        .filter(|dir| dir.is_dir())
        .filter_map(|dir| {
            let resolved = trusted_path(&dir, false).ok()?;
            Some(if cfg!(windows) { dir } else { resolved })
        })
        .collect()
}

/// Inspect the existing part of a supplied path, including both sides of
/// directory symlinks. Agent configs may be created by the first connection.
pub(super) fn trusted_path(path: &Path, allow_missing: bool) -> io::Result<PathBuf> {
    let invalid = || io::Error::from(io::ErrorKind::PermissionDenied);
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err(invalid());
    }
    let mut existing = path;
    let resolved = loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => break existing.canonicalize()?,
            Err(error) if allow_missing && error.kind() == io::ErrorKind::NotFound => {
                existing = existing.parent().ok_or_else(invalid)?;
            }
            Err(error) => return Err(error),
        }
    };
    let parent = existing.parent().map(Path::canonicalize).transpose()?;
    let normalized = match (parent, existing.file_name()) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => resolved.clone(),
    };
    let paths = [existing, resolved.as_path(), normalized.as_path()];
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions.
        let user = unsafe { libc::geteuid() };
        for path in paths.into_iter().flat_map(Path::ancestors) {
            let metadata = fs::metadata(path)?;
            if ![user, 0].contains(&metadata.uid()) || metadata.mode() & 0o002 != 0 {
                return Err(invalid());
            }
        }
    }
    #[cfg(windows)]
    {
        use desktop_core::windows_acl;
        use windows_sys::Win32::{
            Foundation::{GENERIC_ALL, GENERIC_WRITE},
            Storage::FileSystem::{
                DELETE, FILE_ADD_FILE, FILE_APPEND_DATA, FILE_DELETE_CHILD, FILE_WRITE_DATA,
                WRITE_DAC, WRITE_OWNER,
            },
        };
        let acl = |path: &Path| {
            windows_acl::read(&windows_acl::open_for_inspection(path)?).map_err(|_| invalid())
        };
        let trusted = |acl: &windows_acl::Acl, sid: &str| {
            sid == acl.current_user_sid || windows_acl::TRUSTED_SIDS.contains(&sid)
        };
        let writes = |acl: &windows_acl::Acl, mask, check_sid: &dyn Fn(&str) -> bool| {
            // ACEs marked inherit-only do not apply to this entry.
            acl.aces.iter().any(|ace| {
                ace.kind == 0
                    && ace.flags & 0x08 == 0
                    && ace.mask & mask != 0
                    && check_sid(&ace.sid)
            })
        };
        const COMMON_WRITE: u32 = GENERIC_ALL | GENERIC_WRITE | DELETE | WRITE_DAC | WRITE_OWNER;
        let entry = acl(&resolved)?;
        let write = if resolved.is_dir() {
            FILE_DELETE_CHILD
        } else {
            FILE_WRITE_DATA | FILE_APPEND_DATA
        };
        if !trusted(&entry, &entry.owner_sid)
            || writes(&entry, COMMON_WRITE | write, &|sid| !trusted(&entry, sid))
        {
            return Err(invalid());
        }
        for dir in paths.into_iter().flat_map(|path| path.ancestors().skip(1)) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            if writes(
                &acl(dir)?,
                COMMON_WRITE | FILE_ADD_FILE | FILE_DELETE_CHILD,
                &|sid| {
                    matches!(
                        sid,
                        "S-1-1-0" | "S-1-5-11" | "S-1-5-32-545" | "S-1-5-32-546"
                    )
                },
            ) {
                return Err(invalid());
            }
        }
    }
    Ok(resolved.join(path.strip_prefix(existing).map_err(|_| invalid())?))
}
