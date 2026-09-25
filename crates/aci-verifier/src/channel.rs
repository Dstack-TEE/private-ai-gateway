//! §9.1(6) channel selection: which attested TLS key a client of this
//! deployment pins, as the report declares it (§4.2 `downstream_tls_binding`).

use aci_protocol::receipt::ChannelBinding;
use aci_protocol::types::WorkloadKeyset;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ChannelBindingError {
    #[error("verified ACI/dstack upstream report did not publish a TLS SPKI binding")]
    MissingTlsSpkiBinding,
    #[error("verified ACI/dstack upstream report did not select a downstream TLS binding")]
    MissingDownstreamTlsBinding,
    #[error("invalid downstream TLS binding: {0}")]
    InvalidDownstreamTlsBinding(String),
    #[error(
        "selected downstream TLS binding domain {reported:?} does not match upstream host {expected:?}"
    )]
    DownstreamTlsBindingHostMismatch { reported: String, expected: String },
    #[error("selected downstream TLS binding is not present in the attested keyset")]
    DownstreamTlsBindingNotInKeyset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedDownstreamTlsBinding {
    domain: String,
    spki_sha256: String,
}

pub(crate) fn declared_tls_channel_bindings(
    keyset: &WorkloadKeyset,
    evidence: &Value,
    origin: &str,
) -> Result<Vec<ChannelBinding>, ChannelBindingError> {
    let tls_public_keys = &keyset.tls_public_keys;
    if tls_public_keys.is_empty() {
        return Err(ChannelBindingError::MissingTlsSpkiBinding);
    }

    let has_domain_scoped_keys = tls_public_keys.iter().any(|key| key.domain.is_some());
    if !has_domain_scoped_keys {
        return tls_public_keys
            .iter()
            .map(|key| {
                Ok(ChannelBinding::TlsSpkiSha256 {
                    origin: origin.to_string(),
                    spki_sha256: normalize_sha256_hex(&key.spki_sha256_hex).map_err(|e| {
                        ChannelBindingError::InvalidDownstreamTlsBinding(format!(
                            "invalid keyset TLS SPKI digest: {e}"
                        ))
                    })?,
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

    // §3.1 decides which entries apply to the hostname; the deployment then
    // narrows to the entry the report declares.
    for key in keyset.tls_keys_for_host(&selected.domain) {
        let key_spki = normalize_sha256_hex(&key.spki_sha256_hex).map_err(|e| {
            ChannelBindingError::InvalidDownstreamTlsBinding(format!(
                "invalid keyset TLS SPKI digest: {e}"
            ))
        })?;
        if key_spki == selected.spki_sha256 {
            return Ok(vec![ChannelBinding::TlsSpkiSha256 {
                origin: origin.to_string(),
                spki_sha256: selected.spki_sha256,
            }]);
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
            "invalid report base URL {origin:?}: {e}"
        ))
    })?;
    let host = url.host_str().ok_or_else(|| {
        ChannelBindingError::InvalidDownstreamTlsBinding(format!(
            "report base URL {origin:?} has no host"
        ))
    })?;
    normalize_tls_domain(host).map_err(|e| {
        ChannelBindingError::InvalidDownstreamTlsBinding(format!(
            "invalid report base URL host {host:?}: {e}"
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
