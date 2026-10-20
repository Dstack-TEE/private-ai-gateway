//! 0G Private Computer provider backend: the 0G E2EE protocol implemented
//! natively, routed through the 0G router to one verified provider broker.
//!
//! The verifier (`crate::aci::verifier::ZeroGProviderVerifier`) checks each
//! private-tier candidate before anything is sealed to it and publishes one
//! `e2ee_public_key_sha256` binding per verified provider. This backend only
//! seals to a key that matches such a binding, pins the router to that
//! provider, and releases a response only after the provider's §8 signature
//! recovers to the verified on-chain signer.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

mod protocol;

pub(crate) use protocol::parse_report_data;
use protocol::{seal_chat_request, verify_response_signature, ChatSignature, ResponseOpener};

use super::tls::response_headers;
use super::{
    transport_error, PreparedUpstreamRequest, UpstreamBackend, UpstreamBodyStream, UpstreamError,
    UpstreamRequest, UpstreamResponse, UpstreamStreamResponse,
};
use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};

/// Provider type recorded in events, sessions and channel bindings.
pub const ZERO_G_PROVIDER: &str = "0g-private-computer";
/// The `e2ee_public_key_sha256` algorithm: the 0G v1 HPKE suite (§3).
pub const ZERO_G_E2EE_ALGORITHM: &str = "0g-pc-v1-hpke-x25519-hkdf-sha256-chacha20poly1305";

/// Selects 0G's private (TeeML, self-hosted in a TEE) tier. Set by code on
/// every router call; never taken from config or the caller.
pub(crate) const TRUST_MODE_HEADER: &str = "x-0g-provider-trust-mode";
pub(crate) const TRUST_MODE_PRIVATE: &str = "private";
const PROVIDER_ADDRESS_HEADER: &str = "x-0g-provider-address";
const PROVIDER_IDENTITY_HEADER: &str = "x-0g-provider-identity";
const ALLOW_FALLBACKS_HEADER: &str = "x-0g-provider-allow-fallbacks";
const RESPONSE_KEY_HEADER: &str = "zg-res-key";
const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";

const SIGNATURE_FETCH_ATTEMPTS: u32 = 3;
const SIGNATURE_FETCH_BACKOFF: Duration = Duration::from_millis(200);
const SIGNATURE_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_SIGNATURE_BYTES: usize = 64 << 10;
const MAX_SSE_LINE_BYTES: usize = 4 << 20;

/// One verified provider, as the verifier records it under
/// `provider_claims.instances[<address>]`. Only providers whose key matches a
/// verified channel binding are ever used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct VerifiedProvider {
    pub provider_address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_identity: Option<String>,
    /// The serving URL from the provider's on-chain record.
    pub endpoint: String,
    pub model: String,
    pub signing_address: String,
    /// Hex X25519 key from the verified quote's report_data.
    pub e2ee_public_key: String,
}

/// An HTTP client that never follows redirects or uses an ambient proxy, and
/// speaks HTTPS only (plain HTTP is allowed in unit tests for local mocks).
pub(crate) fn zero_g_client(connect_timeout_seconds: u64) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(!cfg!(test))
        .connect_timeout(Duration::from_secs(connect_timeout_seconds))
}

/// `<endpoint>/v1`, following 0G's derivation of a broker's API base
/// (client/route deriveV1Base).
pub(crate) fn broker_v1_base(endpoint: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(endpoint).map_err(|e| format!("invalid endpoint: {e}"))?;
    let host = url
        .host_str()
        .ok_or_else(|| "endpoint has no host".to_string())?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("endpoint must be a plain URL".to_string());
    }
    let mut base = url.path().trim_end_matches('/').to_string();
    if let Some(stripped) = base.strip_suffix("/chat/completions") {
        base = stripped.to_string();
    } else if !base.ends_with("/v1") {
        base.push_str("/v1");
    }
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Ok(format!("{}://{host}{port}{base}", url.scheme()))
}

pub struct ZeroGProviderBackend {
    name: String,
    router_base: String,
    api_key: Option<String>,
    client: reqwest::Client,
    signature_client: reqwest::Client,
}

impl ZeroGProviderBackend {
    pub fn new_with_timeouts(
        router_base: impl Into<String>,
        connect_timeout_seconds: u64,
        read_timeout_seconds: u64,
    ) -> Result<Self, UpstreamError> {
        let client = zero_g_client(connect_timeout_seconds)
            .read_timeout(Duration::from_secs(read_timeout_seconds))
            .build()
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        let signature_client = zero_g_client(connect_timeout_seconds)
            .timeout(SIGNATURE_FETCH_TIMEOUT)
            .build()
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        Ok(Self {
            name: ZERO_G_PROVIDER.to_string(),
            router_base: router_base.into().trim().trim_end_matches('/').to_string(),
            api_key: None,
            client,
            signature_client,
        })
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.api_key = Some(token.into());
        self
    }

    fn requires_verified_binding(&self) -> UpstreamError {
        UpstreamError::Transport(
            "0G E2EE transport requires a verified 0g-private-computer key binding".to_string(),
        )
    }

    /// Seal the request to the first verified provider and send it through the
    /// router, pinned to that provider.
    async fn invoke_verified(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<(reqwest::Response, ResponseOpener, SignatureCheck), UpstreamError> {
        if event.result != VerificationResult::Verified {
            return Err(self.requires_verified_binding());
        }
        let api_key = self
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| {
                UpstreamError::Transport(
                    "0G E2EE transport requires bearer_token in upstream config".to_string(),
                )
            })?;
        if req.request.path.as_deref().unwrap_or(CHAT_COMPLETIONS_PATH) != CHAT_COMPLETIONS_PATH {
            return Err(UpstreamError::Routing(
                "0G Private Computer serves only /v1/chat/completions".to_string(),
            ));
        }
        let request: Map<String, Value> = serde_json::from_slice(&req.request.body)
            .map_err(|e| UpstreamError::Routing(format!("invalid JSON request body: {e}")))?;
        let model = request
            .get("model")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                UpstreamError::Routing("request body must contain a string model field".into())
            })?;
        let provider = verified_providers(event, model)
            .into_iter()
            .next()
            .ok_or_else(|| {
                UpstreamError::ChannelBindingMismatch(format!(
                    "no verified 0G provider is bound for model {model:?}"
                ))
            })?;
        let enc_pub: [u8; 32] = hex::decode(&provider.e2ee_public_key)
            .ok()
            .and_then(|key| key.try_into().ok())
            .ok_or_else(|| {
                UpstreamError::ChannelBindingMismatch("invalid verified 0G key".into())
            })?;
        let signature_url = broker_v1_base(&provider.endpoint)
            .map_err(|e| UpstreamError::Transport(format!("0G provider endpoint: {e}")))?;
        let sealed = seal_chat_request(request, &enc_pub, &provider.signing_address)
            .map_err(|e| UpstreamError::Routing(e.to_string()))?;
        let body = serde_json::to_vec(&sealed.envelope)
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;

        // Only these headers reach the router: nothing from the caller or the
        // route config, so no X-0G-* control can be injected.
        let mut builder = self
            .client
            .post(format!("{}{CHAT_COMPLETIONS_PATH}", self.router_base))
            .bearer_auth(api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(TRUST_MODE_HEADER, TRUST_MODE_PRIVATE)
            .header(PROVIDER_ADDRESS_HEADER, &provider.provider_address)
            .header(ALLOW_FALLBACKS_HEADER, "false")
            .body(body);
        if let Some(identity) = provider
            .provider_identity
            .as_deref()
            .filter(|id| !id.is_empty())
        {
            builder = builder.header(PROVIDER_IDENTITY_HEADER, identity);
        }
        let response = builder.send().await.map_err(transport_error)?;
        let check = SignatureCheck {
            client: self.signature_client.clone(),
            v1_base: signature_url,
            signer: provider.signing_address,
            provider: provider.provider_address,
            response_key: response
                .headers()
                .get(RESPONSE_KEY_HEADER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
        };
        Ok((response, sealed.opener, check))
    }
}

/// The verified providers this event binds for `model`, in the router's
/// preview order. A provider is usable only when its recorded key hashes to a
/// binding the event still carries (session pinning may have narrowed them).
fn verified_providers(event: &UpstreamVerifiedEvent, model: &str) -> Vec<VerifiedProvider> {
    let instances = event
        .provider_claims
        .as_ref()
        .and_then(|claims| claims.get("instances"));
    let mut providers = Vec::new();
    for binding in &event.channel_bindings {
        let ChannelBinding::E2eePublicKeySha256 {
            provider,
            key_id: Some(address),
            algorithm,
            public_key_sha256,
        } = binding
        else {
            continue;
        };
        if provider != ZERO_G_PROVIDER || algorithm != ZERO_G_E2EE_ALGORITHM {
            continue;
        }
        let Some(instance) = instances.and_then(|instances| instances.get(address)) else {
            continue;
        };
        let Ok(verified) = serde_json::from_value::<VerifiedProvider>(instance.clone()) else {
            continue;
        };
        let key_matches = hex::decode(&verified.e2ee_public_key)
            .map(|key| hex::encode(Sha256::digest(key)).eq_ignore_ascii_case(public_key_sha256))
            .unwrap_or(false);
        if key_matches
            && verified.provider_address.eq_ignore_ascii_case(address)
            && verified.model == model
        {
            providers.push(verified);
        }
    }
    providers
}

/// Fetches the §8 signature directly from the verified provider's broker (the
/// router does not proxy it) and checks it against the verified signer.
struct SignatureCheck {
    client: reqwest::Client,
    v1_base: String,
    signer: String,
    provider: String,
    response_key: Option<String>,
}

impl SignatureCheck {
    /// A successful response must name its signature before any of it is used.
    fn response_key(&self) -> Result<&str, UpstreamError> {
        self.response_key
            .as_deref()
            .filter(|key| valid_response_key(key))
            .ok_or_else(|| rejected("response carried no valid ZG-Res-Key"))
    }

    async fn verify(&self, expected_text: &str) -> Result<(), UpstreamError> {
        let signature = self.fetch().await.map_err(|reason| {
            tracing::warn!(
                target: "zero_g",
                provider = %self.provider,
                kind = "signature_unavailable",
                %reason,
                "0G response signature could not be fetched"
            );
            UpstreamError::Transport(format!("0G response signature unavailable: {reason}"))
        })?;
        verify_response_signature(&signature, expected_text, &self.signer).map_err(|reason| {
            tracing::warn!(
                target: "zero_g",
                provider = %self.provider,
                kind = "signature_invalid",
                %reason,
                "0G response signature failed verification"
            );
            UpstreamError::Transport(format!("0G response signature invalid: {reason}"))
        })
    }

    /// Bounded retry on the benign races (the broker caches the signature at
    /// end of response, so a 404 may precede it) and transient failures.
    async fn fetch(&self) -> Result<ChatSignature, String> {
        let key = self.response_key().map_err(|e| e.to_string())?;
        let url = format!("{}/proxy/signature/{key}", self.v1_base);
        let mut last_error = String::new();
        for attempt in 0..SIGNATURE_FETCH_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(SIGNATURE_FETCH_BACKOFF * 2u32.pow(attempt - 1)).await;
            }
            let response = match self.client.get(&url).send().await {
                Ok(response) => response,
                Err(err) => {
                    last_error = err.to_string();
                    continue;
                }
            };
            let status = response.status().as_u16();
            if status != 200 {
                last_error = format!("signature endpoint returned {status}");
                if status == 404 || (500..600).contains(&status) {
                    continue;
                }
                return Err(last_error);
            }
            let body = read_limited(response, MAX_SIGNATURE_BYTES).await?;
            return serde_json::from_slice(&body).map_err(|e| format!("malformed signature: {e}"));
        }
        Err(format!(
            "after {SIGNATURE_FETCH_ATTEMPTS} attempts: {last_error}"
        ))
    }
}

/// Read a control-plane response body, refusing one larger than `limit`.
pub(crate) async fn read_limited(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if body.len() + chunk.len() > limit {
            return Err(format!("response body exceeds {limit} bytes"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// 0G's chat-key allowlist: the value arrives from an untrusted hop and goes
/// into a URL path.
fn valid_response_key(key: &str) -> bool {
    (1..=64).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn rejected(err: impl std::fmt::Display) -> UpstreamError {
    UpstreamError::Transport(format!("0G response rejected: {err}"))
}

#[async_trait]
impl UpstreamBackend for ZeroGProviderBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn url_origin(&self) -> Option<&str> {
        Some(&self.router_base)
    }

    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        Err(self.requires_verified_binding())
    }

    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let (response, mut opener, check) = self.invoke_verified(req, event).await?;
        let status_code = response.status().as_u16();
        let headers = response_headers(&response);
        let served_instance_id = Some(check.provider.clone());
        let body = response.bytes().await.map_err(transport_error)?.to_vec();
        if status_code != 200 {
            return Ok(UpstreamResponse {
                status_code,
                body,
                headers,
                served_instance_id,
            });
        }
        check.response_key()?;
        let frame: Map<String, Value> = serde_json::from_slice(&body).map_err(rejected)?;
        let opened = opener.open_frame(frame).map_err(rejected)?;
        let text = opener.buffered_signed_text().map_err(rejected)?;
        check.verify(&text).await?;
        Ok(UpstreamResponse {
            status_code,
            body: serde_json::to_vec(&opened).map_err(rejected)?,
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
            served_instance_id,
        })
    }

    async fn forward_stream(
        &self,
        _req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        Err(self.requires_verified_binding())
    }

    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        let (response, opener, check) = self.invoke_verified(req, event).await?;
        let status_code = response.status().as_u16();
        let headers = response_headers(&response);
        let served_instance_id = Some(check.provider.clone());
        let raw = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(transport_error));
        if status_code != 200 {
            return Ok(UpstreamStreamResponse {
                status_code,
                headers,
                body: Box::pin(raw),
                served_instance_id,
            });
        }
        let is_event_stream = headers
            .get("content-type")
            .is_some_and(|value| value.starts_with("text/event-stream"));
        if !is_event_stream {
            return Err(rejected("a streamed 0G response must be text/event-stream"));
        }
        check.response_key()?;
        Ok(UpstreamStreamResponse {
            status_code,
            headers,
            body: verified_event_stream(raw, opener, check),
            served_instance_id,
        })
    }
}

/// Opens each sealed SSE frame in order and passes it on as it arrives. The
/// stream's own `[DONE]` is emitted only after the final frame and a verified
/// §8 signature; a truncated stream or a failed verification ends in an error
/// instead, so the gateway never presents it as complete.
fn verified_event_stream(
    raw: impl Stream<Item = Result<Bytes, UpstreamError>> + Send + 'static,
    mut opener: ResponseOpener,
    check: SignatureCheck,
) -> UpstreamBodyStream {
    Box::pin(async_stream::try_stream! {
        let mut events = SseEvents::default();
        let mut raw = Box::pin(raw);
        let mut pending = Vec::new();
        'read: loop {
            let next = raw.next().await;
            let at_end = next.is_none();
            match next {
                Some(chunk) => pending.extend(events.push(&chunk?)?),
                None => pending.extend(events.finish()),
            }
            for data in pending.drain(..) {
                if data.trim_ascii() == b"[DONE]" {
                    break 'read;
                }
                let frame: Map<String, Value> = serde_json::from_slice(&data).map_err(rejected)?;
                let opened = opener.open_frame(frame).map_err(rejected)?;
                let mut event = b"data: ".to_vec();
                event.extend(serde_json::to_vec(&opened).map_err(rejected)?);
                event.extend_from_slice(b"\n\n");
                yield Bytes::from(event);
            }
            if at_end {
                break;
            }
        }
        let text = opener.stream_signed_text().map_err(rejected)?;
        check.verify(&text).await?;
        yield Bytes::from_static(b"data: [DONE]\n\n");
    })
}

/// Minimal SSE framing: `data:` lines joined per event, other fields ignored.
#[derive(Default)]
struct SseEvents {
    line: Vec<u8>,
    data: Option<Vec<u8>>,
}

impl SseEvents {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, UpstreamError> {
        let mut events = Vec::new();
        for &byte in chunk {
            if byte != b'\n' {
                self.line.push(byte);
                if self.line.len() > MAX_SSE_LINE_BYTES {
                    return Err(rejected("SSE line too long"));
                }
                continue;
            }
            let line = std::mem::take(&mut self.line);
            events.extend(self.end_line(line));
        }
        Ok(events)
    }

    /// The last event of a stream that ended without a blank line.
    fn finish(&mut self) -> Option<Vec<u8>> {
        let line = std::mem::take(&mut self.line);
        if !line.is_empty() {
            self.end_line(line);
        }
        self.data.take()
    }

    fn end_line(&mut self, mut line: Vec<u8>) -> Option<Vec<u8>> {
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            return self.data.take();
        }
        if let Some(rest) = line.strip_prefix(b"data:") {
            let rest = rest.strip_prefix(b" ").unwrap_or(rest);
            match &mut self.data {
                Some(data) => {
                    data.push(b'\n');
                    data.extend_from_slice(rest);
                }
                None => self.data = Some(rest.to_vec()),
            }
        }
        None
    }
}

#[cfg(test)]
mod tests;
