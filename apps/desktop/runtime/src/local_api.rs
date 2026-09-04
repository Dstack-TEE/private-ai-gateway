use std::{
    fs,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use crate::contracts::LocalApiConfig;
use desktop_gateway::{
    agents::{app_data_dir, write_atomic},
    tokens,
};

const CONFIG_FILE: &str = "local-api.json";

#[derive(Clone, Debug)]
pub struct ResolvedLocalApi {
    pub config: LocalApiConfig,
    pub bind: SocketAddr,
    pub endpoint: String,
}

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

pub fn resolve(mut config: LocalApiConfig) -> Result<ResolvedLocalApi, String> {
    config.listen_address = config.listen_address.trim().to_string();
    let address = config
        .listen_address
        .parse::<IpAddr>()
        .map_err(|_| "Listen address must be an IPv4 or IPv6 address".to_string())?;
    if config.port < 1024 {
        return Err("Port must be between 1024 and 65535".to_string());
    }
    if !config.allow_network_access && !address.is_loopback() {
        return Err("Turn on Allow network access before listening outside this Mac".to_string());
    }
    config.client_host = normalize_client_host(config.client_host.as_deref())?;
    if address.is_unspecified() && config.client_host.is_none() {
        return Err("Client host is required when listening on every interface".to_string());
    }
    let host = config
        .client_host
        .as_deref()
        .unwrap_or(&config.listen_address);
    let endpoint_host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    Ok(ResolvedLocalApi {
        bind: SocketAddr::new(address, config.port),
        endpoint: format!("http://{endpoint_host}:{}", config.port),
        config,
    })
}

fn normalize_client_host(value: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let unbracketed = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(value);
    if unbracketed.parse::<IpAddr>().is_ok() {
        return Ok(Some(unbracketed.to_string()));
    }
    if value.len() > 253
        || value.contains(['/', ':', '@'])
        || value.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
        })
    {
        return Err(
            "Client host must be a hostname or IP address without a scheme or path".to_string(),
        );
    }
    Ok(Some(value.to_ascii_lowercase()))
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
            .contains("Allow network access"));
        config.allow_network_access = true;
        assert!(resolve(config.clone()).unwrap_err().contains("Client host"));
        config.client_host = Some("gateway.local".to_string());
        assert_eq!(
            resolve(config).unwrap().endpoint,
            "http://gateway.local:4180"
        );
    }
}
