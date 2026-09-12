use crate::contracts::GatewayState;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

pub struct Recovery {
    pub changed: Arc<tokio::sync::Notify>,
    pub available: Arc<AtomicBool>,
    requested: Arc<AtomicBool>,
    pending: AtomicBool,
    retry: Mutex<Retry>,
    agent_retry: Mutex<Option<Instant>>,
    watcher: Mutex<Option<netwatcher::WatchHandle>>,
}

impl Default for Recovery {
    fn default() -> Self {
        Self {
            changed: Arc::new(tokio::sync::Notify::new()),
            available: Arc::new(AtomicBool::new(true)),
            requested: Arc::new(AtomicBool::new(false)),
            pending: AtomicBool::new(false),
            retry: Mutex::new(Retry::default()),
            agent_retry: Mutex::new(None),
            watcher: Mutex::new(None),
        }
    }
}

impl Recovery {
    pub fn start(&self) -> Result<(), String> {
        let changed = self.changed.clone();
        let available = self.available.clone();
        let requested = self.requested.clone();
        let watcher = netwatcher::watch_interfaces_with_callback(move |update| {
            let online = update.interfaces.values().any(|interface| interface.ips.iter().any(|record| !record.ip.is_loopback() && !record.ip.is_unspecified()));
            available.store(online, Ordering::Release);
            if !update.is_initial && (update.addrs_added().next().is_some() || update.addrs_removed().next().is_some()) {
                requested.store(true, Ordering::Release);
                changed.notify_one();
            }
        }).map_err(|_| "Network changes could not be monitored. Reconnect protection manually after changing networks.".to_string())?;
        *self
            .watcher
            .lock()
            .map_err(|_| "Network monitor is unavailable")? = Some(watcher);
        Ok(())
    }
    pub fn cancel(&self) {
        self.pending.store(false, Ordering::Release);
        self.clear_request();
        self.reset_retry();
        self.agents_finished(false);
    }
    pub fn retry_due(&self) -> bool {
        self.retry
            .lock()
            .is_ok_and(|mut retry| retry.due(Instant::now()))
    }
    pub fn reset_retry(&self) {
        if let Ok(mut retry) = self.retry.lock() {
            *retry = Retry::default();
        }
    }
    pub fn agents_ready(&self) -> bool {
        self.agent_retry
            .lock()
            .is_ok_and(|next| next.is_none_or(|next| Instant::now() >= next))
    }
    pub fn agents_finished(&self, failed: bool) {
        if let Ok(mut next) = self.agent_retry.lock() {
            *next = failed.then(|| Instant::now() + Duration::from_secs(30));
        }
    }
    pub fn request(&self) {
        self.requested.store(true, Ordering::Release);
        self.changed.notify_one();
    }
    pub fn needs_check(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
    pub fn clear_request(&self) {
        self.requested.store(false, Ordering::Release);
    }
    pub fn clear_wait(&self) {
        self.pending.store(false, Ordering::Release);
    }
    pub fn wait(&self) {
        self.pending.store(true, Ordering::Release);
    }
    pub fn pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }
    pub fn online(&self) -> bool {
        self.available.load(Ordering::Acquire)
    }
}

#[derive(Default)]
struct Retry {
    next: Option<Instant>,
    delay: u64,
}

impl Retry {
    fn due(&mut self, now: Instant) -> bool {
        let Some(next) = self.next else {
            self.delay = self.delay.max(3);
            self.next = Some(now + Duration::from_secs(self.delay));
            return false;
        };
        if now < next {
            return false;
        }
        self.delay = (self.delay * 2).min(30);
        // The next failure starts its own delay, even after a long verification.
        self.next = None;
        true
    }
}

pub fn connection_intended(state: &GatewayState) -> bool {
    state.session_active && !state.configuration_verification && state.status != "blocked"
}

pub fn should_retry(state: &GatewayState) -> bool {
    connection_intended(state)
        && (matches!(state.status.as_str(), "error" | "stopped") || state.endpoint_error.is_some())
}

pub fn should_recover(status: &str, configuration_verification: bool, pending: bool) -> bool {
    !configuration_verification
        && (matches!(status, "verified" | "verifying") || (pending && status == "stopped"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retries_back_off_without_retrying_stop_or_configuration_verification() {
        let now = Instant::now();
        let mut retry = Retry::default();
        assert!(!retry.due(now));
        assert!(!retry.due(now + Duration::from_secs(2)));
        assert!(retry.due(now + Duration::from_secs(3)));
        let mut failed_at = now + Duration::from_secs(100);
        for delay in [6, 12, 24, 30, 30] {
            assert!(!retry.due(failed_at));
            assert!(!retry.due(failed_at + Duration::from_secs(delay - 1)));
            assert!(retry.due(failed_at + Duration::from_secs(delay)));
            failed_at += Duration::from_secs(delay + 45);
        }
        let mut state = GatewayState {
            status: "error".into(),
            session_active: true,
            api_key_saved: true,
            ..Default::default()
        };
        assert!(should_retry(&state));
        state.configuration_verification = true;
        assert!(!should_retry(&state));
        state.configuration_verification = false;
        state.status = "blocked".into();
        assert!(!should_retry(&state));
        state.status = "error".into();
        state.session_active = false;
        assert!(!should_retry(&state));
    }

    #[test]
    fn address_changes_do_not_override_stop_or_security_block() {
        assert!(should_recover("verified", false, false));
        assert!(should_recover("stopped", false, true));
        assert!(should_recover("verifying", false, false));
        for status in ["blocked", "error"] {
            assert!(!should_recover(status, false, true));
        }
        assert!(!should_recover("stopped", false, false));
        assert!(!should_recover("verified", true, true));
        let state = Recovery::default();
        state.wait();
        state.cancel();
        assert!(!state.pending());
    }
}
