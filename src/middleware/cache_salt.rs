//! TEE-local, per-user prompt cache partitioning.

use super::request_features::hmac_sha256;

const USER_DOMAIN: &[u8] = b"private-ai-gateway/cache-salt/v1";
const PROVIDER_DOMAIN: &[u8] = b"private-ai-gateway/cache-salt/provider/v1";

/// A separate dstack KMS key, never shared with request-feature hashing.
#[derive(Clone)]
pub struct CacheSaltKey([u8; 32]);

/// A user secret carried internally until the selected provider is known.
/// Neither this secret nor the resulting salt belongs in control reports.
#[derive(Clone)]
pub struct CacheSalt([u8; 32]);

impl std::fmt::Debug for CacheSaltKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CacheSaltKey(..)")
    }
}

impl std::fmt::Debug for CacheSalt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CacheSalt(..)")
    }
}

fn append_framed(message: &mut Vec<u8>, value: &[u8]) {
    // Request bodies are bounded below 4 GiB by the HTTP admission boundary;
    // the other inputs are a decimal i64 or a fixed provider tag.
    let len = u32::try_from(value.len()).expect("cache salt input fits request body limit");
    message.extend_from_slice(&len.to_be_bytes());
    message.extend_from_slice(value);
}

impl CacheSaltKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// A caller salt can subdivide a user's partition, never select another
    /// user's partition. Empty or non-string JSON values are ignored by the
    /// caller before invoking this method.
    pub fn derive(&self, user_id: i64, caller_salt: Option<&str>) -> CacheSalt {
        let mut message = USER_DOMAIN.to_vec();
        append_framed(&mut message, user_id.to_string().as_bytes());
        if let Some(salt) = caller_salt.filter(|salt| !salt.is_empty()) {
            append_framed(&mut message, salt.as_bytes());
        }
        CacheSalt(hmac_sha256(&self.0, &message))
    }
}

impl CacheSalt {
    /// Use the provider's stable serde name for domain separation.
    pub(crate) fn for_provider(&self, provider_tag: &str) -> String {
        let mut message = PROVIDER_DOMAIN.to_vec();
        append_framed(&mut message, provider_tag.as_bytes());
        hex::encode(hmac_sha256(&self.0, &message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_caller_and_provider_partitions_are_stable_and_separate() {
        let key = CacheSaltKey::new([7; 32]);
        let salt = |user, caller, provider| key.derive(user, caller).for_provider(provider);
        let baseline = salt(1, Some("23"), "chutes");
        // Independently generated with Python's hmac.digest and struct.pack(">I").
        assert_eq!(
            baseline,
            "cd73c21b138d063283ee6e60ae21ed9b9ea5c96c110c218503757b140e024e68"
        );
        assert_ne!(baseline, salt(2, Some("23"), "chutes"));
        assert_ne!(baseline, salt(12, Some("3"), "chutes"));
        assert_ne!(baseline, salt(1, Some("24"), "chutes"));
        assert_ne!(baseline, salt(1, Some("23"), "privatemode"));
        assert_ne!(
            baseline,
            CacheSaltKey::new([8; 32])
                .derive(1, Some("23"))
                .for_provider("chutes")
        );
        assert_eq!(salt(1, None, "chutes"), salt(1, Some(""), "chutes"));
        assert_eq!(format!("{key:?}"), "CacheSaltKey(..)");
        assert_eq!(format!("{:?}", key.derive(1, None)), "CacheSalt(..)");
    }
}
