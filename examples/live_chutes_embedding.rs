//! Live end-to-end probe for the Chutes `/v1/embeddings` E2EE path.
//!
//! Drives the production `ChutesProviderBackend` against `api.chutes.ai`
//! using the real Qwen/Qwen3-Embedding-8B-TEE chute. Builds a verified
//! event manually from the chute's live `/e2e/instances/{chute_id}`
//! response, then runs the ML-KEM-768 / HKDF / ChaCha20-Poly1305 invoke
//! against the chute and decrypts the embedding.
//!
//! This bypasses dstack KMS, the verifier sub-process, and the full
//! gateway HTTP plane — it exercises only the upstream adapter — so it
//! can run as a one-shot probe from a developer workstation:
//!
//! ```text
//! CHUTES_API_KEY=<your-cpk_...> cargo run --example live_chutes_embedding
//! ```
//!
//! Exit code 0 means: encrypted request reached the chute, the chute
//! decrypted it, returned an OpenAI-shape embeddings JSON, and we
//! decrypted that JSON back into a float vector.

use std::collections::HashMap;
use std::env;
use std::process;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use private_ai_gateway::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use private_ai_gateway::aci::upstream::{ChutesProviderBackend, UpstreamBackend, UpstreamRequest};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

const CHUTES_API_BASE: &str = "https://api.chutes.ai";
const MODEL: &str = "Qwen/Qwen3-Embedding-8B-TEE";
const CHUTE_ID: &str = "21822836-bfa6-5426-b27e-dd5fdda1249e";
const CHUTES_MLKEM_768_ALGORITHM: &str = "chutes-ml-kem-768";

#[derive(Debug, Deserialize)]
struct ChutesInstance {
    instance_id: String,
    e2e_pubkey: String,
    nonces: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ChutesInstancesResponse {
    instances: Vec<ChutesInstance>,
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("FAIL: {err}");
        process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let api_key = env::var("CHUTES_API_KEY")
        .map_err(|_| "CHUTES_API_KEY must be set in the environment".to_string())?;

    eprintln!("[1/4] Fetching live instance discovery for chute {CHUTE_ID}...");
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("reqwest builder: {e}"))?;
    let discovery: ChutesInstancesResponse = client
        .get(format!("{CHUTES_API_BASE}/e2e/instances/{CHUTE_ID}"))
        .bearer_auth(&api_key)
        .send()
        .await
        .map_err(|e| format!("discovery send: {e}"))?
        .error_for_status()
        .map_err(|e| format!("discovery http: {e}"))?
        .json()
        .await
        .map_err(|e| format!("discovery json: {e}"))?;
    let instance = discovery
        .instances
        .iter()
        .find(|i| !i.nonces.is_empty())
        .ok_or_else(|| "no chutes instance with available nonces".to_string())?;
    let pubkey_bytes = BASE64
        .decode(&instance.e2e_pubkey)
        .map_err(|e| format!("decode e2e_pubkey: {e}"))?;
    let pubkey_sha256 = hex::encode(Sha256::digest(&pubkey_bytes));
    eprintln!(
        "      instance_id={} pubkey_sha256={}",
        instance.instance_id, pubkey_sha256
    );

    eprintln!("[2/4] Building UpstreamVerifiedEvent and backend...");
    // We're skipping the live NRAS/DCAP attestation chain here because
    // this probe targets the wire protocol, not the verifier; in
    // production both the verifier event AND adapter run inside the
    // gateway. Pinning the binding to the just-fetched pubkey is enough
    // for the adapter's session-acquisition path to succeed.
    let event = UpstreamVerifiedEvent {
        upstream_name: "chutes".to_string(),
        // Live-probe bypass, not a real attestation — leave the provider type
        // unset so the session does not synthesize hardware-proven claims.
        provider: None,
        model_id: MODEL.to_string(),
        url_origin: Some(CHUTES_API_BASE.to_string()),
        verifier_id: "live-probe/bypass/v1".to_string(),
        result: VerificationResult::Verified,
        required: true,
        reason: None,
        evidence: None,
        channel_bindings: vec![ChannelBinding::E2eePublicKeySha256 {
            provider: "chutes".to_string(),
            key_id: Some(instance.instance_id.clone()),
            algorithm: CHUTES_MLKEM_768_ALGORITHM.to_string(),
            public_key_sha256: pubkey_sha256,
        }],
        provider_claims: None,
    };

    let backend = ChutesProviderBackend::new_with_timeouts(CHUTES_API_BASE, 10, 120)
        .map_err(|e| format!("backend init: {e}"))?
        .with_bearer_token(api_key)
        .with_chute_ids([(MODEL.to_string(), CHUTE_ID.to_string())]);

    let body = serde_json::to_vec(&serde_json::json!({
        "model": MODEL,
        "input": "the quick brown fox jumps over the lazy dog"
    }))
    .map_err(|e| format!("serialize body: {e}"))?;
    let request = UpstreamRequest {
        body,
        headers: HashMap::new(),
        path: Some("/v1/embeddings".to_string()),
        target_route_id: None,
        client_authorization: Default::default(),
    };
    let prepared = backend
        .prepare(request)
        .map_err(|e| format!("prepare: {e}"))?;

    eprintln!("[3/4] Invoking /e2e/invoke with X-E2E-Path: /v1/embeddings...");
    let response = backend
        .forward_verified_prepared(prepared, &event)
        .await
        .map_err(|e| format!("forward_verified_prepared: {e}"))?;

    eprintln!(
        "      HTTP {} ({} bytes)",
        response.status_code,
        response.body.len()
    );
    if response.status_code != 200 {
        return Err(format!(
            "upstream returned {}: {}",
            response.status_code,
            String::from_utf8_lossy(&response.body)
        ));
    }

    eprintln!("[4/4] Validating decrypted OpenAI-shape embeddings response...");
    let parsed: Value =
        serde_json::from_slice(&response.body).map_err(|e| format!("response parse: {e}"))?;
    let object = parsed.get("object").and_then(Value::as_str);
    if object != Some("list") {
        return Err(format!("expected object=list, got {object:?}"));
    }
    let data = parsed
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "response missing data array".to_string())?;
    if data.is_empty() {
        return Err("response.data is empty".to_string());
    }
    let first_embedding = data[0]
        .get("embedding")
        .and_then(Value::as_array)
        .ok_or_else(|| "data[0].embedding is not an array".to_string())?;
    let dim = first_embedding.len();
    let nonzero = first_embedding
        .iter()
        .filter(|v| v.as_f64().is_some_and(|f| f != 0.0))
        .count();
    if dim == 0 || nonzero == 0 {
        return Err(format!(
            "embedding looks degenerate (dim={dim}, nonzero={nonzero})"
        ));
    }
    let usage = parsed
        .get("usage")
        .and_then(Value::as_object)
        .ok_or_else(|| "response missing usage object".to_string())?;
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    println!(
        "OK: model={} dim={} first3={:?} total_tokens={}",
        parsed
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("(missing)"),
        dim,
        &first_embedding[..first_embedding.len().min(3)],
        total_tokens
    );
    Ok(())
}
