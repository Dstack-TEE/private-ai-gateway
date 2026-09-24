//! One-time login codes and expiring browser sessions for the web UI.
//!
//! Only hashes are stored. Codes are minted over the authenticated IPC
//! endpoint, travel in a URL fragment, and are exchanged once for a session
//! token that the page keeps in `sessionStorage`. Sessions end after an idle
//! period and, however active, after an absolute lifetime. A per-client rate
//! limit bounds unauthenticated requests, which matters once the listener
//! leaves loopback.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const CODE_TTL: Duration = Duration::from_secs(60);
pub const SESSION_IDLE: Duration = Duration::from_secs(60 * 60);
/// Open pages keep a session active, so this bounds every session regardless.
pub const SESSION_LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);
const MAX_CODES: usize = 16;
const MAX_SESSIONS: usize = 16;

type Digested = [u8; 32];

#[derive(Default)]
pub struct Auth(Mutex<Entries>);

#[derive(Default)]
struct Entries {
    /// Code digest and expiry.
    codes: Vec<(Digested, Instant)>,
    sessions: Vec<Session>,
}

struct Session {
    digest: Digested,
    created: Instant,
    used: Instant,
}

impl Session {
    fn live(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.used) < SESSION_IDLE
            && now.saturating_duration_since(self.created) < SESSION_LIFETIME
    }
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

    /// Ends one session, as when its page signs out.
    pub fn revoke(&self, token: &str) {
        if let Ok(mut entries) = self.0.lock() {
            let wanted = digest(token);
            if let Some(index) = position(
                entries.sessions.iter().map(|session| &session.digest),
                &wanted,
            ) {
                entries.sessions.swap_remove(index);
            }
        }
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
        let index = position(entries.codes.iter().map(|(code, _)| code), &digest(code))?;
        entries.codes.swap_remove(index);
        entries.sessions.retain(|session| session.live(now));
        if entries.sessions.len() >= MAX_SESSIONS {
            entries.sessions.remove(0);
        }
        let token = secret();
        entries.sessions.push(Session {
            digest: digest(&token),
            created: now,
            used: now,
        });
        Some(token)
    }

    fn authorize_at(&self, token: &str, now: Instant) -> bool {
        let Ok(mut entries) = self.0.lock() else {
            return false;
        };
        entries.sessions.retain(|session| session.live(now));
        let wanted = digest(token);
        match position(
            entries.sessions.iter().map(|session| &session.digest),
            &wanted,
        ) {
            Some(index) => {
                entries.sessions[index].used = now;
                true
            }
            None => false,
        }
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
fn position<'a>(entries: impl Iterator<Item = &'a Digested>, wanted: &Digested) -> Option<usize> {
    let mut found = None;
    for (index, candidate) in entries.enumerate() {
        if bool::from(candidate.ct_eq(wanted)) {
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
    fn sessions_end_at_their_absolute_lifetime_despite_activity() {
        let auth = Auth::default();
        let start = Instant::now();
        let token = auth
            .exchange_at(&auth.mint_code_at(start), start)
            .expect("fresh code");
        let mut now = start;
        while now + SESSION_IDLE / 2 < start + SESSION_LIFETIME {
            now += SESSION_IDLE / 2;
            assert!(auth.authorize_at(&token, now));
        }
        assert!(!auth.authorize_at(&token, start + SESSION_LIFETIME));
    }

    #[test]
    fn signing_out_ends_only_that_session() {
        let auth = Auth::default();
        let first = auth.exchange(&auth.mint_code()).expect("fresh code");
        let second = auth.exchange(&auth.mint_code()).expect("fresh code");
        auth.revoke(&first);
        assert!(!auth.authorize(&first));
        assert!(auth.authorize(&second));
        auth.revoke("unknown");
        assert!(auth.authorize(&second));
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
