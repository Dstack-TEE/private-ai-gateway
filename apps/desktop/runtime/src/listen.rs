//! Listener validation shared by the Local API and the web UI.
//!
//! Loopback is the default. Any other address fails closed unless the user
//! explicitly allowed network access, and an all-interfaces listener needs a
//! client host so the advertised URL is reachable.

use std::net::{IpAddr, SocketAddr};

use crate::contracts::ListenConfig;

#[derive(Clone, Debug)]
pub struct ResolvedListen {
    /// Normalized settings.
    pub config: ListenConfig,
    pub bind: SocketAddr,
    /// The URL clients use: `http://<client host or listen address>:<port>`.
    pub endpoint: String,
}

/// Validates the address, confirmation and client host. Port ranges are the caller's policy.
pub fn resolve(mut config: ListenConfig) -> Result<ResolvedListen, String> {
    config.listen_address = config.listen_address.trim().to_string();
    let address = config
        .listen_address
        .parse::<IpAddr>()
        .map_err(|_| "Listen address must be an IPv4 or IPv6 address".to_string())?;
    if !config.allow_network_access && !address.is_loopback() {
        return Err("Network listening requires explicit confirmation".to_string());
    }
    config.client_host = normalize_client_host(config.client_host.as_deref())?;
    if address.is_unspecified() && config.client_host.is_none() {
        return Err("Client host is required when listening on every interface".to_string());
    }
    let host = config
        .client_host
        .as_deref()
        .unwrap_or(&config.listen_address);
    Ok(ResolvedListen {
        bind: SocketAddr::new(address, config.port),
        endpoint: format!("http://{}:{}", url_host(host), config.port),
        config,
    })
}

/// Brackets IPv6 literals for use in URLs and `Host` headers.
pub fn url_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config(address: &str, allow: bool, client_host: Option<&str>) -> ListenConfig {
        ListenConfig {
            listen_address: address.into(),
            allow_network_access: allow,
            port: 4182,
            client_host: client_host.map(Into::into),
        }
    }

    #[test]
    fn network_listeners_fail_closed() {
        assert_eq!(
            resolve(config(" ::1 ", false, None)).unwrap().endpoint,
            "http://[::1]:4182"
        );
        assert!(resolve(config("192.168.1.20", false, None))
            .unwrap_err()
            .contains("explicit confirmation"));
        assert_eq!(
            resolve(config("192.168.1.20", true, None))
                .unwrap()
                .endpoint,
            "http://192.168.1.20:4182"
        );
        assert!(resolve(config("::", true, None))
            .unwrap_err()
            .contains("Client host"));
        let resolved = resolve(config("::", true, Some("[fd00::2]"))).unwrap();
        assert_eq!(resolved.config.client_host.as_deref(), Some("fd00::2"));
        assert_eq!(resolved.endpoint, "http://[fd00::2]:4182");
        assert!(resolve(config("0.0.0.0", true, Some("https://host/")))
            .unwrap_err()
            .contains("without a scheme"));
        assert!(resolve(config("localhost", true, None))
            .unwrap_err()
            .contains("IPv4 or IPv6"));
    }
}
