//! The sign-in password and expiring browser sessions for the web UI.
//!
//! Session tokens are kept only as SHA-256 digests. Signing in sets the token
//! in an `HttpOnly` cookie; the page never sees it. Sessions end after an idle
//! period and, however active, after an absolute lifetime. A per-client rate limit bounds sign-in attempts
//! and other unauthenticated requests.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::{rngs::SysRng, TryRng};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::password::Secret;

pub const SESSION_IDLE: Duration = Duration::from_secs(60 * 60);
/// Open pages keep a session active, so this bounds every session regardless.
pub const SESSION_LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);
const MAX_SESSIONS: usize = 16;

type Digested = [u8; 32];

#[derive(Default)]
pub struct Auth {
    sessions: Mutex<Vec<Session>>,
    /// What signs a browser in, when set.
    password: Mutex<Option<Secret>>,
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
    /// Checks the password and opens a session. Checking a hash from an earlier
    /// version is slow by design; run it off the async runtime.
    pub fn sign_in(&self, password: &str) -> Option<String> {
        self.sign_in_at(password, Instant::now())
    }

    pub fn has_password(&self) -> bool {
        self.password.lock().is_ok_and(|secret| secret.is_some())
    }

    /// Replaces the password and ends every session.
    pub fn set_password(&self, secret: Option<Secret>) {
        if let Ok(mut password) = self.password.lock() {
            *password = secret;
            self.revoke_all();
        }
    }

    #[cfg(test)]
    pub fn open_session(&self) -> Option<String> {
        self.open_session_at(Instant::now())
    }

    /// Accepts a live session and records activity.
    pub fn authorize(&self, token: &str) -> bool {
        self.authorize_at(token, Instant::now())
    }

    /// Ends one session, as when its page signs out.
    pub fn revoke(&self, token: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(index) = position(&sessions, &digest(token)) {
                sessions.swap_remove(index);
            }
        }
    }

    pub fn revoke_all(&self) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.clear();
        }
    }

    fn sign_in_at(&self, password: &str, now: Instant) -> Option<String> {
        let secret = self.password.lock().ok()?.clone()?;
        if !secret.verify(password) {
            return None;
        }
        // A password changed during verification must not open a session.
        if self.password.lock().ok()?.as_ref() != Some(&secret) {
            return None;
        }
        self.open_session_at(now)
    }

    fn open_session_at(&self, now: Instant) -> Option<String> {
        let mut sessions = self.sessions.lock().ok()?;
        sessions.retain(|session| session.live(now));
        if sessions.len() >= MAX_SESSIONS {
            sessions.remove(0);
        }
        let token = secret();
        sessions.push(Session {
            digest: digest(&token),
            created: now,
            used: now,
        });
        Some(token)
    }

    fn authorize_at(&self, token: &str, now: Instant) -> bool {
        let Ok(mut sessions) = self.sessions.lock() else {
            return false;
        };
        sessions.retain(|session| session.live(now));
        match position(&sessions, &digest(token)) {
            Some(index) => {
                sessions[index].used = now;
                true
            }
            None => false,
        }
    }
}

fn secret() -> String {
    let mut bytes = [0_u8; 32];
    SysRng
        .try_fill_bytes(&mut bytes)
        .expect("the system random source failed");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn digest(value: &str) -> Digested {
    Sha256::digest(value.as_bytes()).into()
}

/// Compares every entry without early exit so timing does not reveal matches.
fn position(sessions: &[Session], wanted: &Digested) -> Option<usize> {
    let mut found = None;
    for (index, session) in sessions.iter().enumerate() {
        if bool::from(session.digest.ct_eq(wanted)) {
            found = Some(index);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "correct horse battery";

    fn chosen(password: &str) -> Option<Secret> {
        Some(Secret::Password(password.into()))
    }

    fn auth() -> Auth {
        let auth = Auth::default();
        auth.set_password(chosen(PASSWORD));
        auth
    }

    #[test]
    fn only_the_password_opens_sessions() {
        let auth = Auth::default();
        assert!(!auth.has_password());
        assert_eq!(auth.sign_in(PASSWORD), None);
        auth.set_password(chosen(PASSWORD));
        assert!(auth.has_password());
        assert_eq!(auth.sign_in("wrong horse battery"), None);
        let token = auth.sign_in(PASSWORD).expect("password");
        assert!(auth.authorize(&token));
        assert!(!auth.authorize("guess"));
    }

    #[test]
    fn a_hash_from_an_earlier_version_still_signs_in() {
        let auth = Auth::default();
        auth.set_password(Some(Secret::Hash(
            super::super::password::LEGACY_HASH.into(),
        )));
        assert_eq!(auth.sign_in("wrong horse battery"), None);
        assert!(auth.sign_in(PASSWORD).is_some());
    }

    #[test]
    fn changing_or_clearing_the_password_ends_every_session() {
        let auth = auth();
        let token = auth.sign_in(PASSWORD).unwrap();
        auth.set_password(chosen("another long passphrase"));
        assert!(!auth.authorize(&token));
        assert_eq!(auth.sign_in(PASSWORD), None);
        let token = auth.sign_in("another long passphrase").unwrap();
        auth.set_password(None);
        assert!(!auth.authorize(&token));
        assert_eq!(auth.sign_in("another long passphrase"), None);
    }

    #[test]
    fn sessions_expire_when_idle_and_activity_extends_them() {
        let auth = Auth::default();
        let start = Instant::now();
        let token = auth.open_session_at(start).unwrap();
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
        let token = auth.open_session_at(start).unwrap();
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
        let first = auth.open_session().unwrap();
        let second = auth.open_session().unwrap();
        auth.revoke(&first);
        assert!(!auth.authorize(&first));
        assert!(auth.authorize(&second));
        auth.revoke("unknown");
        assert!(auth.authorize(&second));
        auth.revoke_all();
        assert!(!auth.authorize(&second));
    }
}
