use std::{fs, path::PathBuf};

use crate::{
    contracts::LocalApiConfig,
    listen::{self, ResolvedListen},
};
use agent_bridge::{
    agents::{app_data_dir, write_atomic},
    tokens,
};

const CONFIG_FILE: &str = "local-api.json";

pub type ResolvedLocalApi = ResolvedListen;

pub fn load() -> Result<ResolvedLocalApi, String> {
    let path = config_path()?;
    let config = match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|_| "The saved Local API settings are invalid".to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => LocalApiConfig::default(),
        Err(error) => return Err(format!("Cannot read Local API settings: {error}")),
    };
    resolve(config)
}

pub fn save(config: LocalApiConfig) -> Result<ResolvedLocalApi, String> {
    let resolved = resolve(config)?;
    let path = config_path()?;
    let text = serde_json::to_string_pretty(&resolved.config)
        .map_err(|error| format!("Cannot encode Local API settings: {error}"))?;
    let parent = path
        .parent()
        .ok_or_else(|| "The Local API settings path has no parent".to_string())?;
    tokens::create_private_dir(parent)
        .map_err(|error| format!("Cannot create the app data directory: {error}"))?;
    write_atomic(&path, &text, None)
        .map_err(|error| format!("Cannot save Local API settings: {error}"))?;
    Ok(resolved)
}

pub fn resolve(config: LocalApiConfig) -> Result<ResolvedLocalApi, String> {
    if config.port < 1024 {
        return Err("Port must be between 1024 and 65535".to_string());
    }
    listen::resolve(config)
}

fn config_path() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join(CONFIG_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_api_config_is_fail_closed_for_network_listeners() {
        let mut config = LocalApiConfig::default();
        assert_eq!(
            resolve(config.clone()).unwrap().endpoint,
            "http://127.0.0.1:4180"
        );
        config.listen_address = "0.0.0.0".to_string();
        assert!(resolve(config.clone())
            .unwrap_err()
            .contains("explicit confirmation"));
        config.allow_network_access = true;
        assert!(resolve(config.clone()).unwrap_err().contains("Client host"));
        config.client_host = Some("gateway.local".to_string());
        assert_eq!(
            resolve(config).unwrap().endpoint,
            "http://gateway.local:4180"
        );
    }
}
