//! Chutes provider backend: discovery, E2EE `/e2e/invoke` transport, and the
//! per-upstream session store + ML-KEM crypto submodules.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use futures_util::StreamExt;
use ml_kem::ml_kem_768::DecapsulationKey as MlKemDecapsulationKey768;
use serde::Deserialize;
use serde_json::Value;

mod crypto;
mod session;

pub use session::ChutesSessionStore;

use super::tls::response_headers;
use super::{
    OpenAICompatibleBackend, PreparedUpstreamRequest, UpstreamBackend, UpstreamBodyStream,
    UpstreamError, UpstreamRequest, UpstreamResponse, UpstreamStreamResponse,
};
use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use crypto::{
    build_chutes_e2ee_request, chutes_e2ee_pubkey_sha256, decrypt_chutes_response,
    ChutesE2eeDecryptingStream,
};

const CHUTES_DEFAULT_E2EE_API_BASE: &str = "https://api.chutes.ai";
const CHUTES_MLKEM_768_ALGORITHM: &str = "chutes-ml-kem-768";
const CHUTES_MLKEM_CT_SIZE: usize = 1088;
const CHUTES_TAG_SIZE: usize = 16;
const CHUTES_INFO_REQ: &[u8] = b"e2e-req-v1";
const CHUTES_INFO_RESP: &[u8] = b"e2e-resp-v1";
const CHUTES_INFO_STREAM: &[u8] = b"e2e-stream-v1";
const CHUTES_MODEL_CACHE_TTL_SECONDS: u64 = 300;
const CHUTES_DEFAULT_NONCE_TTL_SECONDS: u64 = 55;

/// Chutes provider adapter.
///
/// Chutes binds upstream attestation to an application E2EE public key, so
/// this backend never forwards model requests as plaintext. It fetches a
/// single-use nonce and the live E2EE public key from Chutes, checks that the
/// key matches the verifier-provided channel binding, then sends the request
/// through Chutes' `/e2e/invoke` transport.
pub struct ChutesProviderBackend {
    inner: OpenAICompatibleBackend,
    e2ee_api_base: String,
    api_key: Option<String>,
    chute_ids: HashMap<String, String>,
    client: reqwest::Client,
    session_store: Arc<ChutesSessionStore>,
}

impl ChutesProviderBackend {
    pub fn new_with_timeouts(
        base_url: impl Into<String>,
        connect_timeout_seconds: u64,
        read_timeout_seconds: u64,
    ) -> Result<Self, UpstreamError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(connect_timeout_seconds))
            .read_timeout(Duration::from_secs(read_timeout_seconds))
            .build()
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        Ok(Self {
            inner: OpenAICompatibleBackend::new_with_timeouts(
                base_url,
                connect_timeout_seconds,
                read_timeout_seconds,
            )?
            .with_name("chutes"),
            e2ee_api_base: CHUTES_DEFAULT_E2EE_API_BASE.to_string(),
            api_key: None,
            chute_ids: HashMap::new(),
            client,
            session_store: Arc::new(ChutesSessionStore::new()),
        })
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.inner = self.inner.with_name(name);
        self
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        let token = token.into();
        self.api_key = Some(token.clone());
        self.inner = self.inner.with_bearer_token(token);
        self
    }

    pub fn with_e2ee_api_base(mut self, base_url: impl Into<String>) -> Self {
        self.e2ee_api_base = base_url.into().trim().trim_end_matches('/').to_string();
        self
    }

    pub fn with_chute_ids<I, K, V>(mut self, chute_ids: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.chute_ids = chute_ids
            .into_iter()
            .map(|(model_id, chute_id)| (model_id.into(), chute_id.into()))
            .collect();
        self
    }

    pub fn with_session_store(mut self, session_store: Arc<ChutesSessionStore>) -> Self {
        self.session_store = session_store;
        self
    }

    fn api_key(&self) -> Result<String, UpstreamError> {
        let token = self.api_key.clone().unwrap_or_default();
        if token.trim().is_empty() {
            return Err(UpstreamError::Transport(
                "Chutes E2EE transport requires bearer_token in upstream config".to_string(),
            ));
        }
        Ok(token)
    }

    fn e2ee_requires_verified_binding(&self) -> UpstreamError {
        UpstreamError::Transport(
            "Chutes E2EE transport requires a verified chutes-ml-kem-768 public key binding"
                .to_string(),
        )
    }

    async fn invoke_verified(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
        stream: bool,
    ) -> Result<ChutesInvokeResponse, UpstreamError> {
        if event.result != VerificationResult::Verified {
            return Err(self.e2ee_requires_verified_binding());
        }
        let api_key = self.api_key()?;
        let payload: Value = serde_json::from_slice(&req.request.body)
            .map_err(|e| UpstreamError::Routing(format!("invalid JSON request body: {e}")))?;
        let model = payload
            .get("model")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                UpstreamError::Routing("request body must contain a string model field".to_string())
            })?;
        let chute_id = self.resolve_chute_id(model, &api_key).await?;
        let accepted = chutes_accepted_bindings(event)?;
        let selected = self
            .acquire_verified_chutes_session(&chute_id, &api_key, &accepted)
            .await?;
        let encrypted = build_chutes_e2ee_request(&selected.e2e_pubkey, payload)?;
        let headers = chutes_invoke_headers(
            &api_key,
            &chute_id,
            &selected.instance_id,
            &selected.nonce,
            stream,
            req.request
                .path
                .as_deref()
                .unwrap_or("/v1/chat/completions"),
        );
        let url = format!("{}/e2e/invoke", self.e2ee_api_base);
        let mut builder = self.client.post(url).body(encrypted.blob);
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        let status_code = resp.status().as_u16();
        let headers = response_headers(&resp);
        Ok(ChutesInvokeResponse {
            status_code,
            headers,
            response_sk: encrypted.response_sk,
            response: resp,
        })
    }

    async fn resolve_chute_id(&self, model: &str, api_key: &str) -> Result<String, UpstreamError> {
        if looks_like_uuid(model) {
            return Ok(model.to_string());
        }
        if let Some(chute_id) = self.chute_ids.get(model) {
            return Ok(chute_id.clone());
        }
        if let Some(chute_id) = self.session_store.cached_chute_id(model) {
            return Ok(chute_id);
        }
        let url = format!("{}/chutes/", self.e2ee_api_base);
        let resp = self
            .client
            .get(url)
            .query(&[("include_public", "true"), ("name", model)])
            .header("authorization", format!("Bearer {api_key}"))
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp
            .bytes()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(UpstreamError::Upstream {
                status,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }
        let chutes: ChutesLookupResponse =
            serde_json::from_slice(&body).map_err(|e| UpstreamError::Transport(e.to_string()))?;
        let chute_id = chutes
            .items
            .iter()
            .find(|entry| entry.name.as_deref() == Some(model) && entry.chute_id.is_some())
            .and_then(|entry| entry.chute_id.clone())
            .ok_or_else(|| {
                UpstreamError::Routing(format!(
                    "Chutes /chutes lookup did not return an exact chute_id match for model {model:?}"
                ))
            })?;
        self.session_store.cache_chute_id(model, &chute_id);
        Ok(chute_id)
    }

    async fn fetch_instances(
        &self,
        chute_id: &str,
        api_key: &str,
    ) -> Result<ChutesInstancesResponse, UpstreamError> {
        let url = format!("{}/e2e/instances/{chute_id}", self.e2ee_api_base);
        let resp = self
            .client
            .get(url)
            .header("authorization", format!("Bearer {api_key}"))
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp
            .bytes()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(UpstreamError::Upstream {
                status,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }
        serde_json::from_slice(&body).map_err(|e| UpstreamError::Transport(e.to_string()))
    }

    async fn acquire_verified_chutes_session(
        &self,
        chute_id: &str,
        api_key: &str,
        accepted: &[ChutesAcceptedBinding],
    ) -> Result<SelectedChutesInstance, UpstreamError> {
        if let Some(selected) = self.pop_verified_chutes_nonce(chute_id, accepted)? {
            return Ok(selected);
        }

        let _refill_guard = self.session_store.refill_lock.lock().await;
        if let Some(selected) = self.pop_verified_chutes_nonce(chute_id, accepted)? {
            return Ok(selected);
        }
        let discovery = self.fetch_instances(chute_id, api_key).await?;
        self.cache_verified_chutes_nonces(chute_id, discovery, accepted)?;
        self.pop_verified_chutes_nonce(chute_id, accepted)?
            .ok_or_else(|| {
                UpstreamError::ChannelBindingMismatch(
                    "Chutes did not return an E2EE key matching the verified binding".to_string(),
                )
            })
    }

    fn pop_verified_chutes_nonce(
        &self,
        chute_id: &str,
        accepted: &[ChutesAcceptedBinding],
    ) -> Result<Option<SelectedChutesInstance>, UpstreamError> {
        self.session_store.pop_verified_nonce(chute_id, accepted)
    }

    fn cache_verified_chutes_nonces(
        &self,
        chute_id: &str,
        discovery: ChutesInstancesResponse,
        accepted: &[ChutesAcceptedBinding],
    ) -> Result<usize, UpstreamError> {
        let verified = verified_discovery_from_response(chute_id, discovery, Some(accepted))?;
        self.session_store.record_verified_discovery(verified)
    }

    pub async fn refresh_verified_sessions_for_model(
        &self,
        model: &str,
        event: &UpstreamVerifiedEvent,
    ) -> Result<usize, UpstreamError> {
        if event.result != VerificationResult::Verified {
            return Err(self.e2ee_requires_verified_binding());
        }
        let api_key = self.api_key()?;
        let chute_id = self.resolve_chute_id(model, &api_key).await?;
        let accepted = chutes_accepted_bindings(event)?;
        let discovery = self.fetch_instances(&chute_id, &api_key).await?;
        self.cache_verified_chutes_nonces(&chute_id, discovery, &accepted)
    }

    /// `GET {api_base}/chutes/{chute_id}/evidence?nonce={nonce}` → per-instance
    /// TDX quote + GPU evidence.
    async fn fetch_evidence(
        &self,
        chute_id: &str,
        nonce: &str,
        api_key: &str,
    ) -> Result<Vec<ChutesInstanceEvidence>, UpstreamError> {
        let url = format!("{}/chutes/{chute_id}/evidence", self.e2ee_api_base);
        let resp = self
            .client
            .get(url)
            .query(&[("nonce", nonce)])
            .header("authorization", format!("Bearer {api_key}"))
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp
            .bytes()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(UpstreamError::Upstream {
                status,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }
        let parsed: ChutesEvidenceResponse =
            serde_json::from_slice(&body).map_err(|e| UpstreamError::Transport(e.to_string()))?;
        Ok(parsed.evidence)
    }
}

#[async_trait]
impl UpstreamBackend for ChutesProviderBackend {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn url_origin(&self) -> Option<&str> {
        self.inner.url_origin()
    }

    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        self.inner.prepare(req)
    }

    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        Err(self.e2ee_requires_verified_binding())
    }

    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let invoke = self.invoke_verified(req, event, false).await?;
        let status_code = invoke.status_code;
        let headers = invoke.headers;
        let body = invoke
            .response
            .bytes()
            .await
            .map_err(|e| UpstreamError::Transport(e.to_string()))?
            .to_vec();
        if status_code != 200 {
            return Ok(UpstreamResponse {
                status_code,
                body,
                headers,
            });
        }
        let body = decrypt_chutes_response(&body, &invoke.response_sk)?;
        Ok(UpstreamResponse {
            status_code,
            body,
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
        })
    }

    async fn forward_stream(
        &self,
        _req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        Err(self.e2ee_requires_verified_binding())
    }

    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        let invoke = self.invoke_verified(req, event, true).await?;
        let status_code = invoke.status_code;
        let mut headers = invoke.headers;
        let raw_body = invoke
            .response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|e| UpstreamError::Transport(e.to_string())));
        let body: UpstreamBodyStream = if status_code == 200 {
            headers.insert("content-type".to_string(), "text/event-stream".to_string());
            Box::pin(ChutesE2eeDecryptingStream::new(
                Box::pin(raw_body),
                invoke.response_sk,
            ))
        } else {
            Box::pin(raw_body)
        };
        Ok(UpstreamStreamResponse {
            status_code,
            headers,
            body,
        })
    }

    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        self.inner.models().await
    }

    /// Fetch the Chutes GPU evidence bound to `nonce` and shape it as the legacy
    /// `nvidia_payload` so the gateway can merge it into its own attestation
    /// report (same shape as every other provider).
    async fn chutes_nvidia_payload(
        &self,
        model: &str,
        nonce: &str,
    ) -> Result<Value, UpstreamError> {
        let api_key = self.api_key()?;
        let chute_id = self.resolve_chute_id(model, &api_key).await?;
        let evidence = self.fetch_evidence(&chute_id, nonce, &api_key).await?;
        // Chutes returns per-instance GPU evidence; the nvidia_payload carries a
        // single instance's evidence_list (one NRAS verification), matching the
        // {nonce, evidence_list, arch} shape used by the other providers. Pick the
        // first instance whose evidence is a non-empty list with a usable arch —
        // an instance can report empty/malformed evidence, so don't blindly take
        // the first, and never fabricate a default arch.
        let (evidence_list, arch) = evidence
            .into_iter()
            .find_map(|item| {
                let arch = item
                    .gpu_evidence
                    .as_array()
                    .and_then(|list| list.first())
                    .and_then(|e| e.get("arch"))
                    .and_then(Value::as_str)
                    .filter(|arch| !arch.is_empty())?
                    .to_string();
                Some((item.gpu_evidence, arch))
            })
            .ok_or_else(|| {
                UpstreamError::Routing("chutes returned no usable GPU evidence".to_string())
            })?;
        // Chutes does not sign the raw request nonce into the GPU report: it binds
        // the report to a value derived from the request nonce and the instance
        // E2EE key. NRAS checks the nonce the client submits against the one
        // actually inside the report, so echo the attested nonce read from the
        // evidence (not the raw request nonce); otherwise the client's standard
        // NRAS check fails with "nonce missing in JWT". Report freshness is still
        // anchored by the gateway quote, which binds the raw request nonce.
        let report_nonce = gpu_report_nonce(&evidence_list).unwrap_or_else(|| nonce.to_string());
        let payload = serde_json::json!({
            "nonce": report_nonce,
            "evidence_list": evidence_list,
            "arch": arch,
        });
        Ok(Value::String(payload.to_string()))
    }
}

struct ChutesInvokeResponse {
    status_code: u16,
    headers: HashMap<String, String>,
    response_sk: MlKemDecapsulationKey768,
    response: reqwest::Response,
}

#[derive(Debug, Deserialize)]
struct ChutesLookupResponse {
    items: Vec<ChutesLookupEntry>,
}

#[derive(Debug, Deserialize)]
struct ChutesLookupEntry {
    name: Option<String>,
    chute_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChutesInstancesResponse {
    instances: Vec<ChutesInstanceInfo>,
    #[serde(default)]
    nonce_expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ChutesInstanceInfo {
    instance_id: String,
    e2e_pubkey: String,
    nonces: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ChutesEvidenceResponse {
    #[serde(default)]
    evidence: Vec<ChutesInstanceEvidence>,
}

#[derive(Debug, Deserialize)]
struct ChutesInstanceEvidence {
    #[serde(default)]
    gpu_evidence: Value,
}

/// The 32-byte nonce the GPU actually signed into its attestation report, read
/// straight from the report. NVIDIA's GPU evidence begins with a 4-byte header
/// followed by the 32-byte nonce (verified against both Hopper and Blackwell
/// reports); the gateway echoes this exact value so the client's NRAS check
/// matches, without needing to know how Chutes derived it. Returns `None` if the
/// evidence is missing or too short to hold the field.
fn gpu_report_nonce(evidence_list: &Value) -> Option<String> {
    let evidence = evidence_list
        .as_array()?
        .first()?
        .get("evidence")?
        .as_str()?;
    let raw = BASE64.decode(evidence).ok()?;
    raw.get(4..36).map(hex::encode)
}

#[derive(Debug)]
struct SelectedChutesInstance {
    instance_id: String,
    e2e_pubkey: String,
    nonce: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChutesVerifiedDiscovery {
    pub chute_id: String,
    #[serde(default)]
    pub nonce_expires_in: Option<u64>,
    #[serde(default)]
    pub instances: Vec<ChutesVerifiedInstance>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChutesVerifiedInstance {
    pub instance_id: String,
    pub e2e_pubkey: String,
    pub public_key_sha256: String,
    #[serde(default)]
    pub nonces: Vec<String>,
}

#[derive(Debug)]
struct ChutesAcceptedBinding {
    key_id: Option<String>,
    public_key_sha256: String,
}

struct ChutesE2eeRequest {
    blob: Vec<u8>,
    response_sk: MlKemDecapsulationKey768,
}

fn looks_like_uuid(value: &str) -> bool {
    let parts = value.split('-').collect::<Vec<_>>();
    parts.len() == 5
        && value.len() == 36
        && value.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
}

fn chutes_invoke_headers(
    api_key: &str,
    chute_id: &str,
    instance_id: &str,
    nonce: &str,
    stream: bool,
    e2e_path: &str,
) -> HashMap<&'static str, String> {
    HashMap::from([
        ("authorization", format!("Bearer {api_key}")),
        ("x-chute-id", chute_id.to_string()),
        ("x-instance-id", instance_id.to_string()),
        ("x-e2e-nonce", nonce.to_string()),
        ("x-e2e-stream", stream.to_string()),
        ("x-e2e-path", e2e_path.to_string()),
        ("content-type", "application/octet-stream".to_string()),
    ])
}

fn verified_discovery_from_response(
    chute_id: &str,
    discovery: ChutesInstancesResponse,
    accepted: Option<&[ChutesAcceptedBinding]>,
) -> Result<ChutesVerifiedDiscovery, UpstreamError> {
    let mut matched_verified_key = false;
    let mut instances = Vec::new();
    for instance in discovery.instances {
        let public_key_sha256 = chutes_e2ee_pubkey_sha256(&instance.e2e_pubkey)?;
        let accepted = accepted
            .map(|accepted| {
                chutes_binding_matches(accepted, &instance.instance_id, &public_key_sha256)
            })
            .unwrap_or(true);
        if accepted {
            matched_verified_key = true;
            instances.push(ChutesVerifiedInstance {
                instance_id: instance.instance_id,
                e2e_pubkey: instance.e2e_pubkey,
                public_key_sha256,
                nonces: instance.nonces,
            });
        }
    }
    if !matched_verified_key {
        return Err(UpstreamError::ChannelBindingMismatch(
            "Chutes did not return an E2EE key matching the verified binding".to_string(),
        ));
    }
    Ok(ChutesVerifiedDiscovery {
        chute_id: chute_id.to_string(),
        nonce_expires_in: discovery.nonce_expires_in,
        instances,
    })
}

fn chutes_accepted_bindings(
    event: &UpstreamVerifiedEvent,
) -> Result<Vec<ChutesAcceptedBinding>, UpstreamError> {
    let accepted = event
        .channel_bindings
        .iter()
        .filter_map(|binding| match binding {
            ChannelBinding::E2eePublicKeySha256 {
                provider,
                key_id,
                algorithm,
                public_key_sha256,
            } if provider == "chutes" && algorithm == CHUTES_MLKEM_768_ALGORITHM => {
                Some(ChutesAcceptedBinding {
                    key_id: key_id.clone(),
                    public_key_sha256: public_key_sha256.clone(),
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if accepted.is_empty() {
        return Err(UpstreamError::Transport(
            "verified Chutes event did not include an E2EE key binding".to_string(),
        ));
    }
    Ok(accepted)
}

fn chutes_binding_matches(
    accepted: &[ChutesAcceptedBinding],
    instance_id: &str,
    public_key_sha256: &str,
) -> bool {
    accepted.iter().any(|binding| {
        binding
            .key_id
            .as_deref()
            .is_none_or(|key_id| key_id == instance_id)
            && binding
                .public_key_sha256
                .eq_ignore_ascii_case(public_key_sha256)
    })
}
