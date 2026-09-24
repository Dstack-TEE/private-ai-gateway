use super::audit::verify_exchange;
use super::control::json_response;
use super::managed::{managed_reporter, InProcessVerifierLauncher};
use super::report::{request_outcome_event, summarize};
use super::*;
use crate::checks::{parse_receipt_document, run_response_checks, UpstreamContext};
use crate::client::host_of;
use crate::spec_fixtures::{
    vector_receipt_envelope, vector_report, vector_session_bytes, REQUEST_BODY, RESPONSE_BODY,
};
use crate::transcript::Transcript;
use agent_bridge::proxy::ProxyEvent;
use axum::routing::{get, post};
use axum::Json;
use std::sync::atomic::AtomicUsize;
use tokio::sync::mpsc;

/// The one-line summary over the self-consistent fixtures, without any
/// network: signature, wire hash, and the asserted upstream claim.
#[test]
fn summary_over_fixtures_reads_all_ok() {
    let report = vector_report();
    let identity = crate::checks::established_identity(&report).unwrap();
    let receipt = parse_receipt_document(vector_receipt_envelope()).unwrap();
    let session_bytes = vector_session_bytes();
    let mut transcript = Transcript::default();
    run_response_checks(
        &mut transcript,
        &receipt,
        &identity,
        Some(&BodyDigest::of(REQUEST_BODY)),
        Some(&BodyDigest::of(RESPONSE_BODY)),
        UpstreamContext {
            session_bytes: Some(&session_bytes),
            no_session_reason: "unused",
            pinned: None,
            requires_verified: true,
            serving: "aggregator",
            required_claims: &[],
        },
    );

    assert!(transcript.verified());
    let session: Value = serde_json::from_slice(&session_bytes).unwrap();
    let summary = summarize(&transcript, Some(&session), "upstream");
    assert_eq!(
        summary,
        "signature ok, wire hash ok, upstream tee_attested asserted (hardware_proven)"
    );
}

/// A request that passed the entry checks but has not started sending is
/// refused with zero delivery when the identity is lost in between (as a
/// concurrent response's rotation gate would do).
#[tokio::test]
async fn blocked_after_entry_checks_delivers_nothing() {
    let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = delivered.clone();
    let upstream = Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Json(json!({ "ok": true })) }
        }),
    );
    let base = spawn_server(upstream).await;
    let (tx, _rx) = mpsc::unbounded_channel();
    let state = state_over(base, tx);
    let reached = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    *state.pause.lock().unwrap() = Some((reached.clone(), resume.clone()));
    let proxy = spawn_server(build_proxy_router(state.clone())).await;
    let request = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("{proxy}/v1/chat/completions"))
            .header("content-type", "application/json")
            .body(r#"{"model":"m","messages":[]}"#)
            .send()
            .await
            .unwrap()
    });
    reached.notified().await;
    state.blocked.store(true, Ordering::SeqCst);
    state.revoke_deliveries();
    resume.notify_one();
    let response = request.await.unwrap();
    assert_eq!(response.status().as_u16(), 503);
    assert_eq!(delivered.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// Stopping an in-process verifier cancels the same delivery gate used for
/// identity loss, so admitted requests cannot outlive their owning task.
#[tokio::test]
async fn managed_shutdown_revokes_request_before_send() {
    let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = delivered.clone();
    let upstream = Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Json(json!({ "ok": true })) }
        }),
    );
    let base = spawn_server(upstream).await;
    let (tx, _rx) = mpsc::unbounded_channel();
    let state = state_over(base, tx);
    let reached = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    *state.pause.lock().unwrap() = Some((reached.clone(), resume.clone()));
    let proxy = spawn_server(build_proxy_router(state.clone())).await;
    let request = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("{proxy}/v1/chat/completions"))
            .header("content-type", "application/json")
            .body(r#"{"model":"m","messages":[]}"#)
            .send()
            .await
            .unwrap()
    });
    reached.notified().await;
    state.shutdown.cancel();
    resume.notify_one();
    let response = request.await.unwrap();
    assert_eq!(response.status().as_u16(), 503);
    assert_eq!(delivered.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn managed_shutdown_interrupts_passthrough_response() {
    let resume = Arc::new(tokio::sync::Notify::new());
    let upstream_resume = resume.clone();
    let upstream = Router::new().route(
        "/v1/models",
        get(move || {
            let resume = upstream_resume.clone();
            async move {
                let body = async_stream::stream! {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(b"first"));
                    resume.notified().await;
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(b"second"));
                };
                Response::builder()
                    .body(Body::from_stream(body))
                    .expect("streaming response is valid")
            }
        }),
    );
    let base = spawn_server(upstream).await;
    let (tx, _rx) = mpsc::unbounded_channel();
    let state = state_over(base, tx);
    let proxy = spawn_server(build_proxy_router(state.clone())).await;
    let response = reqwest::Client::new()
        .get(format!("{proxy}/v1/models"))
        .send()
        .await
        .unwrap();
    let mut body = response.bytes_stream();
    assert_eq!(body.next().await.unwrap().unwrap(), "first");

    state.shutdown.cancel();
    resume.notify_waiters();
    let interrupted = tokio::time::timeout(std::time::Duration::from_secs(1), body.next())
        .await
        .expect("managed shutdown should interrupt the response")
        .expect("the interrupted response should report an error");
    assert!(interrupted.is_err());
}

/// While blocked, every method is refused until re-verification succeeds;
/// here the upstream offers no attestation, so it cannot.
#[tokio::test]
async fn non_post_requests_are_refused_while_blocked() {
    let upstream = Router::new().route(
        "/v1/models",
        get(|| async { Json(json!({ "data": [{ "id": "demo-model" }] })) }),
    );
    let base = spawn_server(upstream).await;
    let (tx, _rx) = mpsc::unbounded_channel();
    let state = state_over(base, tx);
    state.blocked.store(true, Ordering::SeqCst);
    let proxy = spawn_server(build_proxy_router(state)).await;
    let resp = reqwest::Client::new()
        .get(format!("{proxy}/v1/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 503);
}

#[test]
fn connection_named_headers_are_stripped_in_both_directions() {
    let mut headers = HeaderMap::new();
    headers.insert("connection", "close, X-Private".parse().unwrap());
    headers.insert("x-private", "1".parse().unwrap());
    headers.insert("proxy-connection", "keep-alive".parse().unwrap());
    headers.insert("content-length", "1".parse().unwrap());
    let dropped = dropped_headers(&headers);
    assert!(dropped.contains("x-private"));
    assert!(dropped.contains("proxy-connection"));
    assert!(dropped.contains("content-length"));
    assert!(!dropped.contains("x-receipt-id"));
}

#[test]
fn request_outcome_event_is_stable_json() {
    let outcome = RequestOutcome {
        method: Method::POST,
        path: "/v1/messages".to_string(),
        status: 200,
        streamed: true,
        receipt_id: Some("rcpt-1".to_string()),
        verified: Some(true),
        detail: "receipt verified".to_string(),
        context: None,
        rewritten: Some(false),
        local_policy_applied: true,
    };
    let event = serde_json::to_value(request_outcome_event(outcome)).unwrap();
    assert_eq!(event["local_policy_applied"], true);

    assert_eq!(event["type"], "request_complete");
    assert_eq!(event["method"], "POST");
    assert_eq!(event["receipt_id"], "rcpt-1");
    assert_eq!(event["verified"], true);
}

#[tokio::test]
async fn keyset_change_and_verification_failure_have_distinct_events() {
    let upstream =
        spawn_server(Router::new().fallback(|| async { StatusCode::SERVICE_UNAVAILABLE })).await;
    let (outcomes, _) = mpsc::unbounded_channel();
    let mut state = state_over(upstream, outcomes);
    let (events, mut received) = mpsc::unbounded_channel();
    Arc::get_mut(&mut state).unwrap().event_sink = Arc::new(move |event| {
        let _ = events.send(event);
    });
    let previous = state.delivery.lock().unwrap().clone();
    let mut headers = HeaderMap::new();
    headers.insert("x-aci-keyset-digest", "new-keyset".parse().unwrap());
    rotation_gate(&state, "old-keyset", &headers);
    assert!(previous.is_cancelled());
    assert!(matches!(
        received.recv().await.unwrap(),
        VerifierEvent::Blocked { code: Some(code), .. } if code == "keyset_changed"
    ));
    let proxy = spawn_server(build_proxy_router(state)).await;
    let response = reqwest::Client::new()
        .get(format!("{proxy}/v1/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(matches!(
        received.recv().await.unwrap(),
        VerifierEvent::Blocked { code: None, .. }
    ));
}

#[test]
fn identity_event_carries_the_verified_workload_summary() {
    let report = vector_report();
    let identity = crate::checks::established_identity(&report).unwrap();
    let event = lifecycle_json(
        "ready",
        identity_event(
            &report,
            &identity,
            Some("sha256:observed"),
            json!({ "checks": [] }),
        ),
        Some(json!({
            "remote_url": "https://tee.example",
            "proxy_url": "http://127.0.0.1:4181",
            "control_url": "http://127.0.0.1:4182",
            "policy": {},
        })),
    );

    assert_eq!(event["type"], "ready");
    assert_eq!(event["tee_type"], "tdx");
    assert_eq!(event["keyset_digest"], report.workload_keyset_digest);
    assert_eq!(event["tls_spki"], "sha256:observed");
    assert_eq!(event["control_url"], "http://127.0.0.1:4182");
}

fn state_over(base_url: String, tx: mpsc::UnboundedSender<RequestOutcome>) -> Arc<ProxyState> {
    let host = host_of(&base_url).unwrap();
    // Byte-exact passthrough harness: enforcement off so fixture-pinned
    // request hashes hold; `apply_constraints` has its own unit test.
    Arc::new(ProxyState::new(
        AciClient::new().unwrap(),
        base_url,
        host,
        false,
        Vec::new(),
        false,
        Vec::new(),
        Vec::new(),
        vector_report(),
        crate::checks::established_identity(&vector_report()).unwrap(),
        Arc::new(move |outcome| {
            let _ = tx.send(outcome);
        }),
        Arc::new(|_| {}),
        tokio_util::sync::CancellationToken::new(),
    ))
}

#[tokio::test]
async fn client_cancelled_stream_is_not_a_failed_proof() {
    let (tx, mut outcomes) = mpsc::unbounded_channel();
    let state = state_over("http://127.0.0.1:9".to_string(), tx);
    let exchange = RecordedExchange {
        receipt_id: "rcpt-cancelled".to_string(),
        path: "/v1/responses".to_string(),
        status: 200,
        streamed: true,
        request: BodyDigest::of(REQUEST_BODY),
        response: BodyDigest::of(b"data: partial\n\n"),
        delivery: ResponseDelivery::Cancelled,
        pinned_sessions: Vec::new(),
        at: 1,
        verified: None,
        context: None,
        local_policy_applied: false,
    };
    let trusted = state.snapshot();

    audit_exchange(state, trusted, exchange, None);

    let outcome = outcomes.recv().await.expect("cancellation outcome");
    assert_eq!(outcome.verified, None);
    assert!(outcome.detail.contains("canceled or protection stopped"));
}

#[tokio::test]
async fn standalone_control_lists_and_retries_recorded_exchanges() {
    let (tx, _outcomes) = mpsc::unbounded_channel();
    let state = state_over("http://127.0.0.1:9".to_string(), tx);
    state.record(RecordedExchange {
        receipt_id: "rcpt-control".to_string(),
        path: "/v1/responses".to_string(),
        status: 200,
        streamed: true,
        request: BodyDigest::of(REQUEST_BODY),
        response: BodyDigest::of(b"partial"),
        delivery: ResponseDelivery::Cancelled,
        pinned_sessions: Vec::new(),
        at: 17,
        verified: None,
        context: None,
        local_policy_applied: false,
    });
    let control = spawn_server(build_control_router(state)).await;

    let listed: Value = reqwest::get(format!("{control}/receipts"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed[0]["receipt_id"], "rcpt-control");
    assert_eq!(listed[0]["cancelled"], true);

    let retried = reqwest::Client::new()
        .post(format!("{control}/receipts/rcpt-control/verify"))
        .send()
        .await
        .unwrap();
    assert_eq!(retried.status(), StatusCode::BAD_GATEWAY);
    let body: Value = retried.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("canceled"));
}

#[tokio::test]
async fn transient_receipt_and_session_fetches_are_retried() {
    let receipt_calls = Arc::new(AtomicUsize::new(0));
    let session_calls = Arc::new(AtomicUsize::new(0));
    let upstream = Router::new()
        .route(
            "/v1/aci/receipts/{id}",
            get({
                let calls = receipt_calls.clone();
                move || {
                    let calls = calls.clone();
                    async move {
                        if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                            return text_response(StatusCode::NOT_FOUND, "not ready");
                        }
                        json_response(StatusCode::OK, vector_receipt_envelope())
                    }
                }
            }),
        )
        .route(
            "/v1/aci/sessions/{id}",
            get({
                let calls = session_calls.clone();
                move || {
                    let calls = calls.clone();
                    async move {
                        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                            return text_response(StatusCode::SERVICE_UNAVAILABLE, "not ready");
                        }
                        Response::builder()
                            .header("content-type", "application/json")
                            .body(Body::from(vector_session_bytes()))
                            .unwrap()
                    }
                }
            }),
        );
    let base = spawn_server(upstream).await;
    let (tx, _outcomes) = mpsc::unbounded_channel();
    let state = state_over(base, tx);
    let exchange = RecordedExchange {
        receipt_id: "rcpt-0001".to_string(),
        path: "/v1/chat/completions".to_string(),
        status: 200,
        streamed: true,
        request: BodyDigest::of(REQUEST_BODY),
        response: BodyDigest::of(RESPONSE_BODY),
        delivery: ResponseDelivery::Complete,
        pinned_sessions: Vec::new(),
        at: 1,
        verified: None,
        context: None,
        local_policy_applied: false,
    };

    let (transcript, _) = verify_exchange(&state, &state.snapshot(), &exchange, None)
        .await
        .unwrap();

    assert!(transcript.verified());
    assert_eq!(receipt_calls.load(Ordering::SeqCst), 3);
    assert_eq!(session_calls.load(Ordering::SeqCst), 2);
}

#[test]
fn apply_constraints_tightens_plaintext_body() {
    // Plain body: the member is added.
    let out = apply_constraints(br#"{"model":"m","messages":[]}"#.to_vec(), true, &[]).unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["provider"]["aci_verified"], true);
    assert_eq!(v["provider"].get("aci_session_ids"), None);

    // Existing routing members survive; an explicit `false` is tightened.
    let out = apply_constraints(
        br#"{"model":"m","provider":{"order":["x"],"aci_verified":false}}"#.to_vec(),
        true,
        &[],
    )
    .unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["provider"]["aci_verified"], true);
    assert_eq!(v["provider"]["order"][0], "x");

    // A pin set is injected — and implies verified serving.
    let pins = vec!["a".repeat(64)];
    let out = apply_constraints(br#"{"model":"m"}"#.to_vec(), false, &pins).unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["provider"]["aci_session_ids"][0], pins[0]);
    assert_eq!(v["provider"]["aci_verified"], true);

    // The client's own narrower set survives when local policy accepts it.
    let out = apply_constraints(
        format!(r#"{{"provider":{{"aci_session_ids":["{}"]}}}}"#, pins[0]).into_bytes(),
        true,
        &pins,
    )
    .unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["provider"]["aci_session_ids"], json!(pins));

    // Disjoint client and local policies fail before network access.
    let disjoint = json!({
        "provider": { "aci_session_ids": ["b".repeat(64)] }
    });
    assert!(apply_constraints(serde_json::to_vec(&disjoint).unwrap(), true, &pins,).is_err());

    // Non-JSON bodies pass through untouched.
    assert_eq!(
        apply_constraints(b"not json".to_vec(), true, &[]).unwrap(),
        b"not json"
    );
}

#[tokio::test]
async fn proxy_rejects_e2ee_request_headers_without_contacting_upstream() {
    let upstream_calls = Arc::new(AtomicUsize::new(0));
    let counted_calls = upstream_calls.clone();
    let upstream = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let counted_calls = counted_calls.clone();
            async move {
                counted_calls.fetch_add(1, Ordering::SeqCst);
                StatusCode::NO_CONTENT
            }
        }),
    );
    let base = spawn_server(upstream).await;

    let (tx, _rx) = mpsc::unbounded_channel();
    let proxy = spawn_server(build_proxy_router(state_over(base, tx))).await;
    let http = reqwest::Client::new();

    for header in E2EE_REQUEST_HEADERS {
        let resp = http
            .post(format!("{proxy}/v1/chat/completions"))
            .header(*header, "2")
            .header("content-type", "application/json")
            .body(REQUEST_BODY.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 400, "header {header}");
        assert!(
            resp.text()
                .await
                .unwrap()
                .contains("accepts plaintext requests only"),
            "header {header}"
        );
    }

    assert_eq!(upstream_calls.load(Ordering::SeqCst), 0);
}

/// Policy-pinned proxy against rotated sessions: the stale pin is
/// refused 412, the proxy re-derives the accepted set from the current
/// sessions (spec 9.2 audit + claims policy) and retries once.
#[tokio::test]
async fn a_412_refusal_refreshes_policy_pins_and_retries() {
    use axum::response::IntoResponse;

    // A currently-valid session: the fixture record with its validity
    // window moved to now (the id is content-addressed, so it changes).
    let mut record: Value = serde_json::from_slice(&vector_session_bytes()).unwrap();
    let now = desktop_core::now_secs();
    record["established_at"] = json!(now - 10);
    record["expires_at"] = json!(now + 3600);
    let session_bytes = crate::aci::digest::jcs_bytes(&record).unwrap();
    let current_id = crate::aci::digest::sha256_bare_hex(&session_bytes);
    let keyset_digest = vector_report().workload_keyset_digest;

    let sid = current_id.clone();
    let upstream = Router::new()
        .route(
            "/v1/chat/completions",
            post(move |body: Bytes| {
                let sid = sid.clone();
                let keyset_digest = keyset_digest.clone();
                async move {
                    let v: Value = serde_json::from_slice(&body).unwrap();
                    let pinned_current = v["provider"]["aci_session_ids"]
                        .as_array()
                        .is_some_and(|pins| pins.iter().any(|pin| pin == &json!(sid)));
                    if pinned_current {
                        (
                            StatusCode::OK,
                            [
                                ("x-receipt-id", "rcpt-0002".to_string()),
                                ("x-aci-keyset-digest", keyset_digest),
                            ],
                            RESPONSE_BODY,
                        )
                            .into_response()
                    } else {
                        (StatusCode::PRECONDITION_FAILED, "session_not_accepted").into_response()
                    }
                }
            }),
        )
        .route(
            "/v1/aci/sessions",
            get({
                let sid = current_id.clone();
                move || {
                    let sid = sid.clone();
                    async move {
                        Json(json!({
                            "api_version": "aci/1",
                            "sessions": [{ "session_id": sid }],
                        }))
                    }
                }
            }),
        )
        .route(
            "/v1/aci/sessions/{id}",
            get({
                let session_bytes = session_bytes.clone();
                move || {
                    let session_bytes = session_bytes.clone();
                    async move { ([("content-type", "application/json")], session_bytes) }
                }
            }),
        );
    let base = spawn_server(upstream).await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let host = host_of(&base).unwrap();
    let state = Arc::new(ProxyState::new(
        AciClient::new().unwrap(),
        base.clone(),
        host,
        true,
        Vec::new(),
        false,
        Vec::new(),
        vec![crate::checks::RequiredClaim::parse("tee_attested").unwrap()],
        vector_report(),
        crate::checks::established_identity(&vector_report()).unwrap(),
        Arc::new(move |outcome| {
            let _ = tx.send(outcome);
        }),
        Arc::new(|_| {}),
        tokio_util::sync::CancellationToken::new(),
    ));
    // A stale pin, as if the pinned session was superseded after startup.
    *state.policy_pins.lock().unwrap() = vec!["f".repeat(64)];
    let proxy = spawn_server(build_proxy_router(state.clone())).await;

    let resp = reqwest::Client::new()
        .post(format!("{proxy}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(br#"{"model":"m","messages":[]}"#.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.bytes().await.unwrap().as_ref(), RESPONSE_BODY);

    let outcome = rx.recv().await.expect("retried outcome reported");
    assert_eq!(outcome.status, 200);
    assert_eq!(outcome.verified, None);
    assert!(
        outcome.detail.contains("audit pending"),
        "{}",
        outcome.detail
    );
    // The refreshed set replaced the stale pin.
    assert_eq!(*state.policy_pins.lock().unwrap(), vec![current_id]);
}

async fn spawn_server(app: Router) -> String {
    crate::install_crypto_provider();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// Hermetic end-to-end: a mock upstream serving fixture artifacts, the
/// proxy in front of it. Asserts byte-exact passthrough, that any POST
/// path gets its exchange verified with the request's transient bearer
/// (an Anthropic-style `/v1/messages` included), and that a receiptless
/// 2xx POST fails loudly.
#[tokio::test]
async fn proxy_forwards_and_verifies_receipt() {
    let inference = || {
        let keyset_digest = vector_report().workload_keyset_digest;
        post(move || async move {
            (
                [
                    ("content-type", "application/json".to_string()),
                    ("x-receipt-id", "rcpt-0001".to_string()),
                    ("x-aci-keyset-digest", keyset_digest),
                ],
                RESPONSE_BODY,
            )
        })
    };
    let upstream = Router::new()
        .route("/v1/chat/completions", inference())
        .route("/v1/messages", inference())
        .route(
            "/v1/responses",
            post(|| async { Json(json!({ "ok": true })) }),
        )
        .route(
            "/v1/aci/receipts/{id}",
            get(|headers: HeaderMap| async move {
                assert_eq!(
                    header_str(&headers, "authorization"),
                    Some("Bearer test-key")
                );
                Json(vector_receipt_envelope())
            }),
        )
        .route(
            // Sessions are served as their exact sealed bytes (§8).
            "/v1/aci/sessions/{id}",
            get(|| async {
                (
                    [("content-type", "application/json")],
                    vector_session_bytes(),
                )
            }),
        )
        .route(
            "/v1/models",
            get(|| async { Json(json!({ "data": [{ "id": "demo-model" }] })) }),
        );
    let base = spawn_server(upstream).await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let state = state_over(base.clone(), tx);
    let proxy = spawn_server(build_proxy_router(state.clone())).await;
    drop(state);

    let http = reqwest::Client::new();

    // Inference forward: byte-exact passthrough + receipt header surfaced;
    // the exchange is verified after the response completes.
    let resp = http
        .post(format!("{proxy}/v1/chat/completions"))
        .header("content-type", "application/json")
        .bearer_auth("test-key")
        .body(REQUEST_BODY.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers()
            .get("x-receipt-id")
            .unwrap()
            .to_str()
            .unwrap(),
        "rcpt-0001"
    );
    assert_eq!(resp.bytes().await.unwrap().as_ref(), RESPONSE_BODY);

    let outcome = rx.recv().await.expect("inference outcome reported");
    assert_eq!(outcome.method, "POST");
    assert_eq!(outcome.path, "/v1/chat/completions");
    assert_eq!(outcome.receipt_id.as_deref(), Some("rcpt-0001"));
    assert_eq!(outcome.verified, None);
    let audited = rx.recv().await.expect("inference audit reported");
    assert_eq!(audited.path, "/v1/chat/completions");
    assert_eq!(audited.verified, Some(true));

    // Any POST path is inference-capable: an Anthropic-style /v1/messages
    // forward is recorded the same way without being enumerated.
    let resp = http
        .post(format!("{proxy}/v1/messages"))
        .header("content-type", "application/json")
        .bearer_auth("test-key")
        .body(REQUEST_BODY.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.bytes().await.unwrap().as_ref(), RESPONSE_BODY);
    let outcome = rx.recv().await.expect("messages outcome reported");
    assert_eq!(outcome.path, "/v1/messages");
    assert_eq!(outcome.verified, None);
    let audited = rx.recv().await.expect("messages audit reported");
    assert_eq!(audited.path, "/v1/messages");
    assert_eq!(audited.verified, Some(true));

    // A 2xx POST response with no receipt header fails loudly (spec 5.2).
    let resp = http
        .post(format!("{proxy}/v1/responses"))
        .header("content-type", "application/json")
        .body(REQUEST_BODY.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert!(resp.text().await.unwrap().contains("\"ok\":true"));
    let outcome = rx.recv().await.expect("responses outcome reported");
    assert_eq!(outcome.verified, Some(false));
    assert!(
        outcome.detail.contains("no X-Receipt-Id"),
        "{}",
        outcome.detail
    );

    // GET passthrough routes and reports without a receipt check.
    let models = http.get(format!("{proxy}/v1/models")).send().await.unwrap();
    assert_eq!(models.status().as_u16(), 200);
    let models_outcome = rx.recv().await.expect("models outcome reported");
    assert_eq!(models_outcome.method, Method::GET);
    assert_eq!(models_outcome.verified, None);
}

#[tokio::test]
async fn client_drop_does_not_cancel_background_receipt_audit() {
    let split = RESPONSE_BODY.len() / 2;
    let finish_stream = Arc::new(tokio::sync::Notify::new());
    let upstream = Router::new()
        .route("/v1/chat/completions", post({
            let finish = finish_stream.clone();
            move || {
                let finish = finish.clone();
                async move {
                    let body = Body::from_stream(async_stream::stream! {
                        yield Ok::<_, std::io::Error>(Bytes::from_static(&RESPONSE_BODY[..split]));
                        finish.notified().await;
                        yield Ok::<_, std::io::Error>(Bytes::from_static(&RESPONSE_BODY[split..]));
                    });
                    Response::builder()
                        .header("content-type", "text/event-stream")
                        .header("x-receipt-id", "rcpt-0001")
                        .body(body)
                        .unwrap()
                }
            }
        }))
        .route(
            "/v1/aci/receipts/{id}",
            get(|| async { json_response(StatusCode::OK, vector_receipt_envelope()) }),
        )
        .route("/v1/aci/sessions/{id}", get(|| async {
            ([(("content-type"), "application/json")], vector_session_bytes())
        }));
    let (tx, mut outcomes) = mpsc::unbounded_channel();
    let state = state_over(spawn_server(upstream).await, tx);
    let proxy = spawn_server(build_proxy_router(state)).await;
    let response = reqwest::Client::new()
        .post(format!("{proxy}/v1/chat/completions"))
        .body(REQUEST_BODY.to_vec())
        .send()
        .await
        .unwrap();
    let mut body = response.bytes_stream();
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), body.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first.as_ref(), &RESPONSE_BODY[..split]);
    drop(body);

    finish_stream.notify_one();
    let pending = tokio::time::timeout(std::time::Duration::from_secs(2), outcomes.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.verified, None);
    let audited = tokio::time::timeout(std::time::Duration::from_secs(5), outcomes.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(audited.verified, Some(true));
}

#[tokio::test]
async fn streaming_audit_does_not_gate_delivery_on_receipt_success() {
    use futures_util::StreamExt;
    for mode in ["valid", "tampered", "unavailable"] {
        let response_bytes: &'static [u8] = if mode == "valid" {
            RESPONSE_BODY
        } else {
            b"data: first\n\ndata: [DONE]\n\n"
        };
        let split = response_bytes.len() / 2;
        let finish_stream = Arc::new(tokio::sync::Notify::new());
        let audit_started = Arc::new(tokio::sync::Notify::new());
        let finish_audit = Arc::new(tokio::sync::Notify::new());
        let upstream = Router::new()
            .route("/v1/chat/completions", post({
                let finish = finish_stream.clone();
                move || {
                    let finish = finish.clone();
                    async move {
                        let body = Body::from_stream(async_stream::stream! {
                            yield Ok::<_, std::io::Error>(Bytes::from_static(&response_bytes[..split]));
                            finish.notified().await;
                            yield Ok::<_, std::io::Error>(Bytes::from_static(&response_bytes[split..]));
                        });
                        Response::builder().header("content-type", "text/event-stream")
                            .header("x-receipt-id", "rcpt-0001").body(body).unwrap()
                    }
                }
            }))
            .route("/v1/aci/receipts/{id}", get({
                let started = audit_started.clone();
                let finish = finish_audit.clone();
                move || {
                    let started = started.clone();
                    let finish = finish.clone();
                    async move {
                        started.notify_one();
                        if mode == "unavailable" {
                            return text_response(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
                        }
                        finish.notified().await;
                        json_response(StatusCode::OK, vector_receipt_envelope())
                    }
                }
            }))
            .route("/v1/aci/sessions/{id}", get(|| async {
                ([("content-type", "application/json")], vector_session_bytes())
            }));
        let (tx, mut outcomes) = mpsc::unbounded_channel();
        let state = state_over(spawn_server(upstream).await, tx);
        let proxy = spawn_server(build_proxy_router(state)).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy}/v1/chat/completions"))
            .body(REQUEST_BODY.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut stream = response.bytes_stream();
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(first.as_ref(), &response_bytes[..split]);
        assert!(outcomes.try_recv().is_err());
        finish_stream.notify_one();
        let remaining = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let mut bytes = Vec::new();
            while let Some(chunk) = stream.next().await {
                bytes.extend_from_slice(&chunk.unwrap());
            }
            bytes
        })
        .await
        .unwrap();
        assert_eq!(remaining, &response_bytes[split..]);
        tokio::time::timeout(std::time::Duration::from_secs(2), audit_started.notified())
            .await
            .unwrap();
        assert_eq!(outcomes.recv().await.unwrap().verified, None);
        finish_audit.notify_one();
        let audited = tokio::time::timeout(std::time::Duration::from_secs(5), outcomes.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(audited.status, 200);
        assert_eq!(
            audited.verified,
            match mode {
                "valid" => Some(true),
                "tampered" => Some(false),
                _ => None,
            }
        );
    }
}

async fn terminated(receiver: &mut mpsc::UnboundedReceiver<VerifierEvent>) -> Option<String> {
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
            .await
            .expect("verifier termination event timed out")
            .expect("verifier event channel closed");
        if let VerifierEvent::Terminated { error } = event {
            return error;
        }
    }
}

fn test_launcher_events() -> (VerifierEventSink, mpsc::UnboundedReceiver<VerifierEvent>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (
        Arc::new(move |event| {
            let _ = sender.send(event);
        }),
        receiver,
    )
}

#[tokio::test]
async fn in_process_launcher_reports_panics_as_failures() {
    let launcher = InProcessVerifierLauncher::new(tokio::runtime::Handle::current());
    let (events, mut received) = test_launcher_events();
    let _task = launcher.spawn_task(events, |_| async move {
        panic!("synthetic verifier panic");
        #[allow(unreachable_code)]
        Ok(())
    });

    assert!(terminated(&mut received).await.is_some());
}

#[tokio::test]
async fn in_process_launcher_explicit_stop_is_clean() {
    let launcher = InProcessVerifierLauncher::new(tokio::runtime::Handle::current());
    let (events, mut received) = test_launcher_events();
    let mut task = launcher.spawn_task(events, |cancelled| async move {
        cancelled.cancelled().await;
        Ok(())
    });

    task.stop().unwrap();
    assert_eq!(terminated(&mut received).await, None);
}

#[tokio::test]
async fn dropping_in_process_launcher_task_cancels_it_cleanly() {
    let launcher = InProcessVerifierLauncher::new(tokio::runtime::Handle::current());
    let (events, mut received) = test_launcher_events();
    let task = launcher.spawn_task(events, |cancelled| async move {
        cancelled.cancelled().await;
        Ok(())
    });

    drop(task);
    assert_eq!(terminated(&mut received).await, None);
}

fn proxy_event(generation: u64, request_id: &str) -> ProxyEvent {
    ProxyEvent {
        generation,
        request_id: request_id.to_string(),
        session_id: "session-1".to_string(),
        agent: None,
        method: "POST".to_string(),
        path: "/v1/responses".to_string(),
        model: None,
        status: 200,
        streamed: false,
        receipt_id: None,
        verified: None,
        detail: String::new(),
        at: 1,
        local_policy_applied: None,
        rewritten: None,
        left_device: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        cost_usd: None,
    }
}

#[tokio::test]
async fn managed_verdict_waits_for_a_full_queue_and_keeps_attribution() {
    let (events, mut received) = mpsc::channel(1);
    events.send(proxy_event(0, "queue-filler")).await.unwrap();
    let reporter = managed_reporter(events);
    reporter(RequestOutcome {
        method: Method::POST,
        path: "/v1/responses".to_string(),
        status: 200,
        streamed: true,
        receipt_id: Some("rcpt-1".to_string()),
        verified: Some(false),
        detail: "receipt failed".to_string(),
        context: Some(ForwardContext {
            generation: 9,
            request_id: "request-9".to_string(),
            session_id: "session-9".to_string(),
            agent: "codex".to_string(),
            model: Some("test-model".to_string()),
            at: 42,
        }),
        rewritten: Some(false),
        local_policy_applied: true,
    });

    assert_eq!(received.recv().await.unwrap().request_id, "queue-filler");
    let verdict = tokio::time::timeout(std::time::Duration::from_secs(2), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(verdict.generation, 9);
    assert_eq!(verdict.request_id, "request-9");
    assert_eq!(verdict.session_id, "session-9");
    assert_eq!(verdict.agent.as_deref(), Some("codex"));
    assert_eq!(verdict.model.as_deref(), Some("test-model"));
    assert_eq!(verdict.verified, Some(false));
}

#[tokio::test]
async fn stopping_during_post_stream_does_not_report_a_failed_verdict() {
    let finish = Arc::new(tokio::sync::Notify::new());
    let upstream_finish = finish.clone();
    let upstream = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let finish = upstream_finish.clone();
            async move {
                let body = Body::from_stream(async_stream::stream! {
                    yield Ok::<_, std::io::Error>(Bytes::from_static(b"first"));
                    finish.notified().await;
                    yield Ok::<_, std::io::Error>(Bytes::from_static(b"second"));
                });
                Response::builder()
                    .header("x-receipt-id", "rcpt-stopped")
                    .body(body)
                    .unwrap()
            }
        }),
    );
    let (sender, mut outcomes) = mpsc::unbounded_channel();
    let state = state_over(spawn_server(upstream).await, sender);
    let proxy = spawn_server(build_proxy_router(state.clone())).await;
    let response = reqwest::Client::new()
        .post(format!("{proxy}/v1/chat/completions"))
        .body(REQUEST_BODY.to_vec())
        .send()
        .await
        .unwrap();
    let mut body = response.bytes_stream();
    assert_eq!(body.next().await.unwrap().unwrap(), "first");

    state.shutdown.cancel();
    finish.notify_waiters();
    let _ = body.next().await;
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), outcomes.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.receipt_id.as_deref(), Some("rcpt-stopped"));
    assert_eq!(outcome.verified, None);
    assert!(outcome.detail.contains("protection stopped"));
    assert!(outcomes.try_recv().is_err());
}

#[test]
fn default_control_port_is_clear_of_the_desktop_listeners() {
    let port = DEFAULT_CONTROL
        .parse::<std::net::SocketAddr>()
        .unwrap()
        .port();
    assert_ne!(port, desktop_core::account::CALLBACK_PORT);
    assert_ne!(port, desktop_core::config::WEB_UI_DEFAULT_PORT);
}
