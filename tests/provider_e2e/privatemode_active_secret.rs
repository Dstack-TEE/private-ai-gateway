use super::*;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

#[derive(Clone)]
struct EvidenceState {
    bundles: Arc<Mutex<BTreeMap<String, Value>>>,
    selected: Arc<AtomicUsize>,
    fetched: Arc<Mutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
    missing_header: bool,
    header_value: Option<String>,
    evidence_status: StatusCode,
    evidence_location: String,
    oversized: bool,
    stall: bool,
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
            missing_header: false,
            header_value: None,
            evidence_status: StatusCode::OK,
            evidence_location: String::new(),
            oversized: false,
            stall: false,
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
    let index = state.selected.load(Ordering::SeqCst);
    // The proxy rotates after selecting the attempt's secret, before evidence is fetched.
    state.selected.store(1, Ordering::SeqCst);
    let streaming = serde_json::from_slice::<Value>(&body).unwrap()["stream"] == true;
    let mut response = if streaming {
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
    if !state.missing_header {
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
    if state.stall {
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
            .route("/readyz", get(|| async { StatusCode::OK }))
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
            let (_fixture, service, app) = setup(state, true).await;
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
                        private_ai_gateway::aggregator::service::ServiceError::UpstreamVerification(
                            _
                        )
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
