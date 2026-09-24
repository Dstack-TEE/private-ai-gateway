//! Secrets that describe this device rather than the user: the values a
//! connected agent's credential fields held before the connection took them
//! over (put back on disconnect), and replaced account keys this device still
//! has to revoke. They belong with the rest of this machine's state, beside
//! `agent-connections.json` in the app data directory, as an owner-only JSON
//! file, so the settings directory stays safe to sync between devices.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, MutexGuard,
    },
};

use agent_bridge::secrets::SecretStore;
use desktop_core::{contracts::ServiceProvider, private_fs};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

pub const LOCAL_STATE_FILE: &str = "local-state.json";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalSecrets {
    /// Previous values of agent credential fields, by restore entry.
    pub agent_restore: BTreeMap<String, String>,
    /// Replaced account keys still to revoke or activate, by record ID.
    pub account_cleanup: IndexMap<String, RetiredCredential>,
}

/// An account key replaced or removed locally that the provider still has to
/// revoke (or, for a new key, activate) once reachable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetiredCredential {
    pub profile_id: String,
    pub action: String,
    pub provider: ServiceProvider,
    pub key: String,
    pub revoke: bool,
}

/// Reads the file; a missing file is empty state.
pub fn load(path: &Path) -> Result<LocalSecrets, String> {
    match private_fs::read_private_text(path)
        .map_err(|error| format!("Cannot read {LOCAL_STATE_FILE}: {error}"))?
    {
        Some(text) => serde_json::from_str(&text).map_err(|_| {
            format!("{LOCAL_STATE_FILE} is damaged; agent restore values and pending key revocations are unavailable")
        }),
        None => Ok(LocalSecrets::default()),
    }
}

/// Replaces the file atomically and leaves it owner-only.
pub fn save(path: &Path, secrets: &LocalSecrets) -> Result<(), String> {
    let text = serde_json::to_string_pretty(secrets)
        .map_err(|_| format!("Cannot encode {LOCAL_STATE_FILE}"))?;
    private_fs::write_atomic(path, &text, None)
        .and_then(|()| private_fs::tighten_private(path))
        .map_err(|error| format!("Cannot save {LOCAL_STATE_FILE}: {error}"))
}

pub struct LocalState {
    path: PathBuf,
    /// A damaged file stays unavailable rather than being overwritten.
    secrets: Mutex<Result<LocalSecrets, String>>,
    /// While the 0.1 credential import runs, a missing restore value may
    /// still be on its way from the OS credential store.
    importing: AtomicBool,
}

impl LocalState {
    pub fn open(data_dir: &Path) -> Self {
        let path = data_dir.join(LOCAL_STATE_FILE);
        Self {
            secrets: Mutex::new(load(&path)),
            path,
            importing: AtomicBool::new(false),
        }
    }

    pub(crate) fn set_importing(&self, importing: bool) {
        self.importing.store(importing, Ordering::Release);
    }

    pub(crate) fn importing(&self) -> bool {
        self.importing.load(Ordering::Acquire)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Result<LocalSecrets, String>>, String> {
        self.secrets
            .lock()
            .map_err(|_| "Local state is unavailable".to_string())
    }

    pub fn read(&self) -> Result<LocalSecrets, String> {
        self.lock()?.clone()
    }

    pub fn update(
        &self,
        change: impl FnOnce(&mut LocalSecrets) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut current = self.lock()?;
        let saved = current.as_ref().map_err(Clone::clone)?;
        let mut next = saved.clone();
        change(&mut next)?;
        if &next != saved {
            save(&self.path, &next)?;
            *current = Ok(next);
        }
        Ok(())
    }
}

/// Agents park the credential values they take over here.
impl SecretStore for LocalState {
    /// A value that may still be importing fails (the agent operation is
    /// retried) rather than reading as absent, which would drop it for good.
    fn get(&self, entry: &str) -> Result<Option<String>, String> {
        let value = self.read()?.agent_restore.get(entry).cloned();
        if value.is_none() && self.importing.load(Ordering::Acquire) {
            return Err("Saved agent credentials from 0.1 are still being imported".to_string());
        }
        Ok(value)
    }

    fn set(&self, entry: &str, value: &str) -> Result<(), String> {
        self.update(|secrets| {
            secrets
                .agent_restore
                .insert(entry.to_string(), value.to_string());
            Ok(())
        })
    }

    fn delete(&self, entry: &str) -> Result<(), String> {
        self.update(|secrets| {
            secrets.agent_restore.remove(entry);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_persist_owner_only_and_a_damaged_file_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let state = LocalState::open(dir.path());
        assert_eq!(state.get("restore:codex:1").unwrap(), None);
        state.set("restore:codex:1", "sk-previous").unwrap();
        let path = dir.path().join(LOCAL_STATE_FILE);
        assert_eq!(
            LocalState::open(dir.path())
                .get("restore:codex:1")
                .unwrap()
                .as_deref(),
            Some("sk-previous")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        state.delete("restore:codex:1").unwrap();
        assert!(state.read().unwrap().agent_restore.is_empty());

        std::fs::write(&path, "{ damaged").unwrap();
        let damaged = LocalState::open(dir.path());
        assert!(damaged.get("restore:codex:1").is_err());
        assert!(damaged.set("restore:codex:1", "value").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ damaged");
    }
}
