//! Optional NEAR AI response binding for verified, buffered chat completions.
//! The router's TLS binding remains the admission boundary.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey as K256VerifyingKey};
use serde::Deserialize;
use serde_json::json;
use sha2::Sha256;
use sha3::{Digest, Keccak256};

use super::body::read_buffered_upstream_response;
use super::{
    OpenAICompatibleBackend, PreparedUpstreamRequest, SseProtocol, UpstreamBackend, UpstreamError,
    UpstreamRequest, UpstreamResponse, UpstreamStreamResponse,
};
use crate::aci::receipt::{UpstreamVerifiedEvent, VerificationResult};
use crate::aci::verifier::NearAiProviderVerifier;
use crate::endpoint::Endpoint;
use tokio::sync::Semaphore;

/// Receipt extension event recording verification of the model key signature.
pub const EVENT_UPSTREAM_RESPONSE_ATTESTED: &str = "upstream.response_attested";

const SIGNATURE_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_SIGNATURE_BYTES: usize = 8 * 1024;
const MAX_SIGNATURE_LOOKUPS: usize = 16;
const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(10);
const MAX_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60);

/// One `GET /v1/signature/{chat_id}` result.
#[derive(Debug, Clone, Deserialize)]
pub struct NearResponseSignature {
    pub text: String,
    pub signature: String,
    pub signing_address: String,
    pub signing_algo: String,
    #[serde(default)]
    pub signature_kind: Option<String>,
}

#[derive(Deserialize)]
struct NearModelEvidence {
    digest: String,
    source_url: String,
    verified_candidates: BTreeMap<String, usize>,
}

/// Check a `provider_tee` signature against the exact bytes exchanged and
/// return the recovered signer (lowercase `0x` address).
pub fn bind_response_signature(
    sig: &NearResponseSignature,
    model: &str,
    request_body: &[u8],
    response_body: &[u8],
) -> Result<String, String> {
    match sig.signature_kind.as_deref() {
        Some("provider_tee") => {}
        Some(other) => {
            return Err(format!(
                "signature kind {other:?} does not identify a model signing key"
            ))
        }
        None => return Err("signature kind missing".to_string()),
    }
    if sig.signing_algo != "ecdsa" {
        return Err(format!(
            "unsupported signing algorithm {:?}",
            sig.signing_algo
        ));
    }
    let expected = format!(
        "{model}:{}:{}",
        near_digest(request_body),
        near_digest(response_body)
    );
    if sig.text != expected {
        return Err("signed text does not match the exchanged request and response".to_string());
    }
    let signer = recover_eip191_signer(&sig.text, &sig.signature)?;
    if !signer.eq_ignore_ascii_case(&sig.signing_address) {
        return Err(format!(
            "recovered signer {signer} does not match reported signing address {}",
            sig.signing_address
        ));
    }
    Ok(signer)
}

/// SHA-256 as NEAR signs it: bare lowercase hex, unlike the gateway's
/// `sha256:`-prefixed digests.
fn near_digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Recover the Ethereum address that produced an EIP-191 `personal_sign`
/// signature (65 bytes, hex, `v` in 0/1 or 27/28) over `message`.
pub fn recover_eip191_signer(message: &str, signature_hex: &str) -> Result<String, String> {
    let raw = hex::decode(signature_hex.trim_start_matches("0x"))
        .map_err(|e| format!("signature is not hex: {e}"))?;
    if raw.len() != 65 {
        return Err(format!("signature must be 65 bytes, got {}", raw.len()));
    }
    let v = match raw[64] {
        0 | 1 => raw[64],
        27 | 28 => raw[64] - 27,
        _ => return Err("invalid recovery id".to_string()),
    };
    let recid =
        RecoveryId::from_byte(v).ok_or_else(|| format!("invalid recovery id {}", raw[64]))?;
    let signature =
        K256Signature::from_slice(&raw[..64]).map_err(|e| format!("invalid signature: {e}"))?;
    let prefix = format!("\x19Ethereum Signed Message:\n{}", message.len());
    let digest = Keccak256::new_with_prefix(prefix.as_bytes()).chain_update(message.as_bytes());
    let key = K256VerifyingKey::recover_from_digest(digest, &signature, recid)
        .map_err(|e| format!("signer recovery failed: {e}"))?;
    let uncompressed = key.to_encoded_point(false);
    let hash = Keccak256::digest(&uncompressed.as_bytes()[1..]);
    Ok(format!("0x{}", hex::encode(&hash[12..])))
}

fn is_near_chat_request(req: &PreparedUpstreamRequest) -> bool {
    req.request
        .path
        .as_deref()
        .unwrap_or(Endpoint::ChatComplete.path())
        == Endpoint::ChatComplete.path()
}

pub struct NearAiBackend {
    inner: OpenAICompatibleBackend,
    base_url: String,
    bearer_token: String,
    client: reqwest::Client,
    lookups: Semaphore,
    rate_limited_until: Mutex<Option<Instant>>,
}

impl NearAiBackend {
    pub fn new(
        inner: OpenAICompatibleBackend,
        base_url: impl Into<String>,
        bearer_token: String,
    ) -> Result<Self, UpstreamError> {
        let client = reqwest::Client::builder()
            .timeout(SIGNATURE_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|e| UpstreamError::Transport(format!("NEAR signature client: {e}")))?;
        Ok(Self {
            inner,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            bearer_token,
            client,
            lookups: Semaphore::new(MAX_SIGNATURE_LOOKUPS),
            rate_limited_until: Mutex::new(None),
        })
    }

    fn with_signing_headers(req: &mut UpstreamRequest) {
        req.headers
            .insert("x-no-aliasing".to_string(), "true".to_string());
        req.headers
            .insert("accept-encoding".to_string(), "identity".to_string());
    }

    async fn fetch_signature(&self, chat_id: &str) -> Result<NearResponseSignature, &'static str> {
        if !matches!(chat_id.len(), 1..=128)
            || !chat_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err("invalid chat id");
        }
        if self
            .rate_limited_until
            .lock()
            .ok()
            .and_then(|until| *until)
            .is_some_and(|until| Instant::now() < until)
        {
            return Err("signature lookup in rate-limit cooldown");
        }
        let _permit = self
            .lookups
            .try_acquire()
            .map_err(|_| "signature lookup at capacity")?;
        let url = format!("{}/v1/signature/{chat_id}", self.base_url);
        let response = self
            .client
            .get(url)
            .query(&[("signing_algo", "ecdsa")])
            .header("accept", "application/json")
            .bearer_auth(&self.bearer_token)
            .send()
            .await
            .map_err(|_| "signature lookup unavailable")?;
        match response.status() {
            reqwest::StatusCode::OK => {}
            reqwest::StatusCode::NOT_FOUND => return Err("signature not found"),
            reqwest::StatusCode::TOO_MANY_REQUESTS => {
                let delay = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|seconds| seconds.parse().ok())
                    .map(Duration::from_secs)
                    .unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN)
                    .min(MAX_RATE_LIMIT_COOLDOWN);
                if let Ok(mut deadline) = self.rate_limited_until.lock() {
                    *deadline = Some(Instant::now() + delay).max(*deadline);
                }
                return Err("signature lookup rate limited");
            }
            _ => return Err("signature lookup unavailable"),
        }
        let body = read_buffered_upstream_response(response, MAX_SIGNATURE_BYTES)
            .await
            .map_err(|_| "signature body unavailable")?;
        serde_json::from_slice(&body).map_err(|_| "signature fields unavailable")
    }

    async fn attest_response(
        &self,
        req: &PreparedUpstreamRequest,
        evidence: &NearModelEvidence,
        response: &mut UpstreamResponse,
    ) {
        #[derive(Deserialize)]
        struct CompletionId<'a> {
            id: &'a str,
        }

        let outcome = match serde_json::from_slice::<CompletionId<'_>>(&response.body) {
            Ok(completion) => self.fetch_signature(completion.id).await,
            Err(_) => Err("response has no chat id"),
        };
        let fields = match outcome {
            Ok(sig) => match bind_response_signature(
                &sig,
                &req.model_id,
                &req.request.body,
                &response.body,
            ) {
                Ok(signer) => {
                    if let Some(index) = evidence.verified_candidates.get(&signer) {
                        json!({
                            "provider": "near-ai", "bound": true, "signature_kind": sig.signature_kind,
                            "signing_algo": sig.signing_algo, "signing_address": signer,
                            "text": sig.text, "signature": sig.signature,
                            "candidate_evidence": {
                                "digest": evidence.digest,
                                "source_url": evidence.source_url,
                                "model_attestation_index": index,
                            },
                        })
                    } else {
                        json!({
                            "provider": "near-ai", "bound": false, "signing_address": signer,
                            "reason": "signer has no unique verified report entry",
                        })
                    }
                }
                Err(_) => {
                    json!({"provider": "near-ai", "bound": false, "reason": "signature does not bind the exchanged bytes to a model signing key"})
                }
            },
            Err(reason) => json!({"provider": "near-ai", "bound": false, "reason": reason}),
        };
        response.response_attestation = fields.as_object().cloned();
    }
}

#[async_trait]
impl UpstreamBackend for NearAiBackend {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn url_origin(&self) -> Option<&str> {
        self.inner.url_origin()
    }

    fn response_protocol(
        &self,
        req: &PreparedUpstreamRequest,
        operation: Endpoint,
    ) -> Option<SseProtocol> {
        self.inner.response_protocol(req, operation)
    }

    async fn forward(&self, req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        self.inner.forward(req).await
    }

    async fn forward_verified_prepared(
        &self,
        mut req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let evidence = event
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.get("near_model"));
        let Some(evidence) = evidence
            .filter(|_| {
                is_near_chat_request(&req)
                    && event.result == VerificationResult::Verified
                    && event.verifier_id == NearAiProviderVerifier::VERIFIER_ID
                    && event.provider_type.as_deref() == Some("near-ai")
                    && event.model_id == req.model_id
                    && !event.channel_bindings.is_empty()
            })
            .and_then(|evidence| NearModelEvidence::deserialize(evidence).ok())
            .filter(|evidence| !evidence.verified_candidates.is_empty())
        else {
            return self.inner.forward_verified_prepared(req, event).await;
        };

        Self::with_signing_headers(&mut req.request);
        let mut response = self
            .inner
            .forward_verified_prepared(req.clone(), event)
            .await?;
        if (200..300).contains(&response.status_code) {
            self.attest_response(&req, &evidence, &mut response).await;
        }
        Ok(response)
    }

    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        self.inner.models().await
    }

    async fn forward_stream(
        &self,
        req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.inner.forward_stream(req).await
    }

    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.inner
            .forward_stream_verified_prepared(req, event)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::SigningKey;

    fn eip191_sign(key: &SigningKey, message: &str) -> String {
        let prefix = format!("\x19Ethereum Signed Message:\n{}", message.len());
        let digest = Keccak256::new_with_prefix(prefix.as_bytes()).chain_update(message.as_bytes());
        let (sig, recid) = key.sign_digest_recoverable(digest).unwrap();
        let mut raw = sig.to_bytes().to_vec();
        raw.push(27 + recid.to_byte());
        format!("0x{}", hex::encode(raw))
    }

    fn address(key: &SigningKey) -> String {
        let point = key.verifying_key().to_encoded_point(false);
        format!(
            "0x{}",
            hex::encode(&Keccak256::digest(&point.as_bytes()[1..])[12..])
        )
    }

    fn signed(key: &SigningKey, kind: &str, text: String) -> NearResponseSignature {
        NearResponseSignature {
            signature: eip191_sign(key, &text),
            text,
            signing_address: address(key),
            signing_algo: "ecdsa".to_string(),
            signature_kind: Some(kind.to_string()),
        }
    }

    const REQ: &[u8] = br#"{"model":"z-ai/glm-5.3-flash","messages":[]}"#;
    const RESP: &[u8] = br#"{"id":"chatcmpl-1","choices":[]}"#;

    fn text_for(model: &str, req: &[u8], resp: &[u8]) -> String {
        format!("{model}:{}:{}", near_digest(req), near_digest(resp))
    }

    #[test]
    fn known_vector_recovers_the_signing_address() {
        // Private key 1, whose address is well known.
        let key =
            SigningKey::from_slice(&[0u8; 31].iter().copied().chain([1u8]).collect::<Vec<_>>())
                .unwrap();
        assert_eq!(address(&key), "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf");
        let sig = eip191_sign(&key, "hello");
        assert_eq!(
            recover_eip191_signer("hello", &sig).unwrap(),
            "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf"
        );
    }

    /// Captured from NEAR AI Cloud on 2026-09-24: a non-streaming completion and
    /// the `provider_tee` signature from `/v1/signature/{chat_id}`, signed by a
    /// model enclave whose TDX and GPU attestation verified that day.
    #[test]
    fn live_near_signature_binds_the_exchanged_bytes() {
        const LIVE_REQ: &[u8] = br#"{"model":"z-ai/glm-5.3-flash","messages":[{"role":"user","content":"Reply with the single word: sealed"}],"max_tokens":200,"stream":false}"#;
        const LIVE_RESP: &[u8] = br#"{"choices":[{"finish_reason":"stop","index":0,"logprobs":null,"matched_stop":154827,"message":{"content":"sealed","reasoning_content":"The user has asked me to reply with the single word \"sealed\". This is a simple, direct request. There's nothing harmful about it. I should just comply and reply with the word \"sealed\".","role":"assistant"}}],"created":1790243703,"id":"c9776b549bc546638a958b0c5902a6b4","model":"z-ai/glm-5.3-flash","object":"chat.completion","usage":{"completion_tokens":44,"prompt_tokens":19,"prompt_tokens_details":null,"reasoning_tokens":42,"total_tokens":63}}"#;
        let sig = NearResponseSignature {
            text: "z-ai/glm-5.3-flash:0f2eaa629c0fdb8f79ff82016a241d7d2250a2fcfc31bcda589cd434d8af30c2:f9d68fc2a6269351b7aef78534b6e9e520aa7c1ddb480d6504138a2685499f2d".to_string(),
            signature: "0xcf99a0fc3b04fafdb340c69f766607bb4d4af8cd3f966b1ea630b0d28b507b17601812ba77bf09fa3f86360a6f3ced6caa9ae91d4b2c075a286c83f586b67ab41b".to_string(),
            signing_address: "0x3b753b4406613c5e70ae640508632b600424fa0c".to_string(),
            signing_algo: "ecdsa".to_string(),
            signature_kind: Some("provider_tee".to_string()),
        };
        assert_eq!(
            bind_response_signature(&sig, "z-ai/glm-5.3-flash", LIVE_REQ, LIVE_RESP).unwrap(),
            "0x3b753b4406613c5e70ae640508632b600424fa0c"
        );
    }

    #[test]
    fn provider_tee_signature_over_the_exchanged_bytes_binds() {
        let key = SigningKey::random(&mut rand::rngs::OsRng);
        let sig = signed(
            &key,
            "provider_tee",
            text_for("z-ai/glm-5.3-flash", REQ, RESP),
        );
        assert_eq!(
            bind_response_signature(&sig, "z-ai/glm-5.3-flash", REQ, RESP).unwrap(),
            address(&key)
        );
    }

    #[test]
    fn gateway_signatures_do_not_bind_an_enclave() {
        let key = SigningKey::random(&mut rand::rngs::OsRng);
        let sig = signed(
            &key,
            "gateway",
            format!("{}:{}", near_digest(REQ), near_digest(RESP)),
        );
        assert!(
            bind_response_signature(&sig, "z-ai/glm-5.3-flash", REQ, RESP)
                .unwrap_err()
                .contains("gateway")
        );
    }

    #[test]
    fn altered_bytes_or_model_do_not_bind() {
        let key = SigningKey::random(&mut rand::rngs::OsRng);
        let sig = signed(
            &key,
            "provider_tee",
            text_for("z-ai/glm-5.3-flash", REQ, RESP),
        );
        assert!(bind_response_signature(&sig, "z-ai/glm-5.3-flash", REQ, b"{}").is_err());
        assert!(bind_response_signature(&sig, "z-ai/glm-5.3-flash", b"{}", RESP).is_err());
        assert!(bind_response_signature(&sig, "other/model", REQ, RESP).is_err());
    }

    #[test]
    fn a_signature_from_another_key_does_not_bind() {
        let key = SigningKey::random(&mut rand::rngs::OsRng);
        let other = SigningKey::random(&mut rand::rngs::OsRng);
        let mut sig = signed(
            &key,
            "provider_tee",
            text_for("z-ai/glm-5.3-flash", REQ, RESP),
        );
        sig.signing_address = address(&other);
        assert!(
            bind_response_signature(&sig, "z-ai/glm-5.3-flash", REQ, RESP)
                .unwrap_err()
                .contains("does not match")
        );
    }

    #[test]
    fn signing_headers_are_added() {
        let mut req = UpstreamRequest::default();
        NearAiBackend::with_signing_headers(&mut req);
        assert_eq!(
            req.headers.get("x-no-aliasing").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            req.headers.get("accept-encoding").map(String::as_str),
            Some("identity")
        );
    }

    #[tokio::test]
    async fn signature_lookup_has_one_attempt_and_bounds_unavailable_responses() {
        use axum::http::{header, StatusCode, Uri};
        use axum::response::IntoResponse;
        use axum::{Json, Router};
        use bytes::Bytes;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let lookups = Arc::new(AtomicUsize::new(0));
        let seen = lookups.clone();
        let key = SigningKey::random(&mut rand::rngs::OsRng);
        let signature = signed(
            &key,
            "provider_tee",
            text_for("z-ai/glm-5.3-flash", REQ, RESP),
        );
        let signing_address = signature.signing_address.clone();
        let signature_json = json!({
            "text": signature.text,
            "signature": signature.signature,
            "signing_address": signature.signing_address,
            "signing_algo": signature.signing_algo,
            "signature_kind": signature.signature_kind,
        });
        let server = Router::new().fallback(move |uri: Uri| {
            let seen = seen.clone();
            let signature_json = signature_json.clone();
            async move {
                seen.fetch_add(1, Ordering::Relaxed);
                match uri.path() {
                    "/v1/signature/chatcmpl-1" => Json(signature_json).into_response(),
                    "/v1/signature/not-found" => StatusCode::NOT_FOUND.into_response(),
                    "/v1/signature/big" => "x".repeat(MAX_SIGNATURE_BYTES + 1).into_response(),
                    "/v1/signature/slow" => {
                        tokio::time::sleep(Duration::from_secs(4)).await;
                        StatusCode::OK.into_response()
                    }
                    "/v1/signature/redirect" => (
                        StatusCode::TEMPORARY_REDIRECT,
                        [(header::LOCATION, "/v1/signature/slow")],
                    )
                        .into_response(),
                    "/v1/signature/throttled" => {
                        (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "30")])
                            .into_response()
                    }
                    _ => StatusCode::BAD_REQUEST.into_response(),
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async { axum::serve(listener, server).await.unwrap() });
        let inner = OpenAICompatibleBackend::new(base_url.clone()).unwrap();
        let backend = NearAiBackend::new(inner, base_url, "fixture-token".into()).unwrap();
        let request = backend
            .prepare(UpstreamRequest {
                body: Bytes::from_static(REQ),
                ..Default::default()
            })
            .unwrap();
        for matched in [true, false] {
            let mut response = UpstreamResponse {
                status_code: 200,
                body: RESP.to_vec(),
                ..Default::default()
            };
            let candidate = if matched {
                signing_address.as_str()
            } else {
                "other"
            };
            let evidence = NearModelEvidence {
                digest: crate::aci::digest::sha256_hex(b"candidate report"),
                source_url: "https://cloud-api.near.ai/v1/attestation/report".into(),
                verified_candidates: BTreeMap::from([(candidate.into(), 2)]),
            };
            backend
                .attest_response(&request, &evidence, &mut response)
                .await;
            assert_eq!(response.body, RESP);
            assert!(response.served_instance_id.is_none());
            let fields = response.response_attestation.unwrap();
            assert_eq!(fields["bound"], matched);
            assert_eq!(fields["signing_address"], signing_address);
            if matched {
                assert_eq!(
                    fields["candidate_evidence"],
                    json!({
                        "digest": evidence.digest, "source_url": evidence.source_url,
                        "model_attestation_index": 2,
                    })
                );
            } else {
                assert!(!fields.contains_key("candidate_evidence"));
            }
        }
        for (id, reason) in [
            ("not-found", "signature not found"),
            ("big", "signature body unavailable"),
            ("slow", "signature lookup unavailable"),
            ("redirect", "signature lookup unavailable"),
            ("throttled", "signature lookup rate limited"),
            ("skipped", "signature lookup in rate-limit cooldown"),
            ("../path", "invalid chat id"),
        ] {
            assert_eq!(backend.fetch_signature(id).await.unwrap_err(), reason);
        }
        assert_eq!(lookups.load(Ordering::Relaxed), 7);
        task.abort();
    }
}
