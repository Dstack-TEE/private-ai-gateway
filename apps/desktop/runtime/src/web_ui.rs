//! The browser UI hosted by the backend service on `127.0.0.1`.
//!
//! It is off by default. The IPC endpoint remains the root of trust: only an
//! authenticated local client can mint the one-time login code that opens a
//! browser session.

mod auth;
#[cfg(feature = "web-ui")]
mod server;

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

pub use auth::Auth;

use crate::preferences::WebUiConfig;

/// Clear of the Local API (4180) and the account callback (4181).
pub const DEFAULT_PORT: u16 = 4182;
const ACCOUNT_CALLBACK_PORT: u16 = 4181;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebUiLogin {
    /// `http://127.0.0.1:PORT/#code=…`; the code works once, within `expires_in_seconds`.
    pub url: String,
    pub expires_in_seconds: u64,
}

pub fn validate(config: WebUiConfig, local_api_port: u16) -> Result<(), String> {
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
    Ok(())
}

pub struct WebUi {
    auth: Arc<Auth>,
    running: Mutex<Option<(u16, CancellationToken)>>,
    #[cfg_attr(not(feature = "web-ui"), allow(dead_code))]
    handle: Handle,
}

impl WebUi {
    pub(crate) fn new(handle: Handle) -> Self {
        Self {
            auth: Arc::default(),
            running: Mutex::new(None),
            handle,
        }
    }

    /// Closes the listener and every browser session. Returns the port that was open.
    pub(crate) fn stop(&self) -> Option<u16> {
        let previous = self
            .running
            .lock()
            .ok()
            .and_then(|mut running| running.take());
        self.auth.revoke_all();
        previous.map(|(port, shutdown)| {
            shutdown.cancel();
            port
        })
    }

    #[cfg(feature = "web-ui")]
    pub(crate) fn start(
        &self,
        runtime: Arc<crate::controller::DesktopRuntime>,
        port: u16,
        reopening: bool,
    ) -> Result<String, String> {
        let shutdown = CancellationToken::new();
        let url = server::start(
            runtime,
            port,
            reopening,
            self.auth.clone(),
            shutdown.clone(),
            &self.handle,
        )?;
        if let Ok(mut running) = self.running.lock() {
            *running = Some((port, shutdown));
        }
        Ok(url)
    }

    #[cfg(not(feature = "web-ui"))]
    pub(crate) fn start(
        &self,
        _runtime: Arc<crate::controller::DesktopRuntime>,
        _port: u16,
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

    #[test]
    fn ports_cannot_collide_with_local_services() {
        let config = |port| WebUiConfig {
            enabled: true,
            port,
        };
        assert!(validate(config(DEFAULT_PORT), 4180).is_ok());
        assert!(validate(config(0), 4180).is_err());
        assert!(validate(config(4180), 4180).is_err());
        assert!(validate(config(4181), 4180).is_err());
        assert!(validate(config(5000), 5000).is_err());
    }
}
