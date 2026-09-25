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
use std::sync::atomic::{AtomicBool, AtomicUsize};
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
        VerifierPolicy::default(),
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
        VerifierPolicy::default(),
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

/// A self-signed key for the local TLS upstream below, generated per test
/// run. It protects nothing: the tests only need handshakes a pin can accept
/// or refuse.
struct TestKey {
    config: Arc<rustls::ServerConfig>,
    spki: String,
}

impl TestKey {
    fn generate() -> Self {
        crate::install_crypto_provider();
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der());
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert.der().clone()], key.into())
            .unwrap();
        Self {
            spki: crate::aci::tls::leaf_spki_sha256_hex(cert.der()).unwrap(),
            config: Arc::new(config),
        }
    }

    fn server_config(&self) -> Arc<rustls::ServerConfig> {
        self.config.clone()
    }

    fn spki(&self) -> String {
        self.spki.clone()
    }
}

/// The key the service rotated to: outside every pin set below.
static ROTATED_KEY: std::sync::LazyLock<TestKey> = std::sync::LazyLock::new(TestKey::generate);

/// The key a request was pinned to before the rotation.
static PINNED_KEY: std::sync::LazyLock<TestKey> = std::sync::LazyLock::new(TestKey::generate);

type TlsStream = rustls::StreamOwned<rustls::ServerConnection, std::net::TcpStream>;

/// A local TLS upstream: connection `n` presents `key_for(n)`; a handshake
/// the client completes is counted in `completed` and handed to `serve`.
struct TlsUpstream {
    base: String,
    completed: Arc<AtomicUsize>,
}

fn spawn_tls_upstream(
    key_for: impl Fn(usize) -> &'static TestKey + Send + 'static,
    serve: impl Fn(usize, TlsStream) + Send + Sync + 'static,
) -> TlsUpstream {
    crate::install_crypto_provider();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream = TlsUpstream {
        base: format!("https://{}", listener.local_addr().unwrap()),
        completed: Arc::new(AtomicUsize::new(0)),
    };
    let completed = upstream.completed.clone();
    let serve = Arc::new(serve);
    std::thread::spawn(move || {
        for (index, stream) in listener.incoming().enumerate() {
            let Ok(mut stream) = stream else { continue };
            let config = key_for(index).server_config();
            let (serve, completed) = (serve.clone(), completed.clone());
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
                let mut conn = rustls::ServerConnection::new(config).unwrap();
                while conn.is_handshaking() {
                    if conn.complete_io(&mut stream).is_err() {
                        return;
                    }
                }
                completed.fetch_add(1, Ordering::SeqCst);
                serve(index, rustls::StreamOwned::new(conn, stream));
            });
        }
    });
    upstream
}

/// Read one request head (the tests only answer body-less requests);
/// `false` once the client closed the connection.
fn read_request_head(stream: &mut TlsStream) -> bool {
    use std::io::Read;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && matches!(stream.read(&mut byte), Ok(1)) {
        head.push(byte[0]);
    }
    !head.is_empty()
}

/// Answer one request with a 200 that is no attestation report, so a
/// re-verification against this upstream always fails.
fn reply_ok(mut stream: TlsStream) {
    use std::io::Write;
    read_request_head(&mut stream);
    let _ =
        stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
    stream.conn.send_close_notify();
    let _ = stream.flush();
}

/// An upstream presenting only the rotated key, answering after `hold` so
/// other requests can arrive while a re-verification is in flight.
fn unverifiable_upstream(hold: std::time::Duration) -> TlsUpstream {
    spawn_tls_upstream(
        |_| &ROTATED_KEY,
        move |_, stream| {
            std::thread::sleep(hold);
            reply_ok(stream);
        },
    )
}

/// A re-pin drops pooled connections: a keep-alive connection set up under
/// the old pin set is never reused once its key is no longer pinned.
#[tokio::test]
async fn a_repin_drops_connections_made_under_the_old_pin() {
    use std::io::Write;
    let upstream = spawn_tls_upstream(
        |_| &PINNED_KEY,
        |_, mut stream| {
            while read_request_head(&mut stream) {
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
                let _ = stream.flush();
            }
        },
    );
    let client = AciClient::new().unwrap();
    let host = host_of(&upstream.base).unwrap();
    let url = format!("{}/v1/models", upstream.base);
    client.pin(&host, &[PINNED_KEY.spki()]).unwrap();
    for _ in 0..2 {
        assert_eq!(client.get(&url, None).await.unwrap().status, 200);
    }
    // Both requests rode one keep-alive connection.
    assert_eq!(upstream.completed.load(Ordering::SeqCst), 1);

    client.pin(&host, &[ROTATED_KEY.spki()]).unwrap();
    assert!(client.get(&url, None).await.is_err());
    assert_eq!(client.pin_rejections(&host), 1);
}

/// A base URL on a local port nothing listens on: connecting is refused.
fn refused_base(scheme: &str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    format!("{scheme}://{}", listener.local_addr().unwrap())
}

fn capture_events(state: &mut Arc<ProxyState>) -> mpsc::UnboundedReceiver<VerifierEvent> {
    let (events, received) = mpsc::unbounded_channel();
    Arc::get_mut(state).unwrap().event_sink = Arc::new(move |event| {
        let _ = events.send(event);
    });
    received
}

/// The codes of the `Blocked` events received so far, in order.
fn blocked_codes(received: &mut mpsc::UnboundedReceiver<VerifierEvent>) -> Vec<Option<String>> {
    std::iter::from_fn(|| received.try_recv().ok())
        .filter_map(|event| match event {
            VerifierEvent::Blocked { code, .. } => Some(code),
            _ => None,
        })
        .collect()
}

async fn wait_until(condition: impl Fn() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !condition() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("condition not reached");
}

fn keyset_changed() -> Option<String> {
    Some("keyset_changed".to_string())
}

/// A connect failure on a host with no registered pin keeps the 502: there
/// is no pin that could be stale.
#[tokio::test]
async fn a_connect_failure_without_a_pin_keeps_the_502() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let state = state_over(refused_base("http"), tx);
    assert!(state.client.pinned_spkis(&state.host).is_empty());

    let proxy = spawn_server(build_proxy_router(state)).await;
    let resp = reqwest::Client::new()
        .post(format!("{proxy}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(REQUEST_BODY.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 502);
    assert!(resp
        .text()
        .await
        .unwrap()
        .contains("upstream connection failed"));
}

/// Only a handshake the pin refused heals: a refused port on a pinned host
/// is an ordinary outage and keeps the plain 502 with no re-attestation.
#[tokio::test]
async fn a_refused_port_on_a_pinned_host_does_not_reverify() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut state = state_over(refused_base("https"), tx);
    let mut events = capture_events(&mut state);
    let pin = vec![PINNED_KEY.spki()];
    state.client.pin(&state.host, &pin).unwrap();

    let proxy = spawn_server(build_proxy_router(state.clone())).await;
    let resp = reqwest::Client::new()
        .get(format!("{proxy}/v1/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 502);
    assert!(!state.blocked.load(Ordering::SeqCst));
    assert_eq!(state.reverify_attempts.load(Ordering::SeqCst), 0);
    assert!(blocked_codes(&mut events).is_empty());
    assert_eq!(state.client.pinned_spkis(&state.host), pin);
}

/// A POST that fails after its own handshake may already have reached the
/// service, so it is never healed or replayed — even when another request
/// on the same client was refused by the pin meanwhile.
#[tokio::test]
async fn a_post_failing_after_its_handshake_is_never_replayed() {
    let request_read = Arc::new(AtomicBool::new(false));
    let hang_up = Arc::new(AtomicBool::new(false));
    let (read, hung_up) = (request_read.clone(), hang_up.clone());
    let upstream = spawn_tls_upstream(
        |index| {
            if index == 0 {
                &PINNED_KEY
            } else {
                &ROTATED_KEY
            }
        },
        move |index, mut stream| {
            if index != 0 {
                return reply_ok(stream);
            }
            // Take the request, then drop the connection without answering.
            read_request_head(&mut stream);
            read.store(true, Ordering::SeqCst);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !hung_up.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        },
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut state = state_over(upstream.base.clone(), tx);
    let mut events = capture_events(&mut state);
    state.client.pin(&state.host, &[PINNED_KEY.spki()]).unwrap();

    let proxy = spawn_server(build_proxy_router(state.clone())).await;
    let post = tokio::spawn(
        reqwest::Client::new()
            .post(format!("{proxy}/v1/chat/completions"))
            .header("content-type", "application/json")
            .body(REQUEST_BODY.to_vec())
            .send(),
    );
    wait_until(|| request_read.load(Ordering::SeqCst)).await;
    let refused = state
        .client
        .request(
            reqwest::Method::GET,
            &format!("{}/v1/models", upstream.base),
        )
        .send()
        .await;
    assert!(refused.is_err());
    assert_eq!(state.client.pin_rejections(&state.host), 1);
    hang_up.store(true, Ordering::SeqCst);

    assert_eq!(post.await.unwrap().unwrap().status().as_u16(), 502);
    assert!(blocked_codes(&mut events).is_empty());
    assert_eq!(state.reverify_attempts.load(Ordering::SeqCst), 0);
    // Only the POST's own connection completed a handshake: no re-verify
    // fetch and no replay.
    assert_eq!(upstream.completed.load(Ordering::SeqCst), 1);
}

/// The heal never widens trust: a pin refusal blocks like a keyset
/// rotation and re-verifies, but an upstream that cannot reach VERIFIED
/// keeps the stale pin and the 502 — no key is adopted on a failed verify.
#[tokio::test]
async fn a_refused_pin_on_an_unverifiable_host_fails_closed() {
    let upstream = unverifiable_upstream(std::time::Duration::ZERO);
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut state = state_over(upstream.base.clone(), tx);
    let mut events = capture_events(&mut state);
    let stale = vec![PINNED_KEY.spki()];
    state.client.pin(&state.host, &stale).unwrap();

    let proxy = spawn_server(build_proxy_router(state.clone())).await;
    let resp = reqwest::Client::new()
        .get(format!("{proxy}/v1/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 502);
    assert!(resp
        .text()
        .await
        .unwrap()
        .contains("upstream connection failed"));
    assert_eq!(state.client.pin_rejections(&state.host), 1);
    // The refusal went through the rotation path (`keyset_changed`, one
    // re-verify attempt whose unpinned fetch completed a handshake), and its
    // failure is reported like any failed re-verification.
    assert_eq!(blocked_codes(&mut events), vec![keyset_changed(), None]);
    assert_eq!(state.reverify_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(upstream.completed.load(Ordering::SeqCst), 1);
    assert_eq!(state.client.pinned_spkis(&state.host), stale);
    assert!(state.blocked.load(Ordering::SeqCst));
}

/// Single flight: requests that queue behind an in-flight heal share its
/// verdict — on both the POST and the passthrough path — instead of each
/// attesting the service again.
#[tokio::test]
async fn requests_queued_behind_a_heal_share_one_reverification() {
    const QUEUED: usize = 6;
    let upstream = unverifiable_upstream(std::time::Duration::from_millis(500));
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut state = state_over(upstream.base.clone(), tx);
    let mut events = capture_events(&mut state);
    state.client.pin(&state.host, &[PINNED_KEY.spki()]).unwrap();

    let proxy = spawn_server(build_proxy_router(state.clone())).await;
    let client = reqwest::Client::new();
    let first = tokio::spawn(client.get(format!("{proxy}/v1/models")).send());
    // The pin refusal blocked forwards; its re-verify is now in flight.
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(5), events.recv()).await,
        Ok(Some(VerifierEvent::Blocked { code: Some(code), .. })) if code == "keyset_changed"
    ));
    let queued = (0..QUEUED).map(|i| {
        let request = if i % 2 == 0 {
            client.get(format!("{proxy}/v1/models"))
        } else {
            client
                .post(format!("{proxy}/v1/chat/completions"))
                .header("content-type", "application/json")
                .body(REQUEST_BODY.to_vec())
        };
        async move { request.send().await.unwrap().status().as_u16() }
    });
    let queued = futures_util::future::join_all(queued).await;

    assert_eq!(first.await.unwrap().unwrap().status().as_u16(), 502);
    assert_eq!(queued, vec![503; QUEUED]);
    assert_eq!(state.reverify_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(upstream.completed.load(Ordering::SeqCst), 1);
    assert!(!blocked_codes(&mut events).contains(&keyset_changed()));
}

/// A request refused by a pin that another request's heal is replacing
/// waits for that heal instead of starting its own. `identity_changed`
/// selects whether the heal adopted a new keyset digest.
async fn refused_while_another_heal_completes(
    identity_changed: bool,
) -> (u16, String, TlsUpstream, Vec<Option<String>>) {
    let upstream = spawn_tls_upstream(|_| &ROTATED_KEY, |_, stream| reply_ok(stream));
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut state = state_over(upstream.base.clone(), tx);
    let mut events = capture_events(&mut state);
    state.client.pin(&state.host, &[PINNED_KEY.spki()]).unwrap();
    let admitted = state.delivery_token();

    let proxy = spawn_server(build_proxy_router(state.clone())).await;
    // Another request's heal holds the re-verify funnel.
    let heal = state.reverify.lock().await;
    let request = tokio::spawn(
        reqwest::Client::new()
            .get(format!("{proxy}/v1/models"))
            .send(),
    );
    wait_until(|| state.client.pin_rejections(&state.host) == 1).await;
    // That heal completes: the rotated key is pinned and every delivery
    // admitted under the previous verification is revoked.
    state
        .client
        .pin(&state.host, &[ROTATED_KEY.spki()])
        .unwrap();
    if identity_changed {
        state.trusted.lock().unwrap().keyset_digest = "rotated-keyset".to_string();
    }
    state.revoke_deliveries();
    drop(heal);

    let resp = request.await.unwrap().unwrap();
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap();
    assert!(admitted.is_cancelled());
    assert_eq!(state.reverify_attempts.load(Ordering::SeqCst), 0);
    (status, body, upstream, blocked_codes(&mut events))
}

/// When the heal kept the identity, the request is sent once more — under
/// the current delivery token, since the one it was admitted with was
/// revoked.
#[tokio::test]
async fn a_request_whose_pin_was_healed_elsewhere_is_sent_once_more() {
    let (status, body, upstream, blocked) = refused_while_another_heal_completes(false).await;
    assert_eq!(status, 200);
    assert_eq!(body, "ok");
    assert_eq!(upstream.completed.load(Ordering::SeqCst), 1);
    assert!(blocked.is_empty());
}

/// When the heal replaced the identity, the request admitted under the old
/// one is refused as retryable, never silently re-sent under the new one.
#[tokio::test]
async fn a_request_whose_identity_changed_during_the_heal_is_retryable() {
    let (status, body, upstream, blocked) = refused_while_another_heal_completes(true).await;
    assert_eq!(status, 503);
    assert!(body.contains("identity changed"), "{body}");
    assert_eq!(upstream.completed.load(Ordering::SeqCst), 0);
    assert!(blocked.is_empty());
}

/// In managed mode the backend answers `keyset_changed` by stopping this
/// verifier and rebuilding the session from a fresh one, so the request
/// that hit the stale pin is refused as retryable (503), not failed.
#[tokio::test]
async fn a_managed_pin_refusal_hands_off_to_the_session_rebuild() {
    let upstream = unverifiable_upstream(std::time::Duration::ZERO);
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut state = state_over(upstream.base.clone(), tx);
    let (events, mut received) = mpsc::unbounded_channel();
    let shutdown = state.shutdown.clone();
    Arc::get_mut(&mut state).unwrap().event_sink = Arc::new(move |event| {
        if matches!(&event, VerifierEvent::Blocked { code: Some(code), .. } if code == "keyset_changed")
        {
            shutdown.cancel();
        }
        let _ = events.send(event);
    });
    state.client.pin(&state.host, &[PINNED_KEY.spki()]).unwrap();

    let proxy = spawn_server(build_proxy_router(state.clone())).await;
    let resp = reqwest::Client::new()
        .post(format!("{proxy}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(REQUEST_BODY.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 503);
    assert_eq!(blocked_codes(&mut received), vec![keyset_changed()]);
}

/// Stale-pin heal against the live service: a bogus pin on the real host
/// makes every forward's handshake fail closed, exactly as when the service
/// rotates its attested TLS key. The proxy must re-verify (fresh nonce, full
/// §9.1 checks incl. the DCAP quote) and re-pin — not wedge in 502 until
/// restarted. The harness starts from a fixture identity, so the request
/// that triggered the heal sees the identity change (retryable 503) and the
/// next one goes through.
/// Run from apps/desktop with: cargo test --package private-ai-proxy --lib -- --ignored stale_pin
#[tokio::test]
#[ignore]
async fn stale_pin_heals_against_live_service() {
    let base = "https://inference.phala.com";
    let (tx, _rx) = mpsc::unbounded_channel();
    let state = state_over(base.to_string(), tx);
    let stale = "00".repeat(32);
    state
        .client
        .pin(&state.host, std::slice::from_ref(&stale))
        .unwrap();

    let proxy = spawn_server(build_proxy_router(state.clone())).await;
    let client = reqwest::Client::new();
    let healing = client
        .get(format!("{proxy}/v1/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(healing.status().as_u16(), 503);
    let resp = client
        .get(format!("{proxy}/v1/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let observed = state.client.observed_spki(&state.host).expect("observed");
    let pinned = state.client.pinned_spkis(&state.host);
    assert!(pinned.contains(&observed), "{pinned:?}");
    assert!(!pinned.contains(&stale));
}
