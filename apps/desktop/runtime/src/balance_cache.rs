//! Short-lived, non-secret balance results shared by every desktop window.
use crate::contracts::AccountBalance;
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

type BalanceResult = Result<AccountBalance, String>;
type Entry = Arc<tokio::sync::Mutex<Option<(Instant, BalanceResult)>>>;

#[derive(Default)]
pub(crate) struct BalanceCache(Mutex<HashMap<String, Entry>>);

impl BalanceCache {
    pub async fn get(
        &self,
        key: String,
        fetch: impl Future<Output = BalanceResult>,
    ) -> BalanceResult {
        let entry = {
            let mut entries = self.0.lock().map_err(|_| "Balance cache is unavailable")?;
            // Bound retained accounts without evicting requests still in flight.
            if entries.len() >= 64 {
                entries.retain(|_, entry| Arc::strong_count(entry) > 1);
            }
            entries.entry(key).or_default().clone()
        };
        let mut cached = entry.lock().await;
        if let Some((at, result)) = cached.as_ref() {
            let ttl = if result.is_ok() { 30 } else { 10 };
            if at.elapsed() < Duration::from_secs(ttl) {
                return result.clone();
            }
        }
        let result = fetch.await;
        *cached = Some((Instant::now(), result.clone()));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn concurrent_windows_share_a_request_but_replaced_credentials_do_not() {
        let cache = BalanceCache::default();
        let calls = AtomicUsize::new(0);
        let fetch = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Err("temporarily unavailable".into())
        };
        let (first, second) = tokio::join!(
            cache.get("profile:old".into(), fetch()),
            cache.get("profile:old".into(), fetch())
        );
        assert_eq!(first.unwrap_err(), second.unwrap_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(cache.get("profile:new".into(), fetch()).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
