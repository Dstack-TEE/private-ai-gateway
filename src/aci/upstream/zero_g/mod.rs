//! 0G Private Computer backend: requests are sealed with 0G's E2EE protocol to
//! a provider the verifier bound, sent through the 0G router pinned to that
//! provider, and released only after the provider's §8 signature verifies.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

mod protocol;

use protocol::{seal_chat_request, verify_signature, ChatSignature, ResponseOpener};

use super::{
    forwarding_client, readiness_client, transport_error, PreparedUpstreamRequest, UpstreamBackend,
    UpstreamBodyStream, UpstreamError, UpstreamRequest, UpstreamResponse, UpstreamStreamResponse,
};
use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};

pub const ZERO_G_PROVIDER: &str = "0g-private-computer";
pub const ZERO_G_E2EE_ALGORITHM: &str = "0g-pc-v1-hpke-x25519-hkdf-sha256-chacha20poly1305";

/// Selects 0G's private tier; set by code on every router call.
pub(crate) const TRUST_MODE_HEADER: &str = "x-0g-provider-trust-mode";
const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";
const SIGNATURE_FETCH_TIMEOUT_SECONDS: u64 = 15;
const MAX_SSE_BUFFER_BYTES: usize = 4 << 20;

/// A verified provider, recorded by the verifier under
/// `provider_claims.instances[<address>]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct VerifiedProvider {
    pub provider_address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_identity: Option<String>,
    /// The serving URL from the provider's on-chain record.
    pub endpoint: String,
    /// The configured upstream model, which is also the on-chain model.
    pub model: String,
    pub signing_address: String,
    /// Hex X25519 key from the verified quote's report_data.
    pub e2ee_public_key: String,
}

/// A provider broker's `/v1` API base (0G's deriveV1Base), HTTPS only.
pub(crate) fn broker_api(endpoint: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(endpoint).map_err(|e| format!("invalid endpoint: {e}"))?;
    if url.scheme() != "https" && !cfg!(test) {
        return Err("provider endpoint is not HTTPS".to_string());
    }
    let path = url.path().trim_end_matches('/');
    let path = path
        .strip_suffix("/chat/completions")
        .or_else(|| path.strip_suffix("/v1"))
        .unwrap_or(path);
    Ok(format!("{}{path}/v1", url.origin().ascii_serialization()))
}

fn rejected(err: impl std::fmt::Display) -> UpstreamError {
    UpstreamError::Transport(format!("0G response rejected: {err}"))
}

fn unverified() -> UpstreamError {
    UpstreamError::Transport("0G transport requires a verified provider binding".to_string())
}

pub struct ZeroGProviderBackend {
    name: String,
    router_base: String,
    api_key: String,
    client: reqwest::Client,
    signature_client: reqwest::Client,
}

impl ZeroGProviderBackend {
    pub fn new_with_timeouts(
        router_base: impl Into<String>,
        api_key: impl Into<String>,
        connect_timeout_seconds: u64,
        read_timeout_seconds: u64,
    ) -> Result<Self, UpstreamError> {
        Ok(Self {
            name: ZERO_G_PROVIDER.to_string(),
            router_base: router_base.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            client: forwarding_client(connect_timeout_seconds, read_timeout_seconds)?,
            signature_client: readiness_client(
                connect_timeout_seconds,
                SIGNATURE_FETCH_TIMEOUT_SECONDS,
            )?,
        })
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Seal the request to the first provider `event` binds for its model and
    /// send it through the router pinned to that provider. Only a 200 response
    /// naming its signature continues; any other status is returned as its
    /// status alone, so provider-controlled bodies never reach the client
    /// unverified.
    async fn send(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<(reqwest::Response, ResponseOpener, SignatureCheck), UpstreamError> {
        if req.request.path.as_deref().unwrap_or(CHAT_COMPLETIONS_PATH) != CHAT_COMPLETIONS_PATH {
            return Err(UpstreamError::InvalidRequest(
                "0G Private Computer serves only /v1/chat/completions".to_string(),
            ));
        }
        let request: Map<String, Value> =
            serde_json::from_slice(&req.request.body).map_err(|e| {
                UpstreamError::InvalidRequest(format!("invalid JSON request body: {e}"))
            })?;
        let model = request
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (provider, enc_pub) = bound_provider(event, model).ok_or_else(|| {
            UpstreamError::ChannelBindingMismatch(format!(
                "no verified 0G provider is bound for model {model:?}"
            ))
        })?;
        let api = broker_api(&provider.endpoint).map_err(UpstreamError::Transport)?;
        let (envelope, opener) = seal_chat_request(request, &enc_pub, &provider.signing_address)?;

        // No caller or route headers are forwarded, so no X-0G-* control can
        // be injected.
        let mut builder = self
            .client
            .post(format!("{}{CHAT_COMPLETIONS_PATH}", self.router_base))
            .bearer_auth(&self.api_key)
            .header(TRUST_MODE_HEADER, "private")
            .header("x-0g-provider-address", &provider.provider_address)
            .header("x-0g-provider-allow-fallbacks", "false")
            .json(&envelope);
        if let Some(identity) = provider
            .provider_identity
            .as_deref()
            .filter(|i| !i.is_empty())
        {
            builder = builder.header("x-0g-provider-identity", identity);
        }
        let response = builder.send().await.map_err(transport_error)?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(UpstreamError::Upstream { status });
        }
        let key = response
            .headers()
            .get("zg-res-key")
            .and_then(|value| value.to_str().ok())
            .filter(|key| {
                (1..=64).contains(&key.len())
                    && key
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            })
            .ok_or_else(|| rejected("response carried no valid ZG-Res-Key"))?;
        let check = SignatureCheck {
            client: self.signature_client.clone(),
            url: format!("{api}/proxy/signature/{key}"),
            signer: provider.signing_address,
            provider: provider.provider_address,
        };
        Ok((response, opener, check))
    }
}

/// The first provider, in preview order, whose recorded key hashes to a
/// binding the verified event still carries (session pinning may have narrowed
/// them), with that key.
fn bound_provider(
    event: &UpstreamVerifiedEvent,
    model: &str,
) -> Option<(VerifiedProvider, [u8; 32])> {
    if event.result != VerificationResult::Verified {
        return None;
    }
    let instances = event.provider_claims.as_ref()?.get("instances")?;
    event.channel_bindings.iter().find_map(|binding| {
        let ChannelBinding::E2eePublicKeySha256 {
            provider,
            key_id: Some(address),
            algorithm,
            public_key_sha256,
        } = binding
        else {
            return None;
        };
        let verified: VerifiedProvider =
            serde_json::from_value(instances.get(address)?.clone()).ok()?;
        let key: [u8; 32] = hex::decode(&verified.e2ee_public_key)
            .ok()?
            .try_into()
            .ok()?;
        (provider == ZERO_G_PROVIDER
            && algorithm == ZERO_G_E2EE_ALGORITHM
            && hex::encode(Sha256::digest(key)).eq_ignore_ascii_case(public_key_sha256)
            && verified.provider_address.eq_ignore_ascii_case(address)
            && verified.model == model)
            .then_some((verified, key))
    })
}

/// Fetches the §8 signature directly from the verified broker (the router does
/// not serve it) and checks it against the verified signer.
struct SignatureCheck {
    client: reqwest::Client,
    url: String,
    signer: String,
    provider: String,
}

impl SignatureCheck {
    async fn verify(&self, expected_text: &str) -> Result<(), UpstreamError> {
        let signature = self.fetch().await.map_err(|reason| {
            tracing::warn!(target: "zero_g", provider = %self.provider, kind = "signature_unavailable", %reason, "0G response signature could not be fetched");
            UpstreamError::Transport("0G response signature unavailable".to_string())
        })?;
        verify_signature(&signature, expected_text, &self.signer).map_err(|reason| {
            tracing::warn!(target: "zero_g", provider = %self.provider, kind = "signature_invalid", %reason, "0G response signature failed verification");
            UpstreamError::Transport("0G response signature invalid".to_string())
        })
    }

    /// Retries 404 (the broker caches the signature at end of response), 5xx
    /// and transport errors, as 0G's client does.
    async fn fetch(&self) -> Result<ChatSignature, String> {
        let mut last_error = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(200 << (attempt - 1))).await;
            }
            match self.client.get(&self.url).send().await {
                Ok(response) if response.status().is_success() => {
                    return response
                        .json()
                        .await
                        .map_err(|e| e.without_url().to_string());
                }
                Ok(response) => {
                    last_error = format!("signature endpoint returned {}", response.status());
                    if response.status() != 404 && !response.status().is_server_error() {
                        break;
                    }
                }
                Err(err) => last_error = err.without_url().to_string(),
            }
        }
        Err(last_error)
    }
}

#[async_trait]
impl UpstreamBackend for ZeroGProviderBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn url_origin(&self) -> Option<&str> {
        Some(&self.router_base)
    }

    fn requires_verified_binding(&self, _req: &PreparedUpstreamRequest) -> bool {
        true
    }

    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        Err(unverified())
    }

    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let (response, mut opener, check) = self.send(req, event).await?;
        let frame = response.json().await.map_err(rejected)?;
        let body = opener.open_frame(frame)?;
        check.verify(&opener.signed_text(false)?).await?;
        Ok(UpstreamResponse {
            status_code: 200,
            body: serde_json::to_vec(&body).map_err(rejected)?,
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
            served_instance_id: Some(check.provider),
        })
    }

    async fn forward_stream(
        &self,
        _req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        Err(unverified())
    }

    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        let (response, opener, check) = self.send(req, event).await?;
        let content_type = "text/event-stream";
        if !response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with(content_type))
        {
            return Err(rejected("a streamed response must be text/event-stream"));
        }
        let served_instance_id = Some(check.provider.clone());
        let raw = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(transport_error));
        Ok(UpstreamStreamResponse {
            status_code: 200,
            headers: HashMap::from([("content-type".to_string(), content_type.to_string())]),
            body: verified_event_stream(raw, opener, check),
            served_instance_id,
        })
    }
}

/// Opens each SSE frame as it arrives. `[DONE]` is emitted only after the
/// final frame and a verified signature; any failure ends the stream with an
/// error instead, so it is never presented as complete.
fn verified_event_stream(
    raw: impl Stream<Item = Result<Bytes, UpstreamError>> + Send + 'static,
    mut opener: ResponseOpener,
    check: SignatureCheck,
) -> UpstreamBodyStream {
    Box::pin(async_stream::try_stream! {
        let mut raw = Box::pin(raw);
        let mut buffer = Vec::new();
        let mut data: Option<Vec<u8>> = None;
        'read: loop {
            let chunk = raw.next().await.transpose()?;
            let at_end = chunk.is_none();
            // At EOF a final event without its blank line still counts.
            buffer.extend_from_slice(chunk.as_deref().unwrap_or(b"\n\n"));
            if buffer.len() > MAX_SSE_BUFFER_BYTES {
                Err(rejected("SSE line too long"))?;
            }
            while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buffer.drain(..=end).collect();
                let line = line.trim_ascii_end();
                if let Some(value) = line.strip_prefix(b"data:") {
                    let value = value.strip_prefix(b" ").unwrap_or(value);
                    let data = data.get_or_insert_with(Vec::new);
                    if !data.is_empty() {
                        data.push(b'\n');
                    }
                    data.extend_from_slice(value);
                } else if line.is_empty() {
                    let Some(event) = data.take() else { continue };
                    if event.trim_ascii() == b"[DONE]" {
                        break 'read;
                    }
                    let frame = serde_json::from_slice(&event).map_err(rejected)?;
                    let opened = opener.open_frame(frame)?;
                    // Opening drops the unbound fields; a frame left without
                    // a value carries nothing for the caller.
                    if opened.values().all(is_empty) {
                        continue;
                    }
                    let mut out = b"data: ".to_vec();
                    out.extend(serde_json::to_vec(&opened).map_err(rejected)?);
                    out.extend_from_slice(b"\n\n");
                    yield Bytes::from(out);
                }
            }
            if at_end {
                break;
            }
        }
        check.verify(&opener.signed_text(true)?).await?;
        yield Bytes::from_static(b"data: [DONE]\n\n");
    })
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(fields) => fields.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
