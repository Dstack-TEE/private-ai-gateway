//! Where a connection parks the previous values of credential fields it took
//! over, so a disconnect can put them back. Values are read into memory when
//! needed and never written to agent configs, manifests, logs, or the UI; the
//! backend keeps them with this device's state, in its owner-only
//! `local-state.json`.

use std::{collections::HashMap, sync::Mutex};

/// A named-entry secret store. Entry names are app-chosen, never user input.
pub trait SecretStore: Send + Sync {
    fn get(&self, entry: &str) -> Result<Option<String>, String>;
    fn set(&self, entry: &str, value: &str) -> Result<(), String>;
    fn delete(&self, entry: &str) -> Result<(), String>;
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
