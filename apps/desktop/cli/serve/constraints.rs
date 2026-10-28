//! Prepare bytes, carrier headers and the effective audit policy together.

use crate::aci::types::{
    ServiceCapabilities, AUDIO_TRANSCRIPTIONS_PATH, MAX_MULTIPART_PROVIDER_BYTES,
    MULTIPART_PROVIDER_HEADER, PROVIDER_ACI_SESSION_IDS, PROVIDER_ACI_VERIFIED,
};
use agent_bridge::proxy::{check_carrier_headers, multipart};
use axum::body::Bytes;
use axum::http::HeaderMap;
use serde_json::{json, Value};

pub(super) struct PreparedRequest {
    pub body: Bytes,
    pub headers: HeaderMap,
    pub policy: EffectivePolicy,
}

#[derive(Default)]
pub(super) struct EffectivePolicy {
    pub pinned_sessions: Vec<String>,
    pub requires_verified: bool,
    pub local_policy_applied: bool,
    pub policy_derived_pins: bool,
}

pub(super) async fn prepare_request(
    body: Bytes,
    headers: &HeaderMap,
    path: &str,
    enforce_verified: bool,
    pins: &[String],
    capabilities: &ServiceCapabilities,
) -> Result<PreparedRequest, String> {
    let mut headers = headers.clone();
    let content_type = check_carrier_headers(&headers)?;
    let carriers: Vec<_> = headers.get_all(MULTIPART_PROVIDER_HEADER).iter().collect();
    if carriers.len() > 1 {
        return Err("duplicate X-ACI-Provider headers".into());
    }
    let carrier = carriers.first().copied();
    let media_type = content_type.split(';').next().unwrap_or("").trim();
    let multipart = media_type.eq_ignore_ascii_case("multipart/form-data");
    let json = media_type.eq_ignore_ascii_case("application/json")
        || (media_type.to_ascii_lowercase().starts_with("application/")
            && media_type.to_ascii_lowercase().ends_with("+json"));
    if carrier.is_some() && (!multipart || path != AUDIO_TRANSCRIPTIONS_PATH) {
        return Err("X-ACI-Provider is only valid on supported multipart endpoints".into());
    }
    let constrained = enforce_verified || !pins.is_empty();
    if multipart && path == AUDIO_TRANSCRIPTIONS_PATH {
        let provider = match carrier {
            Some(value) => {
                if value.as_bytes().len() > MAX_MULTIPART_PROVIDER_BYTES {
                    return Err("X-ACI-Provider exceeds 8192 bytes".into());
                }
                serde_json::from_slice::<Value>(value.as_bytes())
                    .map_err(|_| "X-ACI-Provider must contain a JSON object")?
                    .as_object()
                    .cloned()
                    .ok_or("X-ACI-Provider must contain a JSON object")?
            }
            None => serde_json::Map::new(),
        };
        let (provider, policy) = merge_provider(provider, enforce_verified, pins)?;
        if (constrained || carrier.is_some())
            && !capabilities
                .supported_multipart_constraint_endpoints
                .iter()
                .any(|endpoint| endpoint == path)
        {
            return Err(
                "gateway does not advertise multipart serving constraints for this endpoint".into(),
            );
        }
        scan_multipart_headers(body.clone(), content_type).await?;
        if constrained || carrier.is_some() {
            let value =
                serde_json::to_vec(&provider).map_err(|_| "cannot encode X-ACI-Provider")?;
            if value.len() > MAX_MULTIPART_PROVIDER_BYTES {
                return Err("effective X-ACI-Provider exceeds 8192 bytes".into());
            }
            let value = axum::http::HeaderValue::from_bytes(&value)
                .map_err(|_| "X-ACI-Provider cannot be encoded as an HTTP header")?;
            headers.insert(MULTIPART_PROVIDER_HEADER, value);
        }
        return Ok(PreparedRequest {
            body,
            headers,
            policy,
        });
    }
    if !json {
        if constrained && !body.is_empty() {
            return Err("cannot apply serving constraints to this request media type".into());
        }
        return unconstrained(body, headers, enforce_verified);
    }
    let mut parsed: Value = match serde_json::from_slice(&body) {
        Ok(parsed) => parsed,
        Err(_) if !constrained => return unconstrained(body, headers, enforce_verified),
        Err(_) => return Err("JSON request body must be valid JSON".into()),
    };
    let Some(members) = parsed.as_object_mut() else {
        if !constrained {
            return unconstrained(body, headers, enforce_verified);
        }
        return Err("JSON request body must have an object top level".into());
    };
    let provider = match members.get("provider") {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(provider)) => provider.clone(),
        Some(_) if !constrained => return unconstrained(body, headers, enforce_verified),
        Some(_) => return Err("JSON request provider must be an object or null".into()),
    };
    let (provider, policy) = merge_provider(provider, enforce_verified, pins)?;
    if policy.requires_verified {
        members.insert("provider".into(), Value::Object(provider));
        let outgoing = serde_json::to_vec(&parsed).map_err(|_| "cannot encode JSON request")?;
        return Ok(PreparedRequest {
            body: outgoing.into(),
            headers,
            policy,
        });
    }
    unconstrained(body, headers, enforce_verified)
}

fn unconstrained(
    body: Bytes,
    headers: HeaderMap,
    enforce_verified: bool,
) -> Result<PreparedRequest, String> {
    Ok(PreparedRequest {
        body,
        headers,
        policy: EffectivePolicy {
            requires_verified: enforce_verified,
            ..Default::default()
        },
    })
}

fn merge_provider(
    mut provider: serde_json::Map<String, Value>,
    enforce_verified: bool,
    pins: &[String],
) -> Result<(serde_json::Map<String, Value>, EffectivePolicy), String> {
    for key in provider.keys() {
        if key.starts_with("aci_")
            && key != PROVIDER_ACI_VERIFIED
            && key != PROVIDER_ACI_SESSION_IDS
        {
            return Err("unknown aci_ serving constraint".into());
        }
    }
    let verified = match provider.get(PROVIDER_ACI_VERIFIED) {
        None => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return Err("provider.aci_verified must be a boolean".into()),
    };
    let supplied = match provider.get(PROVIDER_ACI_SESSION_IDS) {
        None => Vec::new(),
        Some(value) => {
            let ids = value
                .as_array()
                .ok_or("provider.aci_session_ids must be an array of strings")?;
            if ids.is_empty() {
                return Err("provider.aci_session_ids must not be empty".into());
            }
            let mut supplied = Vec::new();
            for id in ids {
                let id = id
                    .as_str()
                    .ok_or("provider.aci_session_ids must be an array of strings")?;
                if id.len() != 64
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err("provider.aci_session_ids must contain lowercase 64-hex ids".into());
                }
                if !supplied.iter().any(|item| item == id) {
                    supplied.push(id.to_string());
                }
            }
            supplied
        }
    };
    if !supplied.is_empty() && verified == Some(false) {
        return Err("provider.aci_session_ids cannot be combined with aci_verified=false".into());
    }
    let policy_derived_pins = !pins.is_empty() && supplied.is_empty();
    let pinned_sessions = if pins.is_empty() {
        supplied.clone()
    } else if supplied.is_empty() {
        pins.to_vec()
    } else {
        supplied
            .iter()
            .filter(|id| pins.contains(id))
            .cloned()
            .collect()
    };
    if !pins.is_empty() && !supplied.is_empty() && pinned_sessions.is_empty() {
        return Err("request pins and local accepted session set are disjoint".into());
    }
    let requires_verified =
        enforce_verified || verified == Some(true) || !pinned_sessions.is_empty();
    let caller_requires_verified = verified == Some(true) || !supplied.is_empty();
    let local_policy_applied =
        (requires_verified && !caller_requires_verified) || pinned_sessions != supplied;
    if requires_verified {
        provider.insert(PROVIDER_ACI_VERIFIED.into(), Value::Bool(true));
    }
    if !pinned_sessions.is_empty() {
        provider.insert(PROVIDER_ACI_SESSION_IDS.into(), json!(pinned_sessions));
    }
    Ok((
        provider,
        EffectivePolicy {
            pinned_sessions,
            requires_verified,
            local_policy_applied,
            policy_derived_pins,
        },
    ))
}

/// The standard parser scans boundaries/part headers and skips field data.
/// No audio decoding, field collection or multipart re-encoding takes place.
async fn scan_multipart_headers(body: Bytes, content_type: &str) -> Result<(), String> {
    let mut multipart = multipart(body, content_type, Default::default())?;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| "malformed multipart body")?
    {
        if field.name() == Some("provider") {
            return Err("multipart provider form parts are forbidden; use X-ACI-Provider".into());
        }
    }
    Ok(())
}
