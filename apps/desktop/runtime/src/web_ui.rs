//! The browser UI hosted by the backend service.
//!
//! It is off by default and listens on `127.0.0.1` unless network access is
//! explicitly allowed, exactly like the Local API. The IPC endpoint remains the
//! root of trust: only an authenticated local client can mint the one-time
//! login code that opens a browser session.

mod auth;
#[cfg(feature = "web-ui")]
mod server;
#[cfg(feature = "web-ui")]
mod throttle;

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

pub use auth::Auth;
#[cfg(feature = "web-ui")]
pub use throttle::Throttle;

use desktop_core::{
    contracts::WebUiLogin,
    listen::{self, ResolvedListen},
    preferences::WebUiConfig,
};

const ACCOUNT_CALLBACK_PORT: u16 = 4181;

/// Checks the port policy and the shared listener rules; non-loopback fails closed.
pub fn validate(config: &WebUiConfig, local_api_port: u16) -> Result<ResolvedListen, String> {
    let port = config.port;
    if port == 0 {
        return Err("Web UI port must be between 1 and 65535".into());
    }
    if port == local_api_port {
        return Err(format!(
            "Web UI port {port} is used by the Local API; choose another port"
        ));
    }
    if port == ACCOUNT_CALLBACK_PORT {
        return Err(format!(
            "Web UI port {port} is reserved for account connection callbacks; choose another port"
        ));
    }
    // The prefix keeps these messages through the management error allowlist.
    listen::resolve(config.listen()).map_err(|error| format!("Web UI: {error}"))
}

pub struct WebUi {
    auth: Arc<Auth>,
    #[cfg(feature = "web-ui")]
    throttle: Arc<Throttle>,
    running: Mutex<Option<(SocketAddr, CancellationToken)>>,
    #[cfg_attr(not(feature = "web-ui"), allow(dead_code))]
    handle: Handle,
}

impl WebUi {
    pub(crate) fn new(handle: Handle) -> Self {
        Self {
            auth: Arc::default(),
            #[cfg(feature = "web-ui")]
            throttle: Arc::default(),
            running: Mutex::new(None),
            handle,
        }
    }

    /// Closes the listener and every browser session. Returns the address that was open.
    pub(crate) fn stop(&self) -> Option<SocketAddr> {
        let previous = self
            .running
            .lock()
            .ok()
            .and_then(|mut running| running.take());
        self.auth.revoke_all();
        previous.map(|(bind, shutdown)| {
            shutdown.cancel();
            bind
        })
    }

    #[cfg(feature = "web-ui")]
    pub(crate) fn start(
        &self,
        runtime: Arc<crate::controller::DesktopRuntime>,
        listen: &ResolvedListen,
        reopening: bool,
    ) -> Result<String, String> {
        let shutdown = CancellationToken::new();
        server::start(
            runtime,
            listen,
            reopening,
            self.auth.clone(),
            self.throttle.clone(),
            shutdown.clone(),
            &self.handle,
        )?;
        if let Ok(mut running) = self.running.lock() {
            *running = Some((listen.bind, shutdown));
        }
        Ok(listen.endpoint.clone())
    }

    #[cfg(not(feature = "web-ui"))]
    pub(crate) fn start(
        &self,
        _runtime: Arc<crate::controller::DesktopRuntime>,
        _listen: &ResolvedListen,
        _reopening: bool,
    ) -> Result<String, String> {
        Err("The web UI is not included in this build".into())
    }

    pub(crate) fn login(&self, url: &str) -> WebUiLogin {
        WebUiLogin {
            url: format!("{url}/#code={}", self.auth.mint_code()),
            expires_in_seconds: auth::CODE_TTL.as_secs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use desktop_core::preferences::WEB_UI_DEFAULT_PORT;

    #[test]
    fn ports_cannot_collide_with_local_services() {
        let config = |port| WebUiConfig {
            enabled: true,
            port,
            ..WebUiConfig::default()
        };
        assert_eq!(
            validate(&config(WEB_UI_DEFAULT_PORT), 4180)
                .unwrap()
                .endpoint,
            "http://127.0.0.1:4182"
        );
        assert!(validate(&config(0), 4180).is_err());
        assert!(validate(&config(4180), 4180).is_err());
        assert!(validate(&config(4181), 4180).is_err());
        assert!(validate(&config(5000), 5000).is_err());
    }

    #[test]
    fn network_listening_needs_confirmation_and_a_reachable_host() {
        let mut config = WebUiConfig {
            enabled: true,
            listen_address: "192.168.1.20".into(),
            ..WebUiConfig::default()
        };
        assert!(validate(&config, 4180)
            .unwrap_err()
            .contains("explicit confirmation"));
        config.allow_network_access = true;
        assert_eq!(
            validate(&config, 4180).unwrap().endpoint,
            "http://192.168.1.20:4182"
        );
        config.listen_address = "0.0.0.0".into();
        assert!(validate(&config, 4180).unwrap_err().contains("Client host"));
        config.client_host = Some("Studio.local".into());
        let listen = validate(&config, 4180).unwrap();
        assert_eq!(listen.bind.to_string(), "0.0.0.0:4182");
        assert_eq!(listen.endpoint, "http://studio.local:4182");
    }
}
