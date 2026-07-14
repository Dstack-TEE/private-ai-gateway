//! End-to-end test of the `aci` CLI's offline audit: serve a real in-process
//! ACI service over HTTP, capture the artifacts a client would save (report,
//! request/response bytes, receipt, attested session), then run `aci audit`
//! on them and check every transcript status — including the honest failures
//! for the stub quote, which is not a real DCAP quote.

mod common;

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use private_ai_gateway::aci::digest::sha256_hex;
use private_ai_gateway::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent};
use private_ai_gateway::aci::upstream::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
};
use private_ai_gateway::aci::verifier::StaticUpstreamVerifier;
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore,
};
use private_ai_gateway::http::build_router;
use serde_json::Value;

use common::{verified_event, StaticKeyProvider, StubQuoter};

const NONCE: &str = "cd20088d763605cf78564e5b35524ad52715419624b76e029582a3652758708d";
const REQUEST_BODY: &[u8] =
    br#"{"model":"demo-model","messages":[{"role":"user","content":"hi"}]}"#;
const RESPONSE_BODY: &[u8] = br#"{"id":"chat-xyz","object":"chat.completion","choices":[]}"#;

/// The checked-in §5.1 report shape, captured byte-exact from this in-process
/// service (deterministic: fixed keys, stub quote, fixed clock, [`NONCE`]).
/// Regenerate with `ACI_UPDATE_FIXTURES=1 cargo test --test aci_cli`.
const REPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/aci_report_fixture.json"
);

struct StubUpstream {
    body: Vec<u8>,
}

#[async_trait]
impl UpstreamBackend for StubUpstream {
    fn name(&self) -> &str {
        "stub-upstream"
    }
    fn url_origin(&self) -> Option<&str> {
        Some("https://stub-upstream")
    }
    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        Ok(UpstreamResponse {
            status_code: 200,
            body: self.body.clone(),
            headers,
            served_instance_id: None,
        })
    }
    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        _event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.forward(req.request).await
    }
}

fn verifier_event() -> UpstreamVerifiedEvent {
    let evidence = br#"{"fixture":"stub-upstream-attestation"}"#;
    UpstreamVerifiedEvent {
        url_origin: Some("https://stub-upstream".to_string()),
        verifier_id: "stub-verifier-1".to_string(),
        evidence: Some(serde_json::json!({
            "digest": sha256_hex(evidence),
            "data": format!("data:application/json;base64,{}", BASE64.encode(evidence)),
        })),
        channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
            origin: "https://stub-upstream".to_string(),
            spki_sha256: "aa".repeat(32),
        }],
        ..verified_event("stub-upstream", "demo-model")
    }
}

fn check_status(output: &Value, id: &str) -> String {
    output["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("check {id} missing"))["status"]
        .as_str()
        .expect("status string")
        .to_string()
}

#[tokio::test]
async fn audit_verifies_artifacts_captured_from_a_live_service() {
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(StaticKeyProvider::default()),
            Arc::new(StubQuoter::default()),
            Arc::new(StubUpstream {
                body: RESPONSE_BODY.to_vec(),
            }),
            Arc::new(StaticUpstreamVerifier::new(verifier_event())),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test(),
            Arc::new(FixedClock(1_700_000_000)),
        )
        .unwrap(),
    );
    let app = build_router(service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Capture the artifacts exactly as a client would save them.
    let http = reqwest::Client::new();
    let report_bytes = http
        .get(format!("{base}/v1/aci/attestation?nonce={NONCE}"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    if std::env::var_os("ACI_UPDATE_FIXTURES").is_some() {
        std::fs::write(REPORT_FIXTURE, &report_bytes).unwrap();
    }
    assert_eq!(
        report_bytes.as_ref(),
        std::fs::read(REPORT_FIXTURE).expect("read report fixture"),
        "the served report drifted from tests/fixtures/aci_report_fixture.json; \
         regenerate with ACI_UPDATE_FIXTURES=1 cargo test --test aci_cli"
    );
    let chat = http
        .post(format!("{base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(REQUEST_BODY.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(chat.status().as_u16(), 200);
    let receipt_id = chat
        .headers()
        .get("x-receipt-id")
        .expect("x-receipt-id header")
        .to_str()
        .unwrap()
        .to_string();
    let response_bytes = chat.bytes().await.unwrap();
    let receipt_bytes = http
        .get(format!("{base}/v1/aci/receipts/{receipt_id}"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    // The endpoint serves the §8.2 envelope; the event log lives in the
    // decoded payload bytes.
    let envelope: Value = serde_json::from_slice(&receipt_bytes).unwrap();
    let payload: Value = serde_json::from_slice(
        &BASE64
            .decode(envelope["payload_b64"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    let session_id = payload["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "upstream.verified")
        .and_then(|e| e["session_id"].as_str())
        .expect("upstream.verified with session_id")
        .to_string();
    let session_bytes = http
        .get(format!("{base}/v1/aci/sessions/{session_id}"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();

    let dir = std::env::temp_dir().join(format!("aci-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = |name: &str| dir.join(name).to_str().unwrap().to_string();
    std::fs::write(path("report.json"), &report_bytes).unwrap();
    std::fs::write(path("receipt.json"), &receipt_bytes).unwrap();
    std::fs::write(path("request.json"), REQUEST_BODY).unwrap();
    std::fs::write(path("response.json"), &response_bytes).unwrap();
    std::fs::write(path("session.json"), &session_bytes).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_aci"))
        .args([
            "audit",
            "--report",
            &path("report.json"),
            "--receipt",
            &path("receipt.json"),
            "--nonce",
            NONCE,
            "--request-body",
            &path("request.json"),
            "--response-body",
            &path("response.json"),
            "--session",
            &path("session.json"),
            "--json",
        ])
        .output()
        .expect("run aci audit");
    let transcript: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("bad JSON output ({e}): {:?}", output));

    // The service-signed artifacts all verify offline...
    for id in ["L2.2", "L2.3", "R.1", "R.2", "R.3", "R.4", "U.1", "U.2"] {
        assert_eq!(check_status(&transcript, id), "pass", "check {id}");
    }
    // ...while the stub quote is not a real DCAP quote and must fail-close, and
    // the skipped checks (no parseable quote / no app_compose) stay skipped.
    assert_eq!(check_status(&transcript, "L2.1"), "fail");
    assert_eq!(check_status(&transcript, "L2.4"), "skip");
    assert_eq!(check_status(&transcript, "L2.5"), "skip");
    assert_eq!(check_status(&transcript, "L2.6"), "skip");
    assert_eq!(transcript["verdict"]["verified"], false);
    assert!(!output.status.success());

    std::fs::remove_dir_all(&dir).ok();
}

/// The checked-in fixture audits offline through the real binary. Expiry is
/// skipped (never passed) so the test does not depend on the wall clock; the
/// live-capture test above covers L2.3 against real time.
#[test]
fn audit_verifies_the_checked_in_report_fixture() {
    let output = Command::new(env!("CARGO_BIN_EXE_aci"))
        .args([
            "audit",
            "--report",
            REPORT_FIXTURE,
            "--nonce",
            NONCE,
            "--skip-expiry",
            "--json",
        ])
        .output()
        .expect("run aci audit");
    let transcript: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("bad JSON output ({e}): {output:?}"));

    assert_eq!(check_status(&transcript, "L2.2"), "pass");
    assert_eq!(check_status(&transcript, "L2.3"), "skip");
    // No parseable quote / no app_compose from the stub: L2.4 skips honestly.
    assert_eq!(check_status(&transcript, "L2.4"), "skip");
    // The stub quote is not a real DCAP quote: fail-closed, never skipped.
    assert_eq!(check_status(&transcript, "L2.1"), "fail");
    assert_eq!(check_status(&transcript, "L2.5"), "skip");
    assert_eq!(check_status(&transcript, "L2.6"), "skip");
    assert_eq!(transcript["verdict"]["verified"], false);
    assert!(!output.status.success());
}

/// A report in the pre-simplification shape (typed keyset object, identity
/// key, endorsement — no `workload_keyset_b64`) must fail closed at parse
/// time with a message naming the missing field, not run any checks.
#[test]
fn old_protocol_report_fails_closed_with_a_clear_message() {
    let old_shape = serde_json::json!({
        "api_version": "aci/1",
        "workload_id": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
        "workload_keyset_digest":
            "sha256:2222222222222222222222222222222222222222222222222222222222222222",
        "attestation": {
            "tee_type": "tdx",
            "workload_keyset": {
                "workload_identity": { "algo": "secp256k1", "public_key": "02ab" },
                "keyset_epoch": { "version": 1, "not_after": 1_800_000_000 },
                "receipt_signing_keys": [],
                "e2ee_public_keys": [],
                "tls_public_keys": []
            },
            "report_data": "33".repeat(32),
            "keyset_endorsement": { "algo": "secp256k1", "value": "deadbeef" },
            "evidence": {}
        },
        "freshness": { "fetched_at": 1_750_000_000, "stale_after": 1_750_000_600 }
    });
    let dir = std::env::temp_dir().join(format!("aci-cli-old-shape-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let report_path = dir.join("old_report.json");
    std::fs::write(&report_path, serde_json::to_vec(&old_shape).unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_aci"))
        .args(["audit", "--report", report_path.to_str().unwrap()])
        .output()
        .expect("run aci audit");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("workload_keyset_b64"),
        "error must name the missing field: {stderr}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
