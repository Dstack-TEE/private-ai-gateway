mod common;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use private_ai_gateway::aci::receipt::{ChannelBinding, VerificationResult};
use private_ai_gateway::aci::upstream::{
    UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
};
use private_ai_gateway::aci::verifier::validate_aci_report_binding;
use private_ai_gateway::aggregator::service::{
    AciService, AciServiceConfig, FixedClock, InMemoryReceiptStore,
};

use common::{StaticKeyProvider, StubQuoter};

struct NoopUpstream;

#[async_trait]
impl UpstreamBackend for NoopUpstream {
    fn name(&self) -> &str {
        "noop-upstream"
    }

    fn url_origin(&self) -> Option<&str> {
        Some("https://noop-upstream.example")
    }

    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        Ok(UpstreamResponse {
            status_code: 200,
            body: b"{}".to_vec(),
            headers: HashMap::new(),
        })
    }
}

fn service() -> Arc<AciService> {
    let keys = Arc::new(StaticKeyProvider::default());
    let quoter = Arc::new(StubQuoter::default());
    let upstream = Arc::new(NoopUpstream);
    let store = Arc::new(InMemoryReceiptStore::default());
    Arc::new(
        AciService::new(
            keys,
            quoter,
            upstream,
            store,
            AciServiceConfig::for_test("upstream-verifier-test"),
            Arc::new(FixedClock(1_000)),
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn aci_report_binding_validation_accepts_self_consistent_report() {
    let report = service()
        .attestation_report(Some("verifier nonce".to_string()))
        .await
        .unwrap();
    let validated = validate_aci_report_binding(
        &report,
        Some("verifier nonce"),
        1_000,
        Some(br#"{"raw":"report"}"#),
    )
    .unwrap();

    assert_eq!(validated.workload_id, report.workload_id);
    assert_eq!(
        validated.workload_keyset_digest,
        report.workload_keyset_digest
    );
    assert!(validated.evidence.is_some());
}

#[tokio::test]
async fn aci_report_binding_validation_rejects_wrong_nonce() {
    let report = service()
        .attestation_report(Some("verifier nonce".to_string()))
        .await
        .unwrap();
    let err = validate_aci_report_binding(&report, Some("other nonce"), 1_000, None)
        .unwrap_err()
        .to_string();

    assert_eq!(err, "report_data mismatch");
}

#[tokio::test]
async fn aci_report_binding_validation_rejects_bad_keyset_endorsement() {
    let mut report = service()
        .attestation_report(Some("verifier nonce".to_string()))
        .await
        .unwrap();
    report.attestation.keyset_endorsement.value_hex = "00".to_string();

    let err = validate_aci_report_binding(&report, Some("verifier nonce"), 1_000, None)
        .unwrap_err()
        .to_string();

    assert_eq!(err, "keyset_endorsement signature verification failed");
}

#[tokio::test]
async fn service_fails_if_selected_backend_cannot_enforce_channel_binding() {
    let event = private_ai_gateway::aci::receipt::UpstreamVerifiedEvent {
        vendor: "noop-upstream".to_string(),
        provider: None,
        model_id: "model-a".to_string(),
        url_origin: Some("https://noop-upstream.example".to_string()),
        verifier_id: "fixture-verifier/v1".to_string(),
        result: VerificationResult::Verified,
        required: true,
        reason: None,
        evidence: None,
        channel_bindings: vec![ChannelBinding::TlsSpkiSha256 {
            origin: "https://noop-upstream.example".to_string(),
            spki_sha256: "aa".repeat(32),
        }],
        provider_claims: None,
    };
    let err = service()
        .forward_chat_completion(
            br#"{"model":"model-a","messages":[]}"#,
            None,
            Some(true),
            Some(event),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("cannot enforce upstream channel bindings"));
}
