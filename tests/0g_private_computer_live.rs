//! Live 0G Private Computer test of the native provider: private-tier
//! verification, then one buffered and one streamed glm-5.3 completion sealed
//! end to end and released only after the provider's signature verifies.
//!
//! Run with:
//! `PRIVATE_AI_GATEWAY_0G_API_KEY_FILE=/path/to/key cargo test --test 0g_private_computer_live -- --ignored --nocapture`

use std::path::Path;

use futures_util::StreamExt;
use private_ai_gateway::aci::receipt::VerificationResult;
use private_ai_gateway::aci::upstream::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamRequest, ZeroGProviderBackend,
};
use private_ai_gateway::aci::verifier::ZeroGProviderVerifier;
use private_ai_gateway::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};
use serde_json::Value;

const API_KEY_FILE_ENV: &str = "PRIVATE_AI_GATEWAY_0G_API_KEY_FILE";
const CHAIN_RPC_ENV: &str = "PRIVATE_AI_GATEWAY_0G_CHAIN_RPC_URL";
const ROUTER: &str = "https://router-api.0g.ai";
const DEFAULT_CHAIN_RPC: &str = "https://evmrpc.0g.ai";
const SERVING_CONTRACT: &str = "0x47340d900bdFec2BD393c626E12ea0656F938d84";
const MODEL: &str = "glm-5.3";

fn read_api_key() -> Option<String> {
    let path = std::env::var_os(API_KEY_FILE_ENV)?;
    let bytes = std::fs::read(Path::new(&path)).ok()?;
    let key = String::from_utf8(bytes).ok()?;
    let key = key.trim();
    (!key.is_empty()).then(|| key.to_owned())
}

fn prepared(stream: bool) -> PreparedUpstreamRequest {
    let body = serde_json::json!({
        "model": MODEL,
        "stream": stream,
        "messages": [{"role": "user", "content": "Reply with the word ok."}],
        "max_completion_tokens": 256,
    });
    PreparedUpstreamRequest {
        request: UpstreamRequest {
            body: serde_json::to_vec(&body).unwrap(),
            path: Some("/v1/chat/completions".to_string()),
            ..Default::default()
        },
        upstream_name: "zero-g".to_string(),
        url_origin: Some(ROUTER.to_string()),
        model_id: MODEL.to_string(),
        route_id: None,
        is_tee: Some(true),
    }
}

#[tokio::test]
#[ignore = "requires PRIVATE_AI_GATEWAY_0G_API_KEY_FILE and sends live glm-5.3 inference"]
async fn glm_5_3_is_sealed_verified_and_signed() {
    let Some(api_key) = read_api_key() else {
        eprintln!("skipping: {API_KEY_FILE_ENV} is unset, unreadable, or empty");
        return;
    };
    let chain_rpc = std::env::var(CHAIN_RPC_ENV).unwrap_or_else(|_| DEFAULT_CHAIN_RPC.to_string());

    let verifier = ZeroGProviderVerifier::new(ROUTER, chain_rpc, SERVING_CONTRACT, None, 10, 60, 0)
        .expect("build 0G verifier")
        .with_api_key(api_key.clone());
    let event = verifier
        .verify(UpstreamVerificationRequest {
            upstream_name: "zero-g".to_string(),
            url_origin: Some(ROUTER.to_string()),
            model_id: MODEL.to_string(),
            forwarded_body_hash: String::new(),
            required: true,
        })
        .await;
    assert_eq!(
        event.result,
        VerificationResult::Verified,
        "private-tier verification: {:?}",
        event.reason
    );
    eprintln!(
        "verified providers: {:?}",
        event
            .provider_claims
            .as_ref()
            .map(|claims| &claims["instances"])
    );

    let backend = ZeroGProviderBackend::new_with_timeouts(ROUTER, 10, 300)
        .expect("build 0G backend")
        .with_bearer_token(api_key);

    let buffered = backend
        .forward_verified_prepared(prepared(false), &event)
        .await
        .expect("buffered completion is opened and its signature verifies");
    assert_eq!(buffered.status_code, 200);
    let body: Value = serde_json::from_slice(&buffered.body).expect("JSON completion");
    assert!(body["choices"].as_array().is_some_and(|c| !c.is_empty()));
    eprintln!("buffered served by {:?}", buffered.served_instance_id);

    let streamed = backend
        .forward_stream_verified_prepared(prepared(true), &event)
        .await
        .expect("stream starts");
    assert_eq!(streamed.status_code, 200);
    let events: Vec<String> = streamed
        .body
        .map(|item| {
            let bytes = item.expect("every streamed frame opens and the signature verifies");
            String::from_utf8(bytes.to_vec()).unwrap()
        })
        .collect()
        .await;
    assert!(events.len() > 1, "at least one frame before [DONE]");
    assert_eq!(events.last().map(String::as_str), Some("data: [DONE]\n\n"));
}
