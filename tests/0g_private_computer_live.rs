//! Live 0G Private Computer test: private-tier verification, then one buffered
//! and one streamed glm-5.3 completion, each released only after the
//! provider's signature verifies.
//!
//! `PRIVATE_AI_GATEWAY_0G_API_KEY_FILE=/path/to/key cargo test --test 0g_private_computer_live -- --ignored --nocapture`

use futures_util::StreamExt;
use private_ai_gateway::aci::receipt::VerificationResult;
use private_ai_gateway::aci::upstream::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamRequest, ZeroGProviderBackend,
};
use private_ai_gateway::aci::verifier::ZeroGProviderVerifier;
use private_ai_gateway::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};
use serde_json::Value;

const ROUTER: &str = "https://router-api.0g.ai";
const SERVING_CONTRACT: &str = "0x47340d900bdFec2BD393c626E12ea0656F938d84";
const MODEL: &str = "glm-5.3";

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
    let api_key = std::env::var_os("PRIVATE_AI_GATEWAY_0G_API_KEY_FILE")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty());
    let Some(api_key) = api_key else {
        eprintln!("skipping: PRIVATE_AI_GATEWAY_0G_API_KEY_FILE is unset, unreadable, or empty");
        return;
    };
    let chain_rpc = std::env::var("PRIVATE_AI_GATEWAY_0G_CHAIN_RPC_URL")
        .unwrap_or_else(|_| "https://evmrpc.0g.ai".to_string());

    let verifier =
        ZeroGProviderVerifier::new(ROUTER, &api_key, chain_rpc, SERVING_CONTRACT, None, 60, 0)
            .unwrap();
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
        "{:?}",
        event.reason
    );
    eprintln!(
        "verified: {}",
        event.provider_claims.as_ref().unwrap()["instances"]
    );

    let backend = ZeroGProviderBackend::new_with_timeouts(ROUTER, api_key, 10, 300).unwrap();
    let buffered = backend
        .forward_verified_prepared(prepared(false), &event)
        .await
        .expect("buffered completion opens and its signature verifies");
    let body: Value = serde_json::from_slice(&buffered.body).unwrap();
    assert!(body["choices"].as_array().is_some_and(|c| !c.is_empty()));

    let events: Vec<String> = backend
        .forward_stream_verified_prepared(prepared(true), &event)
        .await
        .expect("stream starts")
        .body
        .map(|item| {
            String::from_utf8(item.expect("frame opens and signature verifies").to_vec()).unwrap()
        })
        .collect()
        .await;
    assert!(events.len() > 1);
    assert_eq!(events.last().map(String::as_str), Some("data: [DONE]\n\n"));
}
