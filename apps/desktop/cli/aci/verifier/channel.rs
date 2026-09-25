//! §9.1(6) channel selection: which attested TLS key clients of this
//! deployment pin, as the report declares it (§4.2 `downstream_tls_binding`).
//!
//! Follows the gateway's `declared_tls_channel_bindings`, so both verifiers
//! pin the same entry for the same report.

use serde_json::Value;

use crate::aci::types::WorkloadKeyset;

#[derive(Debug, thiserror::Error)]
pub(super) enum ChannelBindingError {
    #[error("the keyset publishes no TLS key")]
    MissingTlsSpkiBinding,
    #[error(
        "the report declares no downstream_tls_binding for its domain-scoped TLS keys (spec 4.2)"
    )]
    MissingDownstreamTlsBinding,
    #[error("invalid downstream TLS binding: {0}")]
    InvalidDownstreamTlsBinding(String),
    #[error("downstream_tls_binding names {reported:?}, but the channel is to {expected:?}")]
    DownstreamTlsBindingHostMismatch { reported: String, expected: String },
    #[error("downstream_tls_binding names a TLS key the keyset does not attest for its domain")]
    DownstreamTlsBindingNotInKeyset,
}

struct SelectedDownstreamTlsBinding {
    domain: String,
    spki_sha256: String,
}

/// The SPKI digests (lowercase hex) a client of `origin` pins: every attested
/// TLS key when none is domain-scoped, otherwise exactly the entry
/// `downstream_tls_binding` selects, which must be attested for the origin's
/// host.
pub(super) fn declared_tls_pins(
    keyset: &WorkloadKeyset,
    evidence: &Value,
    origin: &str,
) -> Result<Vec<String>, ChannelBindingError> {
    let tls_public_keys = &keyset.tls_public_keys;
    if tls_public_keys.is_empty() {
        return Err(ChannelBindingError::MissingTlsSpkiBinding);
    }

    let has_domain_scoped_keys = tls_public_keys.iter().any(|key| key.domain.is_some());
    if !has_domain_scoped_keys {
        return tls_public_keys
            .iter()
            .map(|key| {
                normalize_sha256_hex(&key.spki_sha256_hex).map_err(|e| {
                    ChannelBindingError::InvalidDownstreamTlsBinding(format!(
                        "invalid keyset TLS SPKI digest: {e}"
                    ))
                })
            })
            .collect();
    }

    let selected = selected_downstream_tls_binding(evidence)?;
    let origin_domain = origin_host_domain(origin)?;
    if selected.domain != origin_domain {
        return Err(ChannelBindingError::DownstreamTlsBindingHostMismatch {
            reported: selected.domain,
            expected: origin_domain,
        });
    }

    // §3.1 decides which entries apply to the hostname; the report's
    // declaration then narrows them to one.
    for key in keyset.tls_keys_for_host(&selected.domain) {
        let key_spki = normalize_sha256_hex(&key.spki_sha256_hex).map_err(|e| {
            ChannelBindingError::InvalidDownstreamTlsBinding(format!(
                "invalid keyset TLS SPKI digest: {e}"
            ))
        })?;
        if key_spki == selected.spki_sha256 {
            return Ok(vec![selected.spki_sha256]);
        }
    }

    Err(ChannelBindingError::DownstreamTlsBindingNotInKeyset)
}

fn selected_downstream_tls_binding(
    evidence: &Value,
) -> Result<SelectedDownstreamTlsBinding, ChannelBindingError> {
    let binding = evidence
        .get("downstream_tls_binding")
        .ok_or(ChannelBindingError::MissingDownstreamTlsBinding)?;
    let domain = binding
        .get("domain")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ChannelBindingError::InvalidDownstreamTlsBinding(
                "downstream_tls_binding.domain must be a string".to_string(),
            )
        })?;
    let spki_sha256 = binding
        .get("spki_sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ChannelBindingError::InvalidDownstreamTlsBinding(
                "downstream_tls_binding.spki_sha256 must be a string".to_string(),
            )
        })?;
    Ok(SelectedDownstreamTlsBinding {
        domain: normalize_tls_domain(domain).map_err(|e| {
            ChannelBindingError::InvalidDownstreamTlsBinding(format!(
                "invalid downstream_tls_binding.domain: {e}"
            ))
        })?,
        spki_sha256: normalize_sha256_hex(spki_sha256).map_err(|e| {
            ChannelBindingError::InvalidDownstreamTlsBinding(format!(
                "invalid downstream_tls_binding.spki_sha256: {e}"
            ))
        })?,
    })
}

fn origin_host_domain(origin: &str) -> Result<String, ChannelBindingError> {
    let url = url::Url::parse(origin).map_err(|e| {
        ChannelBindingError::InvalidDownstreamTlsBinding(format!(
            "invalid base URL {origin:?}: {e}"
        ))
    })?;
    let host = url.host_str().ok_or_else(|| {
        ChannelBindingError::InvalidDownstreamTlsBinding(format!("base URL {origin:?} has no host"))
    })?;
    normalize_tls_domain(host).map_err(|e| {
        ChannelBindingError::InvalidDownstreamTlsBinding(format!(
            "invalid base URL host {host:?}: {e}"
        ))
    })
}

fn normalize_tls_domain(raw: &str) -> Result<String, String> {
    let domain = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty()
        || domain.contains('/')
        || domain.contains(':')
        || domain.contains('=')
        || domain.contains(',')
        || domain.chars().any(char::is_whitespace)
    {
        return Err(format!("invalid TLS domain {raw:?}"));
    }
    Ok(domain)
}

fn normalize_sha256_hex(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.len() != 64 || !value.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return Err("expected 64 hex characters".to_string());
    }
    Ok(value.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::aci::types::TlsSpki;

    fn keyset_with_tls(tls_public_keys: Vec<TlsSpki>) -> WorkloadKeyset {
        WorkloadKeyset {
            subject: None,
            not_after: u64::MAX,
            receipt_signing_keys: Vec::new(),
            e2ee_public_keys: Vec::new(),
            tls_public_keys,
        }
    }

    fn tls(domain: Option<&str>, spki: String) -> TlsSpki {
        TlsSpki {
            domain: domain.map(str::to_string),
            spki_sha256_hex: spki,
        }
    }

    #[test]
    fn service_wide_keys_are_all_pinned() {
        let keyset = keyset_with_tls(vec![tls(None, "AA".repeat(32)), tls(None, "bb".repeat(32))]);

        let pins = declared_tls_pins(&keyset, &json!({}), "https://gateway.example").unwrap();

        assert_eq!(pins, vec!["aa".repeat(32), "bb".repeat(32)]);
    }

    #[test]
    fn domain_scoped_keys_pin_the_declared_entry_for_the_origin_host() {
        let keyset = keyset_with_tls(vec![
            tls(Some("api.example.com"), "AA".repeat(32)),
            tls(Some("chat.example.com"), "bb".repeat(32)),
        ]);
        let evidence = json!({
            "downstream_tls_binding": {
                "domain": "API.EXAMPLE.COM.",
                "spki_sha256": "AA".repeat(32),
            }
        });

        let pins = declared_tls_pins(&keyset, &evidence, "https://api.example.com").unwrap();

        assert_eq!(pins, vec!["aa".repeat(32)]);
    }

    #[test]
    fn domain_scoped_keys_without_a_declared_entry_fail() {
        let keyset = keyset_with_tls(vec![tls(Some("api.example.com"), "aa".repeat(32))]);

        let err = declared_tls_pins(&keyset, &json!({}), "https://api.example.com").unwrap_err();

        assert!(matches!(
            err,
            ChannelBindingError::MissingDownstreamTlsBinding
        ));
    }

    #[test]
    fn a_declared_entry_for_another_host_fails() {
        let keyset = keyset_with_tls(vec![
            tls(Some("api.example.com"), "aa".repeat(32)),
            tls(Some("chat.example.com"), "bb".repeat(32)),
        ]);
        let evidence = json!({
            "downstream_tls_binding": {
                "domain": "chat.example.com",
                "spki_sha256": "bb".repeat(32),
            }
        });

        let err = declared_tls_pins(&keyset, &evidence, "https://api.example.com").unwrap_err();

        assert!(matches!(
            err,
            ChannelBindingError::DownstreamTlsBindingHostMismatch { .. }
        ));
    }

    #[test]
    fn a_declared_entry_outside_the_keyset_fails() {
        let keyset = keyset_with_tls(vec![tls(Some("api.example.com"), "aa".repeat(32))]);
        let evidence = json!({
            "downstream_tls_binding": {
                "domain": "api.example.com",
                "spki_sha256": "bb".repeat(32),
            }
        });

        let err = declared_tls_pins(&keyset, &evidence, "https://api.example.com").unwrap_err();

        assert!(matches!(
            err,
            ChannelBindingError::DownstreamTlsBindingNotInKeyset
        ));
    }
}
