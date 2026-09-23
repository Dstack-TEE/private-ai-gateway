//! One-time login codes and idle-expiring browser sessions for the web UI.
//!
//! Only hashes are stored. Codes are minted over the authenticated IPC
//! endpoint, travel in a URL fragment, and are exchanged once for a session
//! token that the page keeps in `sessionStorage`. A token bucket bounds
//! unauthenticated requests, which matters once the listener leaves loopback.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};

pub const CODE_TTL: Duration = Duration::from_secs(60);
pub const SESSION_IDLE: Duration = Duration::from_secs(60 * 60);
const MAX_CODES: usize = 16;
const MAX_SESSIONS: usize = 16;
/// Unauthenticated requests allowed in a burst, then one per `THROTTLE_REFILL`.
pub const THROTTLE_BURST: u32 = 10;
pub const THROTTLE_REFILL: Duration = Duration::from_secs(3);

type Digested = [u8; 32];

#[derive(Default)]
pub struct Auth(Mutex<Entries>);

#[derive(Default)]
struct Entries {
    /// Code digest and expiry.
    codes: Vec<(Digested, Instant)>,
    /// Session digest and last activity.
    sessions: Vec<(Digested, Instant)>,
}

impl Auth {
    pub fn mint_code(&self) -> String {
        self.mint_code_at(Instant::now())
    }

    /// Consumes a live code and opens a session. Expired or reused codes fail.
    pub fn exchange(&self, code: &str) -> Option<String> {
        self.exchange_at(code, Instant::now())
    }

    /// Accepts a live session and records activity.
    pub fn authorize(&self, token: &str) -> bool {
        self.authorize_at(token, Instant::now())
    }

    pub fn revoke_all(&self) {
        if let Ok(mut entries) = self.0.lock() {
            *entries = Entries::default();
        }
    }

    fn mint_code_at(&self, now: Instant) -> String {
        let code = secret();
        if let Ok(mut entries) = self.0.lock() {
            entries.codes.retain(|(_, expires)| *expires > now);
            if entries.codes.len() >= MAX_CODES {
                entries.codes.remove(0);
            }
            entries.codes.push((digest(&code), now + CODE_TTL));
        }
        code
    }

    fn exchange_at(&self, code: &str, now: Instant) -> Option<String> {
        let mut entries = self.0.lock().ok()?;
        entries.codes.retain(|(_, expires)| *expires > now);
        let index = position(&entries.codes, &digest(code))?;
        entries.codes.swap_remove(index);
        entries
            .sessions
            .retain(|(_, used)| now.saturating_duration_since(*used) < SESSION_IDLE);
        if entries.sessions.len() >= MAX_SESSIONS {
            entries.sessions.remove(0);
        }
        let token = secret();
        entries.sessions.push((digest(&token), now));
        Some(token)
    }

    fn authorize_at(&self, token: &str, now: Instant) -> bool {
        let Ok(mut entries) = self.0.lock() else {
            return false;
        };
        entries
            .sessions
            .retain(|(_, used)| now.saturating_duration_since(*used) < SESSION_IDLE);
        match position(&entries.sessions, &digest(token)) {
            Some(index) => {
                entries.sessions[index].1 = now;
                true
            }
            None => false,
        }
    }
}

/// A per-process token bucket shared by code exchanges and rejected API
/// requests. Codes are 256-bit, so this bounds request volume rather than
/// guessing odds; signed-in sessions never draw from it.
pub struct Throttle(Mutex<Bucket>);

struct Bucket {
    tokens: u32,
    refilled: Instant,
}

impl Default for Throttle {
    fn default() -> Self {
        Self(Mutex::new(Bucket {
            tokens: THROTTLE_BURST,
            refilled: Instant::now(),
        }))
    }
}

impl Throttle {
    /// Takes one token, or returns false when the caller must back off.
    pub fn allow(&self) -> bool {
        self.allow_at(Instant::now())
    }

    fn allow_at(&self, now: Instant) -> bool {
        let Ok(mut bucket) = self.0.lock() else {
            return false;
        };
        let earned = now.saturating_duration_since(bucket.refilled).as_millis()
            / THROTTLE_REFILL.as_millis();
        let earned = u32::try_from(earned).unwrap_or(u32::MAX);
        if earned > 0 {
            bucket.tokens = bucket.tokens.saturating_add(earned).min(THROTTLE_BURST);
            bucket.refilled = if bucket.tokens == THROTTLE_BURST {
                now
            } else {
                bucket.refilled + THROTTLE_REFILL * earned
            };
        }
        if bucket.tokens == 0 {
            return false;
        }
        bucket.tokens -= 1;
        true
    }
}

fn secret() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn digest(value: &str) -> Digested {
    Sha256::digest(value.as_bytes()).into()
}

/// Compares every entry without early exit so timing does not reveal matches.
fn position(entries: &[(Digested, Instant)], wanted: &Digested) -> Option<usize> {
    let mut found = None;
    for (index, (candidate, _)) in entries.iter().enumerate() {
        let difference = candidate
            .iter()
            .zip(wanted)
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            });
        if difference == 0 {
            found = Some(index);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_single_use() {
        let auth = Auth::default();
        let code = auth.mint_code();
        let token = auth.exchange(&code).expect("fresh code");
        assert!(auth.authorize(&token));
        assert_eq!(auth.exchange(&code), None);
        assert_eq!(auth.exchange("guess"), None);
    }

    #[test]
    fn expired_codes_are_rejected() {
        let auth = Auth::default();
        let start = Instant::now();
        let code = auth.mint_code_at(start);
        assert_eq!(auth.exchange_at(&code, start + CODE_TTL), None);
    }

    #[test]
    fn sessions_expire_when_idle_and_activity_extends_them() {
        let auth = Auth::default();
        let start = Instant::now();
        let token = auth
            .exchange_at(&auth.mint_code_at(start), start)
            .expect("fresh code");
        let later = start + SESSION_IDLE - Duration::from_secs(1);
        assert!(auth.authorize_at(&token, later));
        assert!(auth.authorize_at(&token, later + SESSION_IDLE - Duration::from_secs(1)));
        assert!(!auth.authorize_at(&token, later + SESSION_IDLE * 3));
        assert!(!auth.authorize_at("missing", start));
    }

    #[test]
    fn throttle_allows_a_burst_then_refills_slowly() {
        let throttle = Throttle::default();
        let start = Instant::now();
        for _ in 0..THROTTLE_BURST {
            assert!(throttle.allow_at(start));
        }
        assert!(!throttle.allow_at(start));
        assert!(!throttle.allow_at(start + THROTTLE_REFILL / 2));
        assert!(throttle.allow_at(start + THROTTLE_REFILL));
        assert!(!throttle.allow_at(start + THROTTLE_REFILL));
        let later = start + THROTTLE_REFILL * 100;
        for _ in 0..THROTTLE_BURST {
            assert!(throttle.allow_at(later));
        }
        assert!(!throttle.allow_at(later));
    }

    #[test]
    fn revocation_ends_sessions_and_pending_codes() {
        let auth = Auth::default();
        let pending = auth.mint_code();
        let token = auth.exchange(&auth.mint_code()).expect("fresh code");
        auth.revoke_all();
        assert!(!auth.authorize(&token));
        assert_eq!(auth.exchange(&pending), None);
    }
}
