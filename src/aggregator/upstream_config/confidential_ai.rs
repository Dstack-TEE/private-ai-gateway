//! Reviewed c8s release pins; no release is implicitly trusted by the adapter.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha384};
use x509_parser::prelude::{FromDer, SubjectPublicKeyInfo};

use super::UpstreamConfigError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfidentialAiConfig {
    pub release: String,
    pub mrtd: String,
    pub rtmr1: String,
    pub rtmr2: String,
    pub rtmr3: String,
    /// Exact PEM bytes are the RTMR3 seed input, including its final newline.
    pub operator_key_pem: String,
    pub workload: String,
    pub allowlist_sha256: Vec<String>,
    pub accepted_models: Vec<String>,
}

impl ConfidentialAiConfig {
    pub(super) fn validate(
        &self,
        models: &BTreeMap<String, String>,
    ) -> Result<(), UpstreamConfigError> {
        let invalid = |message: &str| {
            UpstreamConfigError::InvalidConfig(format!("confidential_ai: {message}"))
        };
        let valid_hex = |value: &str, len: usize| {
            value.len() == len
                && value
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        };
        if self.release.trim().is_empty()
            || ![&self.mrtd, &self.rtmr1, &self.rtmr2, &self.rtmr3]
                .iter()
                .all(|s| valid_hex(s, 96))
        {
            return Err(invalid(
                "release and four 96-character lowercase measurement pins are required",
            ));
        }
        let mut reader = self.operator_key_pem.as_bytes();
        let key = rustls_pemfile::public_keys(&mut reader)
            .next()
            .ok_or_else(|| invalid("operator_key_pem must contain a P-256 public key"))?
            .map_err(|_| invalid("operator_key_pem is malformed"))?;
        let (remaining, spki) = SubjectPublicKeyInfo::from_der(key.as_ref())
            .map_err(|_| invalid("operator_key_pem is not a public key"))?;
        let curve = spki
            .algorithm
            .parameters
            .as_ref()
            .and_then(|p| p.as_oid().ok());
        if !remaining.is_empty()
            || spki.algorithm.algorithm.to_id_string() != "1.2.840.10045.2.1"
            || curve.is_none_or(|oid| oid.to_id_string() != "1.2.840.10045.3.1.7")
            || spki.subject_public_key.data.len() != 65
            || spki.subject_public_key.data.first() != Some(&4)
        {
            return Err(invalid("operator_key_pem must be P-256 for ES256 routes"));
        }
        let anchor = Sha384::digest(self.operator_key_pem.as_bytes());
        let mut seed = Sha384::new();
        seed.update([0u8; 48]);
        seed.update(anchor);
        if self.rtmr3 != hex::encode(seed.finalize()) {
            return Err(invalid("rtmr3 must equal seed(operator_key_pem)"));
        }
        let workload = self.workload.as_bytes();
        if workload.is_empty()
            || workload.len() > 63
            || !workload[0].is_ascii_alphanumeric()
            || !workload
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
        {
            return Err(invalid("workload must be a c8s matched-workload name"));
        }
        if self.allowlist_sha256.is_empty()
            || !self.allowlist_sha256.iter().all(|v| {
                v.strip_prefix("sha256:")
                    .is_some_and(|hex| valid_hex(hex, 64))
            })
        {
            return Err(invalid(
                "allowlist_sha256 requires sha256:<64 lowercase hex> pins",
            ));
        }
        // The legacy V4-Flash-0731 name silently aliases this model upstream.
        const MODEL: &str = "deepseek-ai/DeepSeek-V4.1-Flash";
        if self.accepted_models != [MODEL]
            || models.values().any(|v| !self.accepted_models.contains(v))
            || models.keys().any(|v| v.contains("DeepSeek-V4-Flash-0731"))
        {
            return Err(invalid(
                "only deepseek-ai/DeepSeek-V4.1-Flash is accepted; legacy aliases are forbidden",
            ));
        }
        Ok(())
    }
}
