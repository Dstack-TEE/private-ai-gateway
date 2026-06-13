//! Per-upstream Chutes session store: chute-id cache and verified-nonce pools.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{
    chutes_binding_matches, ChutesAcceptedBinding, ChutesVerifiedDiscovery, SelectedChutesInstance,
    CHUTES_DEFAULT_NONCE_TTL_SECONDS, CHUTES_MODEL_CACHE_TTL_SECONDS,
};
use crate::aci::upstream::UpstreamError;

#[derive(Debug)]
pub struct ChutesSessionStore {
    cache: Mutex<ChutesSessionCache>,
    pub(super) refill_lock: tokio::sync::Mutex<()>,
}

impl ChutesSessionStore {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(ChutesSessionCache::default()),
            refill_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn cached_chute_id(&self, model: &str) -> Option<String> {
        let now = Instant::now();
        let mut cache = self.cache.lock().unwrap();
        match cache.model_map.get(model) {
            Some(entry) if entry.expires_at > now => Some(entry.chute_id.clone()),
            Some(_) => {
                cache.model_map.remove(model);
                None
            }
            None => None,
        }
    }

    pub fn cache_chute_id(&self, model: &str, chute_id: &str) {
        let expires_at = Instant::now() + Duration::from_secs(CHUTES_MODEL_CACHE_TTL_SECONDS);
        self.cache.lock().unwrap().model_map.insert(
            model.to_string(),
            CachedChuteId {
                chute_id: chute_id.to_string(),
                expires_at,
            },
        );
    }

    pub fn record_verified_discovery(
        &self,
        discovery: ChutesVerifiedDiscovery,
    ) -> Result<usize, UpstreamError> {
        let nonce_ttl = discovery
            .nonce_expires_in
            .unwrap_or(CHUTES_DEFAULT_NONCE_TTL_SECONDS);
        if nonce_ttl == 0 {
            return Ok(0);
        }
        let candidates = discovery
            .instances
            .into_iter()
            .filter(|instance| !instance.nonces.is_empty())
            .map(|instance| ChutesNonceCandidate {
                instance_id: instance.instance_id,
                e2e_pubkey: instance.e2e_pubkey,
                public_key_sha256: instance.public_key_sha256,
                nonces: instance.nonces,
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Err(UpstreamError::Transport(
                "verified Chutes E2EE instances did not include fresh nonces".to_string(),
            ));
        }
        Ok(self.record_nonce_candidates(&discovery.chute_id, nonce_ttl, candidates))
    }

    pub(super) fn pop_verified_nonce(
        &self,
        chute_id: &str,
        accepted: &[ChutesAcceptedBinding],
    ) -> Result<Option<SelectedChutesInstance>, UpstreamError> {
        let now = Instant::now();
        let mut cache = self.cache.lock().unwrap();
        let Some(pool) = cache.nonce_pools.get_mut(chute_id) else {
            return Ok(None);
        };

        let mut retained = VecDeque::with_capacity(pool.len());
        let mut selected = None;
        while let Some(nonce) = pool.pop_front() {
            if nonce.expires_at <= now {
                continue;
            }
            if !chutes_binding_matches(accepted, &nonce.instance_id, &nonce.public_key_sha256) {
                continue;
            }
            if selected.is_none() {
                selected = Some(SelectedChutesInstance {
                    instance_id: nonce.instance_id,
                    e2e_pubkey: nonce.e2e_pubkey,
                    nonce: nonce.nonce,
                });
                continue;
            }
            retained.push_back(nonce);
        }
        *pool = retained;
        Ok(selected)
    }

    fn record_nonce_candidates(
        &self,
        chute_id: &str,
        nonce_ttl: u64,
        candidates: Vec<ChutesNonceCandidate>,
    ) -> usize {
        let expires_at = Instant::now() + Duration::from_secs(nonce_ttl);
        let mut cache = self.cache.lock().unwrap();
        let pool = cache.nonce_pools.entry(chute_id.to_string()).or_default();
        let mut existing = pool
            .iter()
            .map(|nonce| (nonce.instance_id.clone(), nonce.nonce.clone()))
            .collect::<HashSet<_>>();
        let max_nonces = candidates
            .iter()
            .map(|candidate| candidate.nonces.len())
            .max()
            .unwrap_or(0);
        let mut added = 0;
        for nonce_index in 0..max_nonces {
            for candidate in &candidates {
                let Some(nonce) = candidate.nonces.get(nonce_index) else {
                    continue;
                };
                if existing.insert((candidate.instance_id.clone(), nonce.clone())) {
                    added += 1;
                    pool.push_back(ChutesPooledNonce {
                        instance_id: candidate.instance_id.clone(),
                        e2e_pubkey: candidate.e2e_pubkey.clone(),
                        public_key_sha256: candidate.public_key_sha256.clone(),
                        nonce: nonce.clone(),
                        expires_at,
                    });
                }
            }
        }
        added
    }

    #[cfg(test)]
    pub(crate) fn pooled_nonce_count(&self, chute_id: &str) -> usize {
        let now = Instant::now();
        self.cache
            .lock()
            .unwrap()
            .nonce_pools
            .get(chute_id)
            .map(|pool| pool.iter().filter(|nonce| nonce.expires_at > now).count())
            .unwrap_or(0)
    }
}

impl Default for ChutesSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default, Debug)]
struct ChutesSessionCache {
    model_map: HashMap<String, CachedChuteId>,
    nonce_pools: HashMap<String, VecDeque<ChutesPooledNonce>>,
}

#[derive(Debug)]
struct CachedChuteId {
    chute_id: String,
    expires_at: Instant,
}

#[derive(Debug)]
struct ChutesPooledNonce {
    instance_id: String,
    e2e_pubkey: String,
    public_key_sha256: String,
    nonce: String,
    expires_at: Instant,
}

struct ChutesNonceCandidate {
    instance_id: String,
    e2e_pubkey: String,
    public_key_sha256: String,
    nonces: Vec<String>,
}
