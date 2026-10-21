//! Live 0G Private Computer test: private-tier verification, then a buffered,
//! a streamed and a sealed-JSON-mode completion, each released only after the
//! provider's signature verifies. `PRIVATE_AI_GATEWAY_0G_MODEL` overrides the model, as
//! the provider's on-chain id (default `glm-5.3`; e.g. `0GM-1.0-35B-A3B`).
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

const ROUTER: &str = "https://router-api-cloud.0g.ai";
const SERVING_CONTRACT: &str = "0x47340d900bdFec2BD393c626E12ea0656F938d84";
/// Boot-chain pins for the dstack images 0G publishes for its providers
/// (`client/evidence/brokerimages.json`): dstack-nvidia-0.5.4.1 and 0.5.9.
const BOOT_CHAINS: [&str; 2] = [
    "tdx-boot-chain:sha256:536a92e7c6204087b2c2910e4c17083b7a9f8b3aa455b6bc14c6df3df2f1a7fc",
    "tdx-boot-chain:sha256:bebbc1f26600aa7e415962d919d1919613d7be1d3ce9f4043c19f6785da11c0e",
];

fn model() -> String {
    std::env::var("PRIVATE_AI_GATEWAY_0G_MODEL").unwrap_or_else(|_| "glm-5.3".to_string())
}

fn prepared(stream: bool) -> PreparedUpstreamRequest {
    prepared_body(serde_json::json!({
        "model": model(),
        "stream": stream,
        "messages": [{"role": "user", "content": "Reply with the word ok."}],
        "max_completion_tokens": 256,
    }))
}

fn prepared_body(body: Value) -> PreparedUpstreamRequest {
    PreparedUpstreamRequest {
        request: UpstreamRequest {
            body: serde_json::to_vec(&body).unwrap(),
            path: Some("/v1/chat/completions".to_string()),
            ..Default::default()
        },
        upstream_name: "zero-g".to_string(),
        url_origin: Some(ROUTER.to_string()),
        model_id: model(),
        route_id: None,
        is_tee: Some(true),
    }
}

#[tokio::test]
#[ignore = "requires PRIVATE_AI_GATEWAY_0G_API_KEY_FILE and sends live inference"]
async fn completion_is_sealed_verified_and_signed() {
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
            .unwrap()
            .with_accepted_subjects(BOOT_CHAINS.map(String::from));
    let event = verifier
        .verify(UpstreamVerificationRequest {
            upstream_name: "zero-g".to_string(),
            url_origin: Some(ROUTER.to_string()),
            model_id: model(),
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
    // The router's unbound `x_0g_trace` never reaches the caller, and `model`
    // is the one the request bound.
    assert!(body.get("x_0g_trace").is_none());
    assert_eq!(body["model"], model());

    // `response_format` and the translated `reasoning` travel sealed.
    let json_mode = backend
        .forward_verified_prepared(
            prepared_body(serde_json::json!({
                "model": model(),
                "messages": [{"role": "user", "content": "Give the capital of France as JSON with the key capital."}],
                "response_format": {"type": "json_object"},
                "reasoning": {"effort": "low"},
                "max_completion_tokens": 1024,
            })),
            &event,
        )
        .await
        .expect("sealed JSON-mode completion opens and its signature verifies");
    let body: Value = serde_json::from_slice(&json_mode.body).unwrap();
    let content = body["choices"][0]["message"]["content"].as_str().unwrap();
    assert!(
        serde_json::from_str::<Value>(content).is_ok_and(|v| v.is_object()),
        "{content}"
    );

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
    assert!(events.iter().all(|event| !event.contains("x_0g_trace")));
}
