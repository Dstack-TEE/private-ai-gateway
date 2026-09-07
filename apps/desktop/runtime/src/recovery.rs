use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

pub struct Recovery {
    pub changed: Arc<tokio::sync::Notify>,
    pub available: Arc<AtomicBool>,
    pending: AtomicBool,
    watcher: Mutex<Option<netwatcher::WatchHandle>>,
}

impl Default for Recovery {
    fn default() -> Self {
        Self {
            changed: Arc::new(tokio::sync::Notify::new()),
            available: Arc::new(AtomicBool::new(true)),
            pending: AtomicBool::new(false),
            watcher: Mutex::new(None),
        }
    }
}

impl Recovery {
    pub fn start(&self) -> Result<(), String> {
        let changed = self.changed.clone();
        let available = self.available.clone();
        let watcher = netwatcher::watch_interfaces_with_callback(move |update| {
            let online = update.interfaces.values().any(|interface| interface.ips.iter().any(|record| !record.ip.is_loopback() && !record.ip.is_unspecified()));
            available.store(online, Ordering::Release);
            if !update.is_initial && (update.addrs_added().next().is_some() || update.addrs_removed().next().is_some()) { changed.notify_one(); }
        }).map_err(|_| "Network changes could not be monitored. Reconnect protection manually after changing networks.".to_string())?;
        *self
            .watcher
            .lock()
            .map_err(|_| "Network monitor is unavailable")? = Some(watcher);
        Ok(())
    }
    pub fn cancel(&self) {
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

pub fn should_recover(status: &str, configuration_verification: bool, pending: bool) -> bool {
    !configuration_verification && (status == "verified" || (pending && status == "stopped"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_never_retries_failed_verification_or_manual_stop() {
        assert!(should_recover("verified", false, false));
        assert!(should_recover("stopped", false, true));
        for status in ["blocked", "error", "verifying"] {
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
