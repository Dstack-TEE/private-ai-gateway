//! 0G OpenAI-compatible backend with provider-reported response verification.
//!
//! The current 0G router contract reports TEE execution per response using
//! `ZG-Res-Key` and `x_0g_trace.tee_verified`. This adapter deliberately does
//! not claim cryptographic TEE signature validation; it only enforces that the
//! provider reported per-response TEE verification before the gateway releases
//! successful buffered response bytes.

use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    OpenAICompatibleBackend, PreparedUpstreamRequest, UpstreamBackend, UpstreamError,
    UpstreamRequest, UpstreamResponse, UpstreamStreamResponse,
};
use crate::aci::canonical::sha256_hex;
use crate::aci::receipt::UpstreamVerifiedEvent;

const ZG_RES_KEY_HEADER: &str = "zg-res-key";

const STREAMING_UNSUPPORTED: &str =
    "0G response verification for streaming responses is unsupported";

pub struct ZeroGProviderBackend {
    inner: OpenAICompatibleBackend,
}

impl ZeroGProviderBackend {
    pub fn new(base_url: impl Into<String>) -> Result<Self, UpstreamError> {
        Ok(Self {
            inner: OpenAICompatibleBackend::new(base_url)?.without_redirects()?,
        })
    }

    pub fn new_with_timeouts(
        base_url: impl Into<String>,
        connect_timeout_seconds: u64,
        read_timeout_seconds: u64,
    ) -> Result<Self, UpstreamError> {
        Ok(Self {
            inner: OpenAICompatibleBackend::new_with_timeouts(
                base_url,
                connect_timeout_seconds,
                read_timeout_seconds,
            )?
            .without_redirects()?,
        })
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.inner = self.inner.with_name(name);
        self
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.inner = self.inner.with_bearer_token(token);
        self
    }

    pub fn with_basic_auth(mut self, enabled: bool) -> Self {
        self.inner = self.inner.with_basic_auth(enabled);
        self
    }

    fn force_verify_tee(body: &[u8]) -> Result<Vec<u8>, UpstreamError> {
        let mut parsed: Value = serde_json::from_slice(body)
            .map_err(|e| UpstreamError::Routing(format!("invalid JSON request body: {e}")))?;
        let Some(obj) = parsed.as_object_mut() else {
            return Err(UpstreamError::Routing(
                "request body must be a JSON object".to_string(),
            ));
        };
        obj.insert("verify_tee".to_string(), Value::Bool(true));
        serde_json::to_vec(&parsed).map_err(|e| UpstreamError::Routing(e.to_string()))
    }

    fn verify_buffered_response(
        forwarded_body: &[u8],
        response: &mut UpstreamResponse,
    ) -> Result<(), UpstreamError> {
        if !(200..300).contains(&response.status_code) {
            return Err(UpstreamError::ResponseVerificationFailed(format!(
                "0G response verification applies only to successful inference responses; upstream returned {}",
                response.status_code
            )));
        }

        let zg_res_key = header(&response.headers, ZG_RES_KEY_HEADER)
            .filter(|value| !value.trim().is_empty() && !value.contains(','))
            .ok_or_else(|| {
                UpstreamError::ResponseVerificationFailed(
                    "0G response verification failed: expected exactly one non-empty ZG-Res-Key"
                        .to_string(),
                )
            })?;
        let response_json: Value = serde_json::from_slice(&response.body).map_err(|e| {
            UpstreamError::ResponseVerificationFailed(format!(
                "0G response verification failed: response body must be JSON: {e}"
            ))
        })?;
        let trace = response_json.get("x_0g_trace").ok_or_else(|| {
            UpstreamError::ResponseVerificationFailed(
                "0G response verification failed: missing response x_0g_trace object".to_string(),
            )
        })?;
        if trace.get("tee_verified") != Some(&Value::Bool(true)) {
            return Err(UpstreamError::ResponseVerificationFailed(
                "0G response verification failed: x_0g_trace.tee_verified must be JSON boolean true"
                    .to_string(),
            ));
        }

        response.provider_response_claims = Some(json!({
            "0g_response_verification": {
                "verification_type": "provider_reported_per_response",
                "tee_verified": true,
                "zg_res_key_present": true,
                "zg_res_key_sha256": sha256_hex(zg_res_key.as_bytes()),
                "x_0g_trace_sha256": sha256_hex(
                    serde_json::to_vec(trace)
                        .expect("parsed 0G trace must serialize")
                        .as_slice()
                ),
                "forwarded_body_hash": sha256_hex(forwarded_body),
                "response_body_hash": sha256_hex(&response.body),
                "cryptographic_binding": "pending_0g_clarification"
            }
        }));
        Ok(())
    }

    fn streaming_unsupported() -> UpstreamError {
        UpstreamError::ResponseVerificationUnsupported(STREAMING_UNSUPPORTED.to_string())
    }
}

fn header<'a>(headers: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[async_trait]
impl UpstreamBackend for ZeroGProviderBackend {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn url_origin(&self) -> Option<&str> {
        self.inner.url_origin()
    }

    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        let mut req = req;
        req.body = Self::force_verify_tee(&req.body)?;
        self.inner.prepare(req)
    }

    async fn forward(&self, req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        let prepared = self.prepare(req)?;
        self.forward_prepared(prepared).await
    }

    async fn forward_prepared(
        &self,
        req: PreparedUpstreamRequest,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let forwarded_body = req.request.body.clone();
        let mut response = self.inner.forward_prepared(req).await?;
        Self::verify_buffered_response(&forwarded_body, &mut response)?;
        Ok(response)
    }

    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let forwarded_body = req.request.body.clone();
        let mut response = self.inner.forward_verified_prepared(req, event).await?;
        Self::verify_buffered_response(&forwarded_body, &mut response)?;
        Ok(response)
    }

    async fn forward_stream(
        &self,
        _req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        Err(Self::streaming_unsupported())
    }

    async fn forward_stream_prepared(
        &self,
        _req: PreparedUpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        Err(Self::streaming_unsupported())
    }

    async fn forward_stream_verified_prepared(
        &self,
        _req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        Err(Self::streaming_unsupported())
    }

    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        self.inner.models().await
    }
}
