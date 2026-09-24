//! NEAR AI Cloud backend: the OpenAI-compatible backend plus per-response
//! enclave binding.
//!
//! NEAR's gateway routes each request to one of its model enclaves. For a
//! non-streaming completion with a canonical model id, the serving enclave signs
//! `<model>:<sha256(request body)>:<sha256(response body)>` (EIP-191, secp256k1)
//! with the key its TDX quote binds (`provider_tee` signature, served at
//! `GET /v1/signature/{chat_id}`). After a buffered response this backend fetches
//! that signature, checks the signed text against the exact bytes exchanged,
//! recovers the signer and reports it as the serving instance, so the receipt
//! cites that enclave's attested session. The provider verifier seals one session
//! per verified enclave signer; a signer outside that set has no session to cite
//! and the receipt falls back to the router session.
//!
//! Streaming responses, and responses NEAR's gateway re-signs because it changed
//! the relayed bytes (`gateway` signatures: aliasing, the Responses API), stay
//! bound to the router session. The receipt records the outcome either way.

use std::time::Duration;

use async_trait::async_trait;
use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey as K256VerifyingKey};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha3::{Digest, Keccak256};

use super::openai::request_model_id;
use super::{
    OpenAICompatibleBackend, PreparedUpstreamRequest, UpstreamBackend, UpstreamError,
    UpstreamRequest, UpstreamResponse, UpstreamStreamResponse,
};
use crate::aci::digest::sha256_hex;
use crate::aci::receipt::UpstreamVerifiedEvent;

/// Receipt extension event recording the per-response enclave signature.
pub const EVENT_UPSTREAM_RESPONSE_ATTESTED: &str = "upstream.response_attested";

const SIGNATURE_ATTEMPTS: u32 = 4;
const SIGNATURE_RETRY_DELAY: Duration = Duration::from_millis(500);
const SIGNATURE_TIMEOUT: Duration = Duration::from_secs(10);

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
                "signature kind {other:?} is not bound to a model enclave"
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
        sha256_hex(request_body),
        sha256_hex(response_body)
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

/// Recover the Ethereum address that produced an EIP-191 `personal_sign`
/// signature (65 bytes, hex, `v` in 0/1 or 27/28) over `message`.
pub fn recover_eip191_signer(message: &str, signature_hex: &str) -> Result<String, String> {
    let raw = hex::decode(signature_hex.trim_start_matches("0x"))
        .map_err(|e| format!("signature is not hex: {e}"))?;
    if raw.len() != 65 {
        return Err(format!("signature must be 65 bytes, got {}", raw.len()));
    }
    let mut v = raw[64];
    if v >= 27 {
        v -= 27;
    }
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

pub struct NearAiBackend {
    inner: OpenAICompatibleBackend,
    base_url: String,
    bearer_token: Option<String>,
    client: reqwest::Client,
}

impl NearAiBackend {
    pub fn new(
        inner: OpenAICompatibleBackend,
        base_url: impl Into<String>,
        bearer_token: Option<String>,
    ) -> Result<Self, UpstreamError> {
        let client = reqwest::Client::builder()
            .timeout(SIGNATURE_TIMEOUT)
            .build()
            .map_err(|e| UpstreamError::Transport(format!("NEAR signature client: {e}")))?;
        Ok(Self {
            inner,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            bearer_token,
            client,
        })
    }

    /// NEAR signs with the serving enclave only when the relayed bytes are the
    /// enclave's own: no model aliasing, and uncompressed bodies to hash.
    fn with_signing_headers(mut req: UpstreamRequest) -> UpstreamRequest {
        req.headers
            .insert("x-no-aliasing".to_string(), "true".to_string());
        req.headers
            .insert("accept-encoding".to_string(), "identity".to_string());
        req
    }

    async fn fetch_signature(&self, chat_id: &str) -> Result<NearResponseSignature, String> {
        let url = format!("{}/v1/signature/{chat_id}", self.base_url);
        let mut last = String::from("no attempt");
        for attempt in 0..SIGNATURE_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(SIGNATURE_RETRY_DELAY * attempt).await;
            }
            let mut req = self
                .client
                .get(&url)
                .query(&[("signing_algo", "ecdsa")])
                .header("accept", "application/json");
            if let Some(token) = &self.bearer_token {
                req = req.bearer_auth(token);
            }
            match req.send().await {
                // NEAR documents a brief 404 window before a signature exists.
                Ok(resp) if resp.status().as_u16() == 404 => {
                    last = "signature not yet available".to_string()
                }
                Ok(resp) if resp.status().is_success() => {
                    let body: Value = resp
                        .json()
                        .await
                        .map_err(|e| format!("signature body: {e}"))?;
                    if body.get("error_code").is_some() {
                        last = format!(
                            "signature unavailable: {}",
                            body.get("message").cloned().unwrap_or_default()
                        );
                        continue;
                    }
                    return serde_json::from_value(body)
                        .map_err(|e| format!("signature fields: {e}"));
                }
                Ok(resp) => {
                    last = format!("signature fetch returned HTTP {}", resp.status().as_u16())
                }
                Err(e) => last = format!("signature fetch failed: {e}"),
            }
        }
        Err(last)
    }

    /// Bind a buffered 2xx completion to the enclave that signed it. Sets the
    /// serving instance only on a verified `provider_tee` signature, and always
    /// records the outcome for the receipt.
    async fn attest_response(
        &self,
        request_body: &[u8],
        mut response: UpstreamResponse,
    ) -> UpstreamResponse {
        if !(200..300).contains(&response.status_code) {
            return response;
        }
        let parsed: Option<Value> = serde_json::from_slice(&response.body).ok();
        let chat_id = parsed
            .as_ref()
            .and_then(|v| v.get("id"))
            .and_then(Value::as_str);
        let model = request_model_id(request_body);
        let mut fields = Map::new();
        fields.insert("provider".to_string(), Value::String("near-ai".to_string()));
        let (Some(chat_id), Some(model)) = (chat_id, model) else {
            fields.insert("bound".to_string(), Value::Bool(false));
            fields.insert(
                "reason".to_string(),
                Value::String("response has no chat id or request has no model".to_string()),
            );
            response.response_attestation = Some(fields);
            return response;
        };
        match self.fetch_signature(chat_id).await {
            Ok(sig) => {
                fields.insert(
                    "signature_kind".to_string(),
                    sig.signature_kind
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                );
                fields.insert(
                    "signing_algo".to_string(),
                    Value::String(sig.signing_algo.clone()),
                );
                fields.insert(
                    "signing_address".to_string(),
                    Value::String(sig.signing_address.to_lowercase()),
                );
                fields.insert("text".to_string(), Value::String(sig.text.clone()));
                fields.insert(
                    "signature".to_string(),
                    Value::String(sig.signature.clone()),
                );
                match bind_response_signature(&sig, &model, request_body, &response.body) {
                    Ok(signer) => {
                        fields.insert("bound".to_string(), Value::Bool(true));
                        response.served_instance_id = Some(signer);
                    }
                    Err(reason) => {
                        fields.insert("bound".to_string(), Value::Bool(false));
                        fields.insert("reason".to_string(), Value::String(reason));
                    }
                }
            }
            Err(reason) => {
                fields.insert("bound".to_string(), Value::Bool(false));
                fields.insert("reason".to_string(), Value::String(reason));
            }
        }
        response.response_attestation = Some(fields);
        response
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

    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        self.inner.prepare(Self::with_signing_headers(req))
    }

    async fn forward(&self, req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        let req = Self::with_signing_headers(req);
        let body = req.body.clone();
        let response = self.inner.forward(req).await?;
        Ok(self.attest_response(&body, response).await)
    }

    async fn forward_prepared(
        &self,
        req: PreparedUpstreamRequest,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let body = req.request.body.clone();
        let response = self.inner.forward_prepared(req).await?;
        Ok(self.attest_response(&body, response).await)
    }

    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let body = req.request.body.clone();
        let response = self.inner.forward_verified_prepared(req, event).await?;
        Ok(self.attest_response(&body, response).await)
    }

    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        self.inner.models().await
    }

    async fn forward_stream(
        &self,
        req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.inner
            .forward_stream(Self::with_signing_headers(req))
            .await
    }

    async fn forward_stream_prepared(
        &self,
        req: PreparedUpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.inner.forward_stream_prepared(req).await
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
        format!("{model}:{}:{}", sha256_hex(req), sha256_hex(resp))
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
            format!("{}:{}", sha256_hex(REQ), sha256_hex(RESP)),
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
        let req = NearAiBackend::with_signing_headers(UpstreamRequest::default());
        assert_eq!(
            req.headers.get("x-no-aliasing").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            req.headers.get("accept-encoding").map(String::as_str),
            Some("identity")
        );
    }
}
