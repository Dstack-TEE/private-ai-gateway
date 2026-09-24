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

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

pub use auth::Auth;
#[cfg(feature = "web-ui")]
pub(crate) use server::{routes, Gate};
#[cfg(feature = "web-ui")]
pub use throttle::Throttle;

use desktop_core::listen::ResolvedListen;

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

    pub(crate) fn has_password(&self) -> bool {
        self.auth.has_password()
    }

    /// Applies a saved password change; every browser session ends.
    pub(crate) fn set_password(&self, hash: Option<String>) {
        self.auth.set_password(hash);
    }
}
