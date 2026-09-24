//! The web UI sign-in password.
//!
//! Only an Argon2id PHC string (the crate defaults: 19 MiB, 2 passes, 1 lane,
//! the OWASP-recommended minimum) is kept, in the owner-only preferences file.
//! It never leaves the service: preference reads, state snapshots and
//! diagnostics omit it. Length is the only rule (NIST SP 800-63B); the upper
//! bound only caps hashing work.

use argon2::{
    password_hash::{phc::PasswordHash, PasswordHasher, PasswordVerifier},
    Argon2,
};

pub const MIN_LENGTH: usize = 12;
pub const MAX_LENGTH: usize = 256;

/// Hashes a new password after checking its length in characters.
pub fn hash(password: &str) -> Result<String, String> {
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
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|_| "Web UI password could not be saved".into())
}

/// Checks `password` against a stored hash in constant time; malformed hashes never match.
pub fn verify(hash: &str, password: &str) -> bool {
    password.chars().count() <= MAX_LENGTH
        && PasswordHash::new(hash).is_ok_and(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_verify_only_the_same_password() {
        let hash = hash("correct horse battery").unwrap();
        assert!(hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(!hash.contains("correct horse"));
        assert!(verify(&hash, "correct horse battery"));
        assert!(!verify(&hash, "correct horse battery "));
        assert!(!verify("not a hash", "correct horse battery"));
    }

    #[test]
    fn length_is_the_only_rule() {
        assert!(hash("elevenchars").is_err());
        assert!(hash("twelve chars").is_ok());
        // Characters, not bytes, count toward the minimum.
        assert!(hash(&"é".repeat(MIN_LENGTH - 1)).is_err());
        assert!(hash(&"a".repeat(MAX_LENGTH)).is_ok());
        assert!(hash(&"a".repeat(MAX_LENGTH + 1)).is_err());
    }
}
