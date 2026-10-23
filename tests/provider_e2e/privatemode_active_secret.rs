use super::*;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use std::collections::VecDeque;

#[derive(Clone)]
struct EvidenceState {
    bundles: Arc<Mutex<BTreeMap<String, Value>>>,
    selected: Arc<AtomicUsize>,
    fetched: Arc<Mutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
    forwarded: Arc<Mutex<Vec<Vec<u8>>>>,
    chat_statuses: Arc<Mutex<VecDeque<StatusCode>>>,
    readiness_status: StatusCode,
    retry_after: Option<String>,
    missing_header: bool,
    header_value: Option<String>,
    evidence_status: StatusCode,
    evidence_location: String,
    oversized: bool,
    stall: bool,
    stalled_id: Option<String>,
    evidence_started: Arc<tokio::sync::Notify>,
}

impl Default for EvidenceState {
    fn default() -> Self {
        Self {
            bundles: Arc::new(Mutex::new(BTreeMap::from([
                (url_secret(0), bundle(0)),
                (url_secret(1), bundle(1)),
            ]))),
            selected: Arc::new(AtomicUsize::new(0)),
            fetched: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::new(AtomicUsize::new(0)),
            forwarded: Arc::new(Mutex::new(Vec::new())),
            chat_statuses: Arc::new(Mutex::new(VecDeque::new())),
            readiness_status: StatusCode::OK,
            retry_after: None,
            missing_header: false,
            header_value: None,
            evidence_status: StatusCode::OK,
            evidence_location: String::new(),
            oversized: false,
            stall: false,
            stalled_id: None,
            evidence_started: Arc::new(tokio::sync::Notify::new()),
        }
    }
}

fn secret(index: usize) -> String {
    BASE64.encode(sha2::Sha256::digest([index as u8; 32]))
}

fn url_secret(index: usize) -> String {
    URL_SAFE_NO_PAD.encode(sha2::Sha256::digest([index as u8; 32]))
}

fn bundle(index: usize) -> Value {
    let manifest = format!(r#"{{"Policies":{{}},"version":{index}}}"#);
    json!({
        "version": 1,
        "secret_id": secret(index),
        "manifest_sha256": normalized_sha256(manifest.as_bytes()),
        "manifest": BASE64.encode(manifest),
        "coordinator_attestation": {
            "nonce": BASE64.encode([0u8; 32]),
            "document": BASE64.encode(b"offline verifier checks this document"),
            "attested_at": "2026-10-23T00:00:00Z"
        },
        "mesh_ca_pem": "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n",
        "secret_service_cert": BASE64.encode([1u8; 64]),
        "request_public_key": BASE64.encode([index as u8; 32]),
        "encapsulated_key": BASE64.encode([2u8; 32]),
        "signature": BASE64.encode([3u8; 64]),
        "exchanged_at": "2026-10-23T00:00:01Z",
        "expires_at": "2026-10-23T00:45:01Z"
    })
}

async fn chat(State(state): State<EvidenceState>, body: Bytes) -> axum::response::Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    state.forwarded.lock().unwrap().push(body.to_vec());
    let status = state
        .chat_statuses
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or(StatusCode::OK);
    let index = state.selected.load(Ordering::SeqCst);
    // The proxy rotates after selecting the attempt's secret, before evidence is fetched.
    state.selected.store(1, Ordering::SeqCst);
    let streaming = serde_json::from_slice::<Value>(&body).unwrap()["stream"] == true;
    let mut response = if !status.is_success() {
        (
            [("content-type", "application/json")],
            r#"{"error":{"message":"synthetic upstream error"}}"#,
        )
            .into_response()
    } else if streaming {
        (
            [("content-type", "text/event-stream")],
            format!(
                "{}\n\ndata: [DONE]\n\n",
                String::from_utf8_lossy(STREAM_CHAT_RESPONSE_EVENT)
            ),
        )
            .into_response()
    } else {
        ([("content-type", "application/json")], CHAT_RESPONSE).into_response()
    };
    *response.status_mut() = status;
    if let Some(retry_after) = state.retry_after {
        response
            .headers_mut()
            .insert("retry-after", retry_after.parse().unwrap());
    }
    if !state.missing_header && status.is_success() {
        response.headers_mut().insert(
            "privatemode-secret-id",
            state
                .header_value
                .unwrap_or_else(|| secret(index))
                .parse()
                .unwrap(),
        );
    }
    response
}

async fn evidence(
    State(state): State<EvidenceState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    state.fetched.lock().unwrap().push(id.clone());
    state.evidence_started.notify_one();
    if state.stall || state.stalled_id.as_deref() == Some(id.as_str()) {
        return (
            [("content-type", "application/json")],
            Body::from_stream(futures_util::stream::pending::<Result<Bytes, std::io::Error>>()),
        )
            .into_response();
    }
    if state.oversized {
        // No Content-Length: exercise the streaming size limit as well.
        return (
            [("content-type", "application/json")],
            Body::from_stream(futures_util::stream::iter(
                (0..9).map(|_| Ok::<_, std::io::Error>(Bytes::from(vec![b' '; 1024 * 1024]))),
            )),
        )
            .into_response();
    }
    if state.evidence_status != StatusCode::OK {
        return (
            state.evidence_status,
            [("location", state.evidence_location)],
            "",
        )
            .into_response();
    }
    let bundle = state.bundles.lock().unwrap().get(&id).cloned();
    match bundle {
        Some(bundle) => Json(bundle).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn setup(
    state: EvidenceState,
    enabled: bool,
) -> (PrivatemodeTestDeployment, Arc<AciService>, Router) {
    let origin = serve_router(
        Router::new()
            .route(
                "/readyz",
                get(|State(state): State<EvidenceState>| async move { state.readiness_status }),
            )
            .route("/v1/chat/completions", post(chat))
            .route("/privatemode/v1/secret-evidence/:id", get(evidence))
            .with_state(state),
    )
    .await;
    let mut fixture = PrivatemodeTestDeployment::new(origin.clone());
    fixture.deployment = Arc::new(
        PrivatemodeProxyDeployment::new(
            &origin,
            &fixture.manifest_log_path,
            &fixture.credential_path,
            &fixture.credential_digest,
            &fixture.image_digest,
        )
        .unwrap()
        .with_active_secret_evidence(enabled),
    );
    let (_config_dir, path) = temp_config_path();
    let mut options = runtime_options(UpstreamVerifierMode::None);
    options.privatemode_proxy = Some(fixture.deployment.clone());
    let manager = Arc::new(UpstreamConfigManager::load(&path, options).unwrap());
    manager
        .replace(vec![privatemode_upstream_config(origin, 300)])
        .unwrap();
    let service = service_for_manager(manager);
    let app = build_router(service.clone());
    (fixture, service, app)
}

fn request(stream: bool) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "model":"public-model", "messages":[{"role":"user","content":"hello"}],
        "stream":stream, "provider":{"aci_verified":true}
    }))
    .unwrap()
}

fn receipt_session(service: &AciService, headers: &HeaderMap) -> (Value, Value) {
    let id = headers["x-receipt-id"].to_str().unwrap();
    let receipt = service.get_receipt_by_receipt_id(id).unwrap();
    let event = receipt_event(&receipt, EVENT_UPSTREAM_VERIFIED);
    let session = service
        .get_attested_session(event["session_id"].as_str().unwrap())
        .unwrap();
    (event, serde_json::from_slice(session.bytes()).unwrap())
}

#[tokio::test]
async fn privatemode_active_secret_receipts_follow_each_response_across_rotation_and_cache() {
    for streaming in [false, true] {
        let state = EvidenceState::default();
        let (fixture, service, app) = setup(state.clone(), true).await;
        assert!(service
            .list_current_sessions(&[fixture.verification_request("provider-model")])
            .unwrap()
            .is_empty());
        // Enabled mode does not depend on observational manifest logs.
        std::fs::remove_file(&fixture.manifest_log_path).unwrap();
        let mut ids = Vec::new();
        for index in [0, 1, 1] {
            let (status, headers, body) = call(
                app.clone(),
                "POST",
                "/v1/chat/completions",
                request(streaming),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
            let (event, document) = receipt_session(&service, &headers);
            ids.push(event["session_id"].clone());
            let claims = &document["claims"];
            assert_eq!(claims["extra"]["manifest_bound_to_active_secret"], true);
            assert_eq!(claims["extra"]["secret_id"], secret(index));
            assert_eq!(
                claims["extra"]["active_manifest_sha256"],
                bundle(index)["manifest_sha256"]
            );
            for claim in ["gpu_attested", "os_known_good", "model_weights_provenance"] {
                assert_eq!(claims[claim]["status"], "unknown");
            }
            let encoded = document["evidence"]["data"]
                .as_str()
                .unwrap()
                .strip_prefix("data:application/json;base64,")
                .unwrap();
            let decoded = BASE64.decode(encoded).unwrap();
            assert_eq!(document["evidence"]["digest"], sha256_hex(&decoded));
            assert_eq!(
                serde_json::from_slice::<Value>(&decoded).unwrap(),
                bundle(index)
            );
        }
        assert_ne!(ids[0], ids[1]);
        assert_eq!(ids[1], ids[2]);
        assert_eq!(
            *state.fetched.lock().unwrap(),
            [url_secret(0), url_secret(1)]
        );
    }
}

#[tokio::test]
async fn privatemode_active_secret_failures_withhold_buffered_and_streaming_bodies_and_issue_refusals(
) {
    for streaming in [false, true] {
        for failure in [
            "header",
            "badheader",
            "unknown",
            "mismatch",
            "malformed",
            "oversized",
        ] {
            let mut state = EvidenceState::default();
            match failure {
                "header" => state.missing_header = true,
                "badheader" => state.header_value = Some("not-a-secret-id".to_string()),
                "unknown" => {
                    state.bundles.lock().unwrap().clear();
                }
                "mismatch" => {
                    state
                        .bundles
                        .lock()
                        .unwrap()
                        .insert(url_secret(0), bundle(1));
                }
                "malformed" => {
                    state
                        .bundles
                        .lock()
                        .unwrap()
                        .get_mut(&url_secret(0))
                        .unwrap()["version"] = json!(2);
                }
                "oversized" => state.oversized = true,
                _ => unreachable!(),
            }
            let (_fixture, service, app) = setup(state.clone(), true).await;
            let (status, headers, body) =
                call(app, "POST", "/v1/chat/completions", request(streaming)).await;
            assert_eq!(
                status,
                StatusCode::SERVICE_UNAVAILABLE,
                "{failure}: {}",
                String::from_utf8_lossy(&body)
            );
            assert!(!String::from_utf8_lossy(&body).contains("world"));
            let receipt_id = headers
                .get("x-receipt-id")
                .expect("verification refusal has a receipt")
                .to_str()
                .unwrap();
            let receipt = service.get_receipt_by_receipt_id(receipt_id).unwrap();
            let event = receipt_event(&receipt, EVENT_UPSTREAM_VERIFIED);
            assert_eq!(event["result"], "failed");
            assert_eq!(event["required"], true);
            assert!(event.get("session_id").is_none());
            let document = receipt.document_json().unwrap();
            let types: Vec<_> = document["event_log"]
                .as_array()
                .unwrap()
                .iter()
                .map(|event| event["type"].as_str().unwrap())
                .collect();
            assert_eq!(
                types,
                [
                    "request.received",
                    "request.forwarded",
                    "upstream.verified",
                    "response.returned"
                ]
            );
            assert_eq!(
                receipt_event(&receipt, EVENT_REQUEST_FORWARDED)["body_hash"],
                sha256_hex(&state.forwarded.lock().unwrap()[0])
            );
            assert_eq!(
                receipt_event(&receipt, "response.returned")["body_hash"],
                sha256_hex(&body)
            );
        }
    }
}

#[tokio::test]
async fn privatemode_active_secret_default_off_ignores_missing_header_and_evidence() {
    let state = EvidenceState {
        missing_header: true,
        ..EvidenceState::default()
    };
    let (_fixture, service, app) = setup(state.clone(), false).await;
    let (status, headers, body) = call(app, "POST", "/v1/chat/completions", CHAT_REQUEST).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, CHAT_RESPONSE);
    let (_, session) = receipt_session(&service, &headers);
    assert_eq!(
        session["claims"]["extra"]["manifest_bound_to_active_secret"],
        false
    );
    assert!(session["claims"]["extra"]
        .get("active_secret_evidence")
        .is_none());
    assert!(state.fetched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn privatemode_active_secret_evidence_refuses_redirects_and_bounds_stalled_bodies() {
    let sink_hits = Arc::new(AtomicUsize::new(0));
    let sink = sink_hits.clone();
    let sink_origin = serve_router(Router::new().route(
        "/",
        get(move || {
            let sink = sink.clone();
            async move {
                sink.fetch_add(1, Ordering::SeqCst);
                Json(bundle(0))
            }
        }),
    ))
    .await;
    let state = EvidenceState {
        evidence_status: StatusCode::TEMPORARY_REDIRECT,
        evidence_location: sink_origin,
        ..EvidenceState::default()
    };
    let (fixture, _, _) = setup(state, true).await;
    let verifier = fixture.verifier(1, 300);
    let req = fixture.verification_request("provider-model");
    let event = verifier.verify(req.clone()).await;
    let headers =
        std::collections::HashMap::from([("privatemode-secret-id".to_string(), secret(0))]);
    assert!(verifier
        .verify_response(req, event, &headers)
        .await
        .is_err());
    assert_eq!(sink_hits.load(Ordering::SeqCst), 0);

    let (fixture, _, _) = setup(
        EvidenceState {
            stall: true,
            ..EvidenceState::default()
        },
        true,
    )
    .await;
    let verifier = fixture.verifier(1, 300);
    let req = fixture.verification_request("provider-model");
    let event = verifier.verify(req.clone()).await;
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        verifier.verify_response(req, event, &headers),
    )
    .await;
    assert!(result
        .expect("evidence body must respect total deadline")
        .is_err());
}

#[tokio::test]
async fn privatemode_active_secret_pinned_session_refuses_rotation_after_preflight() {
    for streaming in [false, true] {
        let (_fixture, service, app) = setup(EvidenceState::default(), true).await;
        let (status, headers, _) =
            call(app.clone(), "POST", "/v1/chat/completions", request(false)).await;
        assert_eq!(status, StatusCode::OK);
        let (event, _) = receipt_session(&service, &headers);
        let mut pinned: Value = serde_json::from_slice(&request(streaming)).unwrap();
        pinned["provider"]["aci_session_ids"] = json!([event["session_id"]]);
        let (status, headers, body) = call(
            app,
            "POST",
            "/v1/chat/completions",
            serde_json::to_vec(&pinned).unwrap(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::PRECONDITION_FAILED,
            "{}",
            String::from_utf8_lossy(&body)
        );
        assert!(!String::from_utf8_lossy(&body).contains("world"));
        let receipt = service
            .get_receipt_by_receipt_id(headers["x-receipt-id"].to_str().unwrap())
            .unwrap();
        assert!(receipt_event(&receipt, EVENT_UPSTREAM_VERIFIED)
            .get("session_id")
            .is_none());
    }
}

#[tokio::test]
async fn privatemode_active_secret_rejects_malformed_contract_fields() {
    for (field, value) in [
        ("version", json!("1")),
        ("manifest_sha256", json!("A".repeat(64))),
        ("manifest", json!("not base64")),
        ("manifest", json!(BASE64.encode(b"[]"))),
        ("request_public_key", json!(BASE64.encode([255u8; 32]))),
        ("encapsulated_key", json!("")),
        ("signature", json!("AQI")),
        ("secret_service_cert", json!("%%")),
        ("mesh_ca_pem", json!("not PEM")),
        ("exchanged_at", json!("tomorrow")),
        ("expires_at", json!("2020-01-01T00:00:00Z")),
        ("extra_field", json!(true)),
    ] {
        let state = EvidenceState::default();
        state
            .bundles
            .lock()
            .unwrap()
            .get_mut(&url_secret(0))
            .unwrap()[field] = value;
        let (fixture, _, _) = setup(state, true).await;
        let verifier = fixture.verifier(1, 300);
        let req = fixture.verification_request("provider-model");
        let event = verifier.verify(req.clone()).await;
        let headers =
            std::collections::HashMap::from([("privatemode-secret-id".to_string(), secret(0))]);
        assert!(
            verifier
                .verify_response(req, event, &headers)
                .await
                .is_err(),
            "{field}"
        );
    }
}

#[tokio::test]
async fn privatemode_active_secret_evidence_cache_evicts_after_eight_ids() {
    let state = EvidenceState::default();
    for index in 0..9 {
        state
            .bundles
            .lock()
            .unwrap()
            .insert(url_secret(index), bundle(index));
    }
    let (fixture, _, _) = setup(state.clone(), true).await;
    let verifier = fixture.verifier(1, 300);
    let req = fixture.verification_request("provider-model");
    let event = verifier.verify(req.clone()).await;
    for index in (0..9).chain([0]) {
        let headers =
            std::collections::HashMap::from([("privatemode-secret-id".to_string(), secret(index))]);
        verifier
            .verify_response(req.clone(), event.clone(), &headers)
            .await
            .unwrap();
    }
    assert_eq!(state.fetched.lock().unwrap().len(), 10);
}

#[tokio::test]
async fn privatemode_active_secret_slow_evidence_does_not_block_other_ids() {
    let state = EvidenceState {
        stalled_id: Some(url_secret(0)),
        ..EvidenceState::default()
    };
    let (fixture, _, _) = setup(state.clone(), true).await;
    let verifier = Arc::new(fixture.verifier(10, 300));
    let req = fixture.verification_request("provider-model");
    let event = verifier.verify(req.clone()).await;
    let stalled = tokio::spawn({
        let verifier = verifier.clone();
        let req = req.clone();
        let event = event.clone();
        async move {
            let headers =
                std::collections::HashMap::from([("privatemode-secret-id".to_string(), secret(0))]);
            verifier.verify_response(req, event, &headers).await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), state.evidence_started.notified())
        .await
        .expect("first evidence fetch must start");
    let headers =
        std::collections::HashMap::from([("privatemode-secret-id".to_string(), secret(1))]);
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        verifier.verify_response(req, event, &headers),
    )
    .await;
    let still_pending = !stalled.is_finished();
    stalled.abort();
    assert!(stalled.await.unwrap_err().is_cancelled());
    assert!(
        still_pending,
        "the other secret completed while the first stalled"
    );
    assert!(result
        .expect("a slow ID must not hold the cache lock")
        .is_ok());
    assert_eq!(
        *state.fetched.lock().unwrap(),
        [url_secret(0), url_secret(1)]
    );
}

#[tokio::test]
async fn privatemode_active_secret_middleware_commits_only_response_bound_receipts() {
    for streaming in [false, true] {
        for missing_header in [false, true] {
            let (_fixture, service, _) = setup(
                EvidenceState {
                    missing_header,
                    ..EvidenceState::default()
                },
                true,
            )
            .await;
            let body = request(streaming);
            let journal = MiddlewareReceiptJournal::default();
            let result = service
                .forward_chat_completion_for_middleware(
                    ChatCompletionRequest {
                        context: GatewayRequestContext {
                            user_model: Some("public-model".to_string()),
                            ..Default::default()
                        },
                        endpoint_path: "/v1/chat/completions",
                        received_body: &body,
                        forwarded_body: None,
                        aci_required: true,
                        aci_session_ids: Vec::new(),
                        upstream_verification_event: None,
                        requester: None,
                        e2ee: None,
                    },
                    vec![ForwardCandidate {
                        route_id: "privatemode-provider:public-model".to_string(),
                        body: body.clone(),
                        path: "/v1/chat/completions",
                    }],
                    streaming,
                    journal.clone(),
                )
                .await
                .unwrap();
            let (draft, body, session_id) = match result {
                MiddlewareForwardResult::Forwarded(forward) => {
                    (forward.receipt, forward.upstream_body, forward.session_id)
                }
                MiddlewareForwardResult::Stream(mut forward) => {
                    let mut bytes = Vec::new();
                    while let Some(chunk) = forward.body.next().await {
                        bytes.extend_from_slice(&chunk.unwrap());
                    }
                    (journal.take().unwrap(), bytes, forward.session_id)
                }
                MiddlewareForwardResult::AllFailed(failure) => {
                    assert!(missing_header);
                    assert!(matches!(
                        failure.error,
                        private_ai_gateway::aggregator::service::ServiceError::UpstreamVerificationAfterForwarding { .. }
                    ));
                    assert!(journal.take().is_none());
                    continue;
                }
                _ => panic!("expected response-bound receipt or fail-closed refusal"),
            };
            assert!(!missing_header);
            let finalization = service
                .finalize_middleware_receipt(
                    draft,
                    &body,
                    Some(if streaming {
                        "text/event-stream"
                    } else {
                        "application/json"
                    }),
                    None,
                    None,
                )
                .unwrap();
            let event = receipt_event(&finalization.receipt, EVENT_UPSTREAM_VERIFIED);
            assert_eq!(event["session_id"], session_id.unwrap());
            let session = service
                .get_attested_session(event["session_id"].as_str().unwrap())
                .unwrap();
            assert_eq!(session.document().claims.extra["secret_id"], secret(0));
            assert_eq!(
                session.document().claims.extra["manifest_bound_to_active_secret"],
                true
            );
        }
    }
}

#[tokio::test]
async fn privatemode_active_secret_non_success_responses_preserve_status_and_retry_after() {
    for streaming in [false, true] {
        for upstream_status in [
            StatusCode::BAD_REQUEST,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            let mut outcomes = Vec::new();
            for enabled in [false, true] {
                let state = EvidenceState {
                    chat_statuses: Arc::new(Mutex::new(VecDeque::from([upstream_status]))),
                    retry_after: Some("5".to_string()),
                    ..EvidenceState::default()
                };
                let (_fixture, service, app) = setup(state.clone(), enabled).await;
                if streaming {
                    let (status, _, body) =
                        call(app, "POST", "/v1/chat/completions", request(true)).await;
                    outcomes.push((status.as_u16(), body));
                } else {
                    let result = service
                        .forward_chat_completion(&request(false), None, true, None)
                        .await
                        .unwrap();
                    assert_eq!(result.upstream_headers["retry-after"], "5");
                    if enabled {
                        let event = receipt_event(&result.receipt, EVENT_UPSTREAM_VERIFIED);
                        assert_eq!(event["result"], "failed");
                        assert!(event.get("session_id").is_none());
                    }
                    outcomes.push((result.upstream_status, result.upstream_body));
                }
                assert!(state.fetched.lock().unwrap().is_empty());
                assert_eq!(state.calls.load(Ordering::SeqCst), 1);
            }
            assert_eq!(outcomes[0], outcomes[1]);
            assert_eq!(outcomes[0].0, upstream_status.as_u16());
        }
    }
}

#[tokio::test]
async fn privatemode_active_secret_all_success_statuses_still_require_evidence() {
    for streaming in [false, true] {
        for upstream_status in [StatusCode::OK, StatusCode::CREATED, StatusCode::NO_CONTENT] {
            let state = EvidenceState {
                missing_header: true,
                chat_statuses: Arc::new(Mutex::new(VecDeque::from([upstream_status]))),
                ..EvidenceState::default()
            };
            let (_fixture, service, app) = setup(state.clone(), true).await;
            let (status, headers, _) =
                call(app, "POST", "/v1/chat/completions", request(streaming)).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            let receipt = service
                .get_receipt_by_receipt_id(headers["x-receipt-id"].to_str().unwrap())
                .unwrap();
            assert_eq!(
                receipt_event(&receipt, EVENT_REQUEST_FORWARDED)["body_hash"],
                sha256_hex(&state.forwarded.lock().unwrap()[0])
            );
        }
    }
}

#[tokio::test]
async fn privatemode_active_secret_preforward_refusal_omits_forwarded_event() {
    for streaming in [false, true] {
        let state = EvidenceState {
            readiness_status: StatusCode::SERVICE_UNAVAILABLE,
            ..EvidenceState::default()
        };
        let (_fixture, service, app) = setup(state.clone(), true).await;
        let (status, headers, _) =
            call(app, "POST", "/v1/chat/completions", request(streaming)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let receipt = service
            .get_receipt_by_receipt_id(headers["x-receipt-id"].to_str().unwrap())
            .unwrap();
        let document = receipt.document_json().unwrap();
        let types: Vec<_> = document["event_log"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            types,
            ["request.received", "upstream.verified", "response.returned"]
        );
        assert!(state.forwarded.lock().unwrap().is_empty());
        assert!(state.fetched.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn privatemode_active_secret_evidence_failures_also_refuse_unconstrained_requests() {
    for streaming in [false, true] {
        let state = EvidenceState {
            missing_header: true,
            ..EvidenceState::default()
        };
        let (_fixture, service, app) = setup(state.clone(), true).await;
        let mut body: Value = serde_json::from_slice(&request(streaming)).unwrap();
        body.as_object_mut().unwrap().remove("provider");
        let (status, headers, _) = call(
            app,
            "POST",
            "/v1/chat/completions",
            serde_json::to_vec(&body).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let receipt = service
            .get_receipt_by_receipt_id(headers["x-receipt-id"].to_str().unwrap())
            .unwrap();
        assert_eq!(
            receipt_event(&receipt, EVENT_REQUEST_FORWARDED)["body_hash"],
            sha256_hex(&state.forwarded.lock().unwrap()[0])
        );
        assert_eq!(
            receipt_event(&receipt, EVENT_UPSTREAM_VERIFIED)["result"],
            "failed"
        );
    }
}

#[tokio::test]
async fn privatemode_active_secret_non_success_responses_keep_middleware_retry_and_failover() {
    for streaming in [false, true] {
        for first_status in [
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            let state = EvidenceState {
                chat_statuses: Arc::new(Mutex::new(VecDeque::from([first_status, StatusCode::OK]))),
                retry_after: Some("5".to_string()),
                ..EvidenceState::default()
            };
            let (_fixture, service, _) = setup(state.clone(), true).await;
            let body = request(streaming);
            let candidate = || ForwardCandidate {
                route_id: "privatemode-provider:public-model".to_string(),
                body: body.clone(),
                path: "/v1/chat/completions",
            };
            let candidates = if first_status == StatusCode::TOO_MANY_REQUESTS {
                vec![candidate()]
            } else {
                vec![candidate(), candidate()]
            };
            let journal = MiddlewareReceiptJournal::default();
            let started = tokio::time::Instant::now();
            let result = service
                .forward_chat_completion_for_middleware(
                    ChatCompletionRequest {
                        context: GatewayRequestContext {
                            user_model: Some("public-model".to_string()),
                            ..Default::default()
                        },
                        endpoint_path: "/v1/chat/completions",
                        received_body: &body,
                        forwarded_body: None,
                        aci_required: true,
                        aci_session_ids: Vec::new(),
                        upstream_verification_event: None,
                        requester: None,
                        e2ee: None,
                    },
                    candidates,
                    streaming,
                    journal.clone(),
                )
                .await
                .unwrap();
            let (session_id, failed_attempts) = match result {
                MiddlewareForwardResult::Forwarded(forward) => {
                    (forward.session_id, forward.failed_attempts)
                }
                MiddlewareForwardResult::Stream(mut forward) => {
                    while let Some(chunk) = forward.body.next().await {
                        chunk.unwrap();
                    }
                    (forward.session_id, forward.failed_attempts)
                }
                _ => panic!("the ordinary capacity retry or failover must serve the next response"),
            };
            if first_status == StatusCode::TOO_MANY_REQUESTS {
                assert!(
                    started.elapsed() >= Duration::from_secs(5),
                    "capacity retry honors Retry-After"
                );
            }
            assert_eq!(failed_attempts.len(), 1);
            assert_eq!(failed_attempts[0].status, first_status.as_u16());
            assert_eq!(state.calls.load(Ordering::SeqCst), 2);
            assert_eq!(*state.fetched.lock().unwrap(), [url_secret(1)]);
            let session = service.get_attested_session(&session_id.unwrap()).unwrap();
            assert_eq!(session.document().claims.extra["secret_id"], secret(1));
        }
    }
}
