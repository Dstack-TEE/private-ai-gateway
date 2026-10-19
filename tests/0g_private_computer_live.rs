//! Live 0G Private Computer smoke test.
//!
//! Run with:
//! `PRIVATE_AI_GATEWAY_0G_API_KEY_FILE=/path/to/key cargo test --test 0g_private_computer_live -- --ignored`

use std::path::Path;

use reqwest::header::{HeaderValue, AUTHORIZATION};
use serde_json::Value;

const API_KEY_FILE_ENV: &str = "PRIVATE_AI_GATEWAY_0G_API_KEY_FILE";
const ENDPOINT_ENV: &str = "PRIVATE_AI_GATEWAY_0G_TEST_ENDPOINT";
const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8086";

fn read_api_key() -> Option<String> {
    let path = std::env::var_os(API_KEY_FILE_ENV)?;
    let bytes = std::fs::read(Path::new(&path)).ok()?;
    let key = String::from_utf8(bytes).ok()?;
    let key = key.trim();
    (!key.is_empty()).then(|| key.to_owned())
}

#[tokio::test]
#[ignore = "requires PRIVATE_AI_GATEWAY_0G_API_KEY_FILE and a running 0G sidecar"]
async fn glm_5_3_private_computer_is_sealed() {
    let Some(api_key) = read_api_key() else {
        return;
    };

    let endpoint = std::env::var(ENDPOINT_ENV).unwrap_or_else(|_| DEFAULT_ENDPOINT.to_owned());
    let url = format!("{}/v1/chat/completions", endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("build live 0G HTTP client");

    let authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
        .expect("0G API key must be valid in an Authorization header");
    let response = client
        .post(url)
        .header(AUTHORIZATION, authorization)
        .header("X-0G-Provider-Trust-Mode", "private")
        .header("X-0G-Provider-Allow-Fallbacks", "false")
        .json(&serde_json::json!({
            "model": "glm-5.3",
            "messages": [{"role": "user", "content": "Reply with the word ok."}],
            "max_completion_tokens": 32,
            "stream": false
        }))
        .send()
        .await
        .expect("send 0G glm-5.3 request")
        .error_for_status()
        .expect("0G glm-5.3 request must succeed");

    assert_eq!(
        response
            .headers()
            .get("X-0G-E2EE")
            .and_then(|value| value.to_str().ok()),
        Some("sealed"),
        "0G response must be marked sealed"
    );

    let body: Value = response
        .json()
        .await
        .expect("0G response must be valid JSON");
    assert_eq!(
        body.get("object").and_then(Value::as_str),
        Some("chat.completion")
    );
    assert!(body
        .get("choices")
        .and_then(Value::as_array)
        .is_some_and(|choices| !choices.is_empty()));
}
