use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, Semaphore};
use tokio::time::Instant;

fn verifier(origin: &str) -> AciServiceUpstreamVerifier {
    AciServiceUpstreamVerifier::new_with_timeouts(
        origin,
        "http://127.0.0.1:1",
        AciServiceVerifierPolicy::new(
            vec!["test-subject".to_string()],
            Vec::new(),
            vec![public_key_uncompressed_hex(&signing_key(1))],
        )
        .unwrap(),
        300,
        1,
        60,
    )
    .unwrap()
}

fn request(origin: &str) -> UpstreamVerificationRequest {
    UpstreamVerificationRequest {
        upstream_name: "aci".to_string(),
        model_id: "model".to_string(),
        url_origin: Some(origin.to_string()),
        forwarded_body_hash: "22".repeat(32),
        required: true,
    }
}

async fn wait_until_wall(not_after: u64) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    tokio::time::sleep(Duration::from_secs(not_after).saturating_sub(now)).await;
}

struct Control {
    calls: AtomicUsize,
    entered: Notify,
    release: Semaphore,
    not_after: u64,
}

struct Fixture {
    verifier: Arc<AciServiceUpstreamVerifier>,
    request: UpstreamVerificationRequest,
    control: Arc<Control>,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new(not_after: u64) -> Self {
        use crate::aci::identity::{self, SealedWorkloadKeyset};
        use axum::{
            extract::{Query, State},
            routing::get,
            Json, Router,
        };
        async fn report(
            State(control): State<Arc<Control>>,
            Query(query): Query<std::collections::HashMap<String, String>>,
        ) -> Json<AttestationReport> {
            control.calls.fetch_add(1, Ordering::SeqCst);
            control.entered.notify_one();
            control.release.acquire().await.unwrap().forget();
            let nonce = &query["nonce"];
            let mut keyset = keyset_with_tls(Vec::new());
            keyset.not_after = control.not_after;
            let sealed = SealedWorkloadKeyset::seal(keyset).unwrap();
            let statement = identity::attestation_statement(sealed.digest(), Some(nonce)).unwrap();
            Json(AttestationReport {
                api_version: "aci/1".to_string(),
                workload_keyset_digest: sealed.digest().to_string(),
                attestation: AttestationEnvelope {
                    tee_type: "tdx".to_string(),
                    workload_keyset: sealed.to_value(),
                    report_data_hex: hex::encode(identity::report_data(&statement)),
                    source_provenance: SourceProvenance::default(),
                    evidence: json!({}),
                },
                service_capabilities: Default::default(),
            })
        }
        // Mock cryptographic appraisal only; public methods still fetch, bind the nonce,
        // serialize verification, check completion-time expiry, and cache the response.
        fn appraise(report: &AttestationReport, nonce: &str) -> super::super::appraisal::Appraisal {
            super::super::appraisal::Appraisal {
                results: Vec::new(),
                channel_bindings: Vec::new(),
                identity: Some(
                    aci_verify::report::verify_report_binding(report, Some(nonce)).unwrap(),
                ),
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let control = Arc::new(Control {
            calls: AtomicUsize::new(0),
            entered: Notify::new(),
            release: Semaphore::new(0),
            not_after,
        });
        let app = Router::new()
            .route("/v1/aci/attestation", get(report))
            .with_state(control.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            verifier: Arc::new(verifier(&origin).with_appraisal(appraise)),
            request: request(&origin),
            control,
            server,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[tokio::test]
async fn public_cold_verification_is_single_flight_and_refresh_shares_the_lock() {
    let fixture = Fixture::new(u64::MAX).await;
    let initial = {
        let mut calls = Box::pin(futures_util::future::join_all(
            (0..10).map(|_| fixture.verifier.verify(fixture.request.clone())),
        ));
        tokio::select! {
            _ = fixture.control.entered.notified() => {},
            _ = &mut calls => panic!("verification must await the upstream response"),
        }
        fixture.control.release.add_permits(1);
        calls.await
    };
    assert!(initial
        .iter()
        .all(|event| event.result == VerificationResult::Verified));
    assert_eq!(fixture.control.calls.load(Ordering::SeqCst), 1);
    fixture.verifier.invalidate(&fixture.request);
    let verifier = fixture.verifier.clone();
    let request = fixture.request.clone();
    let refresh = tokio::spawn(async move { verifier.refresh(request).await });
    fixture.control.entered.notified().await;
    let mut cold = Box::pin(fixture.verifier.verify(fixture.request.clone()));
    assert!(futures_util::poll!(&mut cold).is_pending());
    assert_eq!(fixture.control.calls.load(Ordering::SeqCst), 2);
    fixture.control.release.add_permits(1);
    let (refreshed, verified) = tokio::join!(refresh, cold);
    let refreshed = refreshed.unwrap();
    assert_eq!(refreshed.result, VerificationResult::Verified);
    assert_eq!(verified.result, VerificationResult::Verified);
    assert_eq!(verified.evidence, refreshed.evidence);
    assert_eq!(fixture.control.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn public_verification_rejects_a_keyset_expiring_before_the_response() {
    let not_after = current_unix_secs() + 2;
    let fixture = Fixture::new(not_after).await;
    let verifier = fixture.verifier.clone();
    let request = fixture.request.clone();
    let pending = tokio::spawn(async move { verifier.verify(request).await });
    fixture.control.entered.notified().await;
    assert!(current_unix_secs() < not_after);
    wait_until_wall(not_after).await;
    fixture.control.release.add_permits(1);
    let result = pending.await.unwrap();
    assert_eq!(result.result, VerificationResult::Failed);
    assert!(result.reason.unwrap().contains("expired"));
    assert!(fixture.verifier.cached(&fixture.request).is_none());
}

#[tokio::test]
async fn absolute_keyset_expiry_invalidates_an_unexpired_monotonic_cache() {
    let origin = "http://127.0.0.1:1";
    let deadline = Instant::now() + Duration::from_secs(300);
    let not_after = current_unix_secs() + 2;
    let verifier = verifier(origin).with_cached(CachedAciServiceVerification {
        expires_at: deadline,
        not_after,
        evidence: None,
        channel_bindings: Vec::new(),
    });
    let request = request(origin);
    assert_eq!(
        verifier.verify(request.clone()).await.result,
        VerificationResult::Verified
    );
    assert!(verifier.cache_remaining(&request).unwrap() <= Duration::from_secs(2));
    wait_until_wall(not_after).await;
    assert!(Instant::now() < deadline);
    assert!(verifier.cached(&request).is_none());
    assert!(verifier.cache_remaining(&request).is_none());
}
