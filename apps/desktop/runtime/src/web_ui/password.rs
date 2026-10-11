//! The web UI sign-in password.
//!
//! When none is set the service generates one from the OS random source, as
//! code-server does on first run, and keeps it in the owner-only
//! `credentials.toml` next to the other secrets, so the desktop app and
//! `pap web-ui password show` can show it like the Local API key. Settings
//! reads, state snapshots and diagnostics omit it. Earlier versions kept only
//! an Argon2id hash of a chosen password; such a hash keeps signing in until a
//! new password replaces it. Length is the only rule for a chosen password
//! (NIST SP 800-63B); the upper bound only caps the work of checking it.

use argon2::{
    password_hash::{phc::PasswordHash, PasswordVerifier},
    Argon2,
};
use rand::{rngs::SysRng, TryRng};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const MIN_LENGTH: usize = desktop_core::config::WEB_UI_PASSWORD_MIN_LENGTH;
pub const MAX_LENGTH: usize = 256;
/// Random bytes in a generated password: 128 bits, as 32 hex digits.
const GENERATED_BYTES: usize = 16;

/// What signs a browser in.
#[derive(Clone, PartialEq, Eq)]
pub enum Secret {
    Password(String),
    /// The Argon2id PHC string an earlier version kept instead of the password.
    Hash(String),
}

impl Secret {
    /// Checks `password` in constant time; checking a hash is slow by design.
    pub fn verify(&self, password: &str) -> bool {
        match self {
            // Comparing digests keeps the length from leaking too.
            Self::Password(expected) => Sha256::digest(expected.as_bytes())
                .ct_eq(&Sha256::digest(password.as_bytes()))
                .into(),
            Self::Hash(hash) => {
                password.chars().count() <= MAX_LENGTH
                    && PasswordHash::new(hash).is_ok_and(|parsed| {
                        Argon2::default()
                            .verify_password(password.as_bytes(), &parsed)
                            .is_ok()
                    })
            }
        }
    }
}

/// A new random password.
pub fn generate() -> String {
    let mut bytes = [0_u8; GENERATED_BYTES];
    SysRng
        .try_fill_bytes(&mut bytes)
        .expect("the system random source failed");
    hex::encode(bytes)
}

/// Checks a chosen password's length in characters.
pub fn validate(password: &str) -> Result<(), String> {
    let length = password.chars().count();
    if length < MIN_LENGTH {
        return Err(format!(
            "Web UI password must be at least {MIN_LENGTH} characters"
        ));
    }
    if length > MAX_LENGTH {
        return Err(format!(
            "Web UI password must be at most {MAX_LENGTH} characters"
        ));
    }
    Ok(())
}

/// Whether `hash` is an Argon2id PHC string [`Secret::Hash`] can verify against.
pub fn is_hash(hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|parsed| parsed.algorithm.as_str() == "argon2id")
}

/// `correct horse battery` as an earlier version hashed it.
#[cfg(test)]
pub(crate) const LEGACY_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$bbvIGR1pjcKBaQs3iaxvTA$A3RI8HTsymaM22PXaN/OJPf67Q+5KYoC4pkEskhu70Y";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_verify_only_themselves() {
        let secret = Secret::Password("correct horse battery".into());
        assert!(secret.verify("correct horse battery"));
        assert!(!secret.verify("correct horse battery "));
        assert!(!secret.verify(""));
    }

    #[test]
    fn hashes_from_earlier_versions_keep_verifying() {
        let secret = Secret::Hash(LEGACY_HASH.into());
        assert!(is_hash(LEGACY_HASH) && !is_hash("not a hash"));
        assert!(secret.verify("correct horse battery"));
        assert!(!secret.verify("correct horse battery "));
        assert!(!Secret::Hash("not a hash".into()).verify("correct horse battery"));
    }

    #[test]
    fn generated_passwords_are_long_random_and_valid() {
        let first = generate();
        assert_eq!(first.len(), GENERATED_BYTES * 2);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, generate());
        assert!(validate(&first).is_ok());
    }

    #[test]
    fn length_is_the_only_rule() {
        assert!(validate("elevenchars").is_err());
        assert!(validate("twelve chars").is_ok());
        // Characters, not bytes, count toward the minimum.
        assert!(validate(&"é".repeat(MIN_LENGTH - 1)).is_err());
        assert!(validate(&"a".repeat(MAX_LENGTH)).is_ok());
        assert!(validate(&"a".repeat(MAX_LENGTH + 1)).is_err());
    }
}
