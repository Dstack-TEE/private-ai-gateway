//! Secrets live only in the OS credential store (macOS Keychain, Windows
//! Credential Manager, Secret Service on Linux): Confidential AI service
//! credentials, and previous values of credential fields a connection took
//! over, kept so a disconnect can put them back. Values are read into memory
//! when needed and never written to config files, manifests, logs, or the UI.

use std::{collections::HashMap, sync::Mutex};

const SERVICE: &str = desktop_core::brand::APP_IDENTIFIER;
const MAX_KEY_LEN: usize = 512;

/// A named-entry secret store. Entry names are app-chosen, never user input.
pub trait SecretStore: Send + Sync {
    fn get(&self, entry: &str) -> Result<Option<String>, String>;
    fn set(&self, entry: &str, value: &str) -> Result<(), String>;
    fn delete(&self, entry: &str) -> Result<(), String>;
}

/// Validate a key the user typed: trimmed, single line, bounded length.
pub fn validate_api_key(value: &str) -> Result<String, String> {
    let key = value.trim();
    if key.is_empty() {
        return Err("Enter an API key".to_string());
    }
    if key.len() > MAX_KEY_LEN || key.chars().any(char::is_whitespace) {
        return Err("The API key must be a single token without spaces".to_string());
    }
    Ok(key.to_string())
}

/// OS credential store backed by the `keyring` crate.
pub struct KeyringStore;

impl KeyringStore {
    fn entry(name: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, name).map_err(store_error)
    }

    #[cfg(target_os = "linux")]
    fn run<T: Send>(operation: impl FnOnce() -> Result<T, String> + Send) -> Result<T, String> {
        // The Linux keyring backend uses zbus's blocking API, which creates a
        // Tokio runtime internally. Profile mutations already run on Tokio
        // workers, so execute Secret Service calls on a plain scoped thread.
        std::thread::scope(|scope| {
            scope
                .spawn(operation)
                .join()
                .map_err(|_| "The system credential store operation panicked".to_string())?
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn run<T>(operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        operation()
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, entry: &str) -> Result<Option<String>, String> {
        Self::run(|| match Self::entry(entry)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(store_error(error)),
        })
    }

    fn set(&self, entry: &str, value: &str) -> Result<(), String> {
        Self::run(|| Self::entry(entry)?.set_password(value).map_err(store_error))
    }

    fn delete(&self, entry: &str) -> Result<(), String> {
        Self::run(|| match Self::entry(entry)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(store_error(error)),
        })
    }
}

fn store_error(error: keyring::Error) -> String {
    format!("The system credential store is unavailable: {error}")
}

/// In-memory store for tests; never persists.
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl MemoryStore {
    /// Whether any stored value equals `value` (tests check nothing leaked).
    pub fn holds(&self, value: &str) -> bool {
        self.0
            .lock()
            .map(|map| map.values().any(|held| held == value))
            .unwrap_or(false)
    }

    pub fn len(&self) -> usize {
        self.0.lock().map(|map| map.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, entry: &str) -> Result<Option<String>, String> {
        Ok(self
            .0
            .lock()
            .map_err(|_| "store poisoned".to_string())?
            .get(entry)
            .cloned())
    }

    fn set(&self, entry: &str, value: &str) -> Result<(), String> {
        self.0
            .lock()
            .map_err(|_| "store poisoned".to_string())?
            .insert(entry.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, entry: &str) -> Result<(), String> {
        self.0
            .lock()
            .map_err(|_| "store poisoned".to_string())?
            .remove(entry);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_blank_and_multiline_keys() {
        assert!(validate_api_key("  ").is_err());
        assert!(validate_api_key("sk-a\nsk-b").is_err());
        assert_eq!(validate_api_key("  sk-abc  ").unwrap(), "sk-abc");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread")]
    async fn keyring_operations_leave_the_callers_tokio_runtime() {
        KeyringStore::run(|| {
            tokio::runtime::Runtime::new()
                .map_err(|error| error.to_string())?
                .block_on(async {});
            Ok(())
        })
        .unwrap();
    }

    /// Real credential store round trip; run explicitly on a desktop OS.
    #[test]
    #[ignore = "touches the OS credential store"]
    fn keyring_round_trip() {
        let store = KeyringStore;
        let entry = "smoke-test-entry";
        store.set(entry, "value-1").unwrap();
        assert_eq!(store.get(entry).unwrap().as_deref(), Some("value-1"));
        store.delete(entry).unwrap();
        assert_eq!(store.get(entry).unwrap(), None);
    }
}
