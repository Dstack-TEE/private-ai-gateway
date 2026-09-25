//! The browser UI hosted by the backend service.
//!
//! It is off by default and listens on `127.0.0.1` unless network access is
//! explicitly allowed, exactly like the Local API. It cannot run without a
//! sign-in password, which only an authenticated local client (the desktop app
//! or CLI over IPC) or an already signed-in browser can set.

mod auth;
pub(crate) mod password;
#[cfg(feature = "web-ui")]
mod server;
#[cfg(feature = "web-ui")]
mod throttle;

use std::sync::{Arc, Mutex};

use tokio::{runtime::Handle, task::JoinHandle};
use tokio_util::sync::CancellationToken;

pub use auth::Auth;
#[cfg(feature = "web-ui")]
pub(crate) use server::{change_password, routes, Gate};
#[cfg(feature = "web-ui")]
pub use throttle::Throttle;

use desktop_core::listen::ResolvedListen;

pub struct WebUi {
    auth: Arc<Auth>,
    #[cfg(feature = "web-ui")]
    throttle: Arc<Throttle>,
    running: Mutex<Option<(CancellationToken, JoinHandle<()>)>>,
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

    /// Closes the listener and every browser session, so its port can be
    /// bound again once this returns.
    pub(crate) async fn stop(&self) {
        let previous = self
            .running
            .lock()
            .ok()
            .and_then(|mut running| running.take());
        self.auth.revoke_all();
        if let Some((shutdown, server)) = previous {
            shutdown.cancel();
            // Only until the listener is closed: the request that stopped
            // it is still open.
            let _ = server.await;
        }
    }

    #[cfg(feature = "web-ui")]
    pub(crate) fn start(
        &self,
        runtime: Arc<crate::controller::DesktopRuntime>,
        listen: &ResolvedListen,
    ) -> Result<String, String> {
        let shutdown = CancellationToken::new();
        let server = server::start(
            runtime,
            listen,
            self.auth.clone(),
            self.throttle.clone(),
            shutdown.clone(),
            &self.handle,
        )?;
        if let Ok(mut running) = self.running.lock() {
            *running = Some((shutdown, server));
        }
        Ok(listen.endpoint.clone())
    }

    #[cfg(not(feature = "web-ui"))]
    pub(crate) fn start(
        &self,
        _runtime: Arc<crate::controller::DesktopRuntime>,
        _listen: &ResolvedListen,
    ) -> Result<String, String> {
        Err("The web UI is not included in this build".into())
    }

    pub(crate) fn has_password(&self) -> bool {
        self.auth.has_password()
    }

    /// Applies a saved password change; every browser session ends.
    pub(crate) fn set_password(&self, hash: Option<String>) {
        self.auth.set_password(hash);
    }
}
