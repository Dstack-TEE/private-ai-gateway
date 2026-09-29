//! Listener validation shared by the Local API and the web UI.
//!
//! Loopback is the default. Any other address fails closed unless the user
//! explicitly allowed network access, and an all-interfaces listener needs a
//! client host so the advertised URL is reachable.

use std::net::{IpAddr, SocketAddr};

use url::{Host, Url};

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
        endpoint: http_endpoint(host, config.port)?,
        config,
    })
}

/// `http://<host>:<port>`; `url` leaves out the port when it is 80.
pub fn http_endpoint(host: &str, port: u16) -> Result<String, String> {
    let invalid = || format!("Cannot form a URL for host {host}");
    let mut url = Url::parse("http://localhost").map_err(|_| invalid())?;
    url.set_host(Some(&url_host(host))).map_err(|_| invalid())?;
    url.set_port(Some(port)).map_err(|()| invalid())?;
    Ok(url.origin().ascii_serialization())
}

/// A host as URLs and `Host` headers write it: IPv6 literals in brackets.
pub fn url_host(host: &str) -> String {
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => Host::<&str>::Ipv4(ip).to_string(),
        Ok(IpAddr::V6(ip)) => Host::<&str>::Ipv6(ip).to_string(),
        Err(_) => host.to_string(),
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
        assert_eq!(url_host("::1"), "[::1]");
        assert_eq!(url_host("[::1]"), "[::1]");
        assert_eq!(url_host("studio.local"), "studio.local");
    }

    /// Unusual addresses are written in `url`'s canonical form: the default
    /// port is left out and IPv6 literals are compressed and lowercased.
    #[test]
    fn endpoints_are_canonical_urls() {
        let endpoint = |address: &str, port, client_host: Option<&str>| {
            let mut config = config(address, true, client_host);
            config.port = port;
            resolve(config).unwrap().endpoint
        };
        assert_eq!(endpoint("127.0.0.1", 4190, None), "http://127.0.0.1:4190");
        assert_eq!(endpoint("127.0.0.1", 80, None), "http://127.0.0.1");
        assert_eq!(endpoint("0:0:0:0:0:0:0:1", 4190, None), "http://[::1]:4190");
        assert_eq!(
            endpoint("::", 4190, Some("FD00:0:0:0:0:0:0:2")),
            "http://[fd00::2]:4190"
        );
        assert_eq!(
            endpoint("0.0.0.0", 8080, Some("Studio.Local")),
            "http://studio.local:8080"
        );
    }
}
