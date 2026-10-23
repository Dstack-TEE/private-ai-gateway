use super::*;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use std::collections::VecDeque;

#[derive(Default)]
struct Proxy {
    bundles: BTreeMap<String, Value>,
    selected: usize,
    header: Option<String>,
    statuses: VecDeque<StatusCode>,
    retry_after: Option<String>,
    redirect: Option<String>,
    oversized: bool,
    stalled: Option<String>,
    fetched: Vec<String>,
    forwarded: Vec<Vec<u8>>,
    started: Arc<tokio::sync::Notify>,
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
        "version": 1, "secret_id": secret(index),
        "manifest_sha256": normalized_sha256(manifest.as_bytes()), "manifest": BASE64.encode(manifest),
        "coordinator_attestation": {
            "nonce": BASE64.encode([0u8; 32]), "document": BASE64.encode(b"synthetic attestation"),
            "attested_at": "2026-10-23T00:00:00Z"
        },
        "mesh_ca_pem": "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n",
        "secret_service_cert": BASE64.encode([1u8; 64]), "request_public_key": BASE64.encode([index as u8; 32]),
        "encapsulated_key": BASE64.encode([2u8; 32]), "signature": BASE64.encode([3u8; 64]),
        "exchanged_at": "2026-10-23T00:00:01Z", "expires_at": "2026-10-23T00:45:01Z"
    })
}

async fn chat(State(proxy): State<Arc<Mutex<Proxy>>>, body: Bytes) -> axum::response::Response {
    let mut proxy = proxy.lock().unwrap();
    proxy.forwarded.push(body.to_vec());
    let status = proxy.statuses.pop_front().unwrap_or(StatusCode::OK);
    let header = proxy
        .header
        .clone()
        .unwrap_or_else(|| secret(proxy.selected));
    proxy.selected = 1; // Rotate after the attempt selects its secret.
    let streaming = serde_json::from_slice::<Value>(&body).unwrap()["stream"] == true;
    let (content_type, body) = match (status.is_success(), streaming) {
        (false, _) => (
            "application/json",
            br#"{"error":{"message":"synthetic upstream error"}}"#.to_vec(),
        ),
        (true, false) => ("application/json", CHAT_RESPONSE.to_vec()),
        (true, true) => (
            "text/event-stream",
            format!(
                "{}\n\ndata: [DONE]\n\n",
                String::from_utf8_lossy(STREAM_CHAT_RESPONSE_EVENT)
            )
            .into_bytes(),
        ),
    };
    let mut response = (status, [("content-type", content_type)], body).into_response();
    if status.is_success() && !header.is_empty() {
        response
            .headers_mut()
            .insert("privatemode-secret-id", header.parse().unwrap());
    }
    if let Some(value) = &proxy.retry_after {
        response
            .headers_mut()
            .insert("retry-after", value.parse().unwrap());
    }
    response
}

async fn evidence(
    State(proxy): State<Arc<Mutex<Proxy>>>,
    Path(id): Path<String>,
) -> axum::response::Response {
    let mut proxy = proxy.lock().unwrap();
    proxy.fetched.push(id.clone());
    proxy.started.notify_one();
    let body = if proxy.stalled.as_ref() == Some(&id) {
        Body::from_stream(futures_util::stream::pending::<Result<Bytes, std::io::Error>>())
    } else if proxy.oversized {
        Body::from_stream(futures_util::stream::iter([Ok::<_, std::io::Error>(
            Bytes::from(vec![b' '; 8 * 1024 * 1024 + 1]),
        )]))
    } else if let Some(location) = &proxy.redirect {
        return (StatusCode::TEMPORARY_REDIRECT, [("location", location)], "").into_response();
    } else {
        return match proxy.bundles.get(&id) {
            Some(bundle) => Json(bundle.clone()).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        };
    };
    ([("content-type", "application/json")], body).into_response()
}

struct Fixture {
    proxy: Arc<Mutex<Proxy>>,
    deployment: PrivatemodeTestDeployment,
    verifier: PrivatemodeProviderVerifier,
    service: Arc<AciService>,
    app: Router,
}

impl Fixture {
    async fn new(enabled: bool) -> Self {
        let proxy = Arc::new(Mutex::new(Proxy {
            bundles: (0..11).map(|id| (url_secret(id), bundle(id))).collect(),
            ..Default::default()
        }));
        let origin = serve_router(
            Router::new()
                .route("/readyz", get(|| async { StatusCode::OK }))
                .route("/v1/chat/completions", post(chat))
                .route("/privatemode/v1/secret-evidence/:id", get(evidence))
                .with_state(proxy.clone()),
        )
        .await;
        let mut deployment = PrivatemodeTestDeployment::new(origin.clone());
        deployment.deployment = Arc::new(
            Arc::try_unwrap(deployment.deployment)
                .unwrap()
                .with_active_secret_evidence(enabled),
        );
        let verifier = deployment.verifier(2, 300);
        let (_dir, path) = temp_config_path();
        let mut options = runtime_options(UpstreamVerifierMode::None);
        options.privatemode_proxy = Some(deployment.deployment.clone());
        let manager = Arc::new(UpstreamConfigManager::load(&path, options).unwrap());
        manager
            .replace(vec![privatemode_upstream_config(origin, 300)])
            .unwrap();
        let service = service_for_manager(manager);
        let app = build_router(service.clone());
        Self {
            proxy,
            deployment,
            verifier,
            service,
            app,
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, Proxy> {
        self.proxy.lock().unwrap()
    }

    async fn send(&self, body: Value) -> (StatusCode, Value, Vec<u8>) {
        let (status, headers, body) = call(
            self.app.clone(),
            "POST",
            "/v1/chat/completions",
            serde_json::to_vec(&body).unwrap(),
        )
        .await;
        let receipt = headers
            .get("x-receipt-id")
            .map(|id| {
                self.service
                    .get_receipt_by_receipt_id(id.to_str().unwrap())
                    .unwrap()
                    .document_json()
                    .unwrap()
            })
            .unwrap_or(Value::Null);
        (status, receipt, body)
    }

    fn session(&self, receipt: &Value) -> Value {
        let id = event(receipt, EVENT_UPSTREAM_VERIFIED)["session_id"]
            .as_str()
            .unwrap();
        serde_json::from_slice(self.service.get_attested_session(id).unwrap().bytes()).unwrap()
    }

    async fn bind(&self, id: usize) -> Result<Option<UpstreamVerifiedEvent>, String> {
        let request = self.deployment.verification_request("provider-model");
        let event = self.verifier.verify(request.clone()).await;
        let headers =
            std::collections::HashMap::from([("privatemode-secret-id".to_string(), secret(id))]);
        self.verifier
            .verify_response(request, event, &headers)
            .await
    }
}

fn request(stream: bool) -> Value {
    json!({"model":"public-model", "messages":[{"role":"user","content":"hello"}],
           "stream":stream, "provider":{"aci_verified":true}})
}

fn event<'a>(receipt: &'a Value, kind: &str) -> &'a Value {
    receipt["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == kind)
        .unwrap()
}

fn assert_events(receipt: &Value, expected: &[&str]) {
    let types: Vec<_> = receipt["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(types, expected);
}

#[tokio::test]
async fn privatemode_active_secret_binds_rotating_responses_and_discovers_sessions() {
    for streaming in [false, true] {
        let f = Fixture::new(true).await;
        assert!(f
            .service
            .list_current_sessions(&[f.deployment.verification_request("provider-model")])
            .unwrap()
            .is_empty());
        std::fs::remove_file(&f.deployment.manifest_log_path).unwrap();
        let mut ids = Vec::new();
        for index in [0, 1, 1] {
            let (status, receipt, _) = f.send(request(streaming)).await;
            assert_eq!(status, StatusCode::OK);
            ids.push(event(&receipt, EVENT_UPSTREAM_VERIFIED)["session_id"].clone());
            let session = f.session(&receipt);
            let claims = &session["claims"];
            assert_eq!(claims["extra"]["manifest_bound_to_active_secret"], true);
            assert_eq!(claims["extra"]["secret_id"], secret(index));
            assert_eq!(
                claims["extra"]["active_manifest_sha256"],
                bundle(index)["manifest_sha256"]
            );
            for name in ["gpu_attested", "os_known_good", "model_weights_provenance"] {
                assert_eq!(claims[name]["status"], "unknown");
            }
            let data = session["evidence"]["data"].as_str().unwrap();
            let raw = BASE64
                .decode(data.strip_prefix("data:application/json;base64,").unwrap())
                .unwrap();
            assert_eq!(session["evidence"]["digest"], sha256_hex(&raw));
            assert_eq!(
                serde_json::from_slice::<Value>(&raw).unwrap(),
                bundle(index)
            );
        }
        assert_ne!(ids[0], ids[1]);
        assert_eq!(ids[1], ids[2]);
        assert_eq!(f.state().fetched, [url_secret(0), url_secret(1)]);
    }
}

#[tokio::test]
async fn privatemode_active_secret_default_off_preserves_wire_and_claims() {
    let f = Fixture::new(false).await;
    f.state().header = Some(String::new());
    let (status, receipt, body) = f.send(request(false)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, CHAT_RESPONSE);
    let extra = &f.session(&receipt)["claims"]["extra"];
    assert_eq!(extra["manifest_bound_to_active_secret"], false);
    assert!(extra.get("active_secret_evidence").is_none());
    assert!(f.state().fetched.is_empty());
}

#[tokio::test]
async fn privatemode_active_secret_refusals_withhold_2xx_and_record_forwarding_context() {
    for streaming in [false, true] {
        for (failure, status, required) in [
            ("header", 200, true),
            ("header", 201, true),
            ("header", 204, true),
            ("header", 200, false),
            ("badheader", 200, true),
            ("unknown", 200, true),
            ("mismatch", 200, true),
            ("malformed", 200, true),
            ("oversized", 200, true),
        ] {
            let f = Fixture::new(true).await;
            {
                let mut p = f.state();
                p.statuses.push_back(StatusCode::from_u16(status).unwrap());
                match failure {
                    "header" => p.header = Some(String::new()),
                    "badheader" => p.header = Some("invalid-id".to_string()),
                    "unknown" => p.bundles.clear(),
                    "mismatch" => {
                        p.bundles.insert(url_secret(0), bundle(1));
                    }
                    "malformed" => p.bundles.get_mut(&url_secret(0)).unwrap()["version"] = json!(2),
                    "oversized" => p.oversized = true,
                    _ => unreachable!(),
                }
            }
            let mut body = request(streaming);
            if !required {
                body.as_object_mut().unwrap().remove("provider");
            }
            let (status, receipt, body) = f.send(body).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{failure}");
            assert_events(
                &receipt,
                &[
                    "request.received",
                    "request.forwarded",
                    "upstream.verified",
                    "response.returned",
                ],
            );
            assert_eq!(
                event(&receipt, EVENT_REQUEST_FORWARDED)["body_hash"],
                sha256_hex(&f.state().forwarded[0])
            );
            let verified = event(&receipt, EVENT_UPSTREAM_VERIFIED);
            assert_eq!(verified["result"], "failed");
            assert!(verified.get("session_id").is_none());
            assert_eq!(
                event(&receipt, "response.returned")["body_hash"],
                sha256_hex(&body)
            );
            assert!(!String::from_utf8_lossy(&body).contains("world"));
        }
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
        let f = Fixture::new(true).await;
        f.state().bundles.get_mut(&url_secret(0)).unwrap()[field] = value;
        assert!(f.bind(0).await.is_err(), "{field}");
    }
}

#[tokio::test]
async fn privatemode_active_secret_fetch_refuses_redirects_and_bounds_stalled_bodies() {
    let f = Fixture::new(true).await;
    let sink = Fixture::new(true).await;
    f.state().redirect = Some(format!(
        "{}/privatemode/v1/secret-evidence/{}",
        sink.deployment.base_url,
        url_secret(0)
    ));
    assert!(f.bind(0).await.is_err());
    assert!(sink.state().fetched.is_empty());
    f.state().redirect = None;
    f.state().stalled = Some(url_secret(0));
    assert!(tokio::time::timeout(Duration::from_secs(4), f.bind(0))
        .await
        .expect("total evidence deadline")
        .is_err());
}

#[tokio::test]
async fn privatemode_active_secret_pinned_session_refuses_rotation_after_preflight() {
    for streaming in [false, true] {
        let f = Fixture::new(true).await;
        let (_, receipt, _) = f.send(request(false)).await;
        let mut pinned = request(streaming);
        pinned["provider"]["aci_session_ids"] =
            json!([event(&receipt, EVENT_UPSTREAM_VERIFIED)["session_id"]]);
        let (status, receipt, body) = f.send(pinned).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED);
        assert!(event(&receipt, EVENT_UPSTREAM_VERIFIED)
            .get("session_id")
            .is_none());
        assert!(!String::from_utf8_lossy(&body).contains("world"));
    }
}

#[tokio::test]
async fn privatemode_active_secret_cache_is_bounded_and_does_not_block_other_ids() {
    let f = Fixture::new(true).await;
    for index in (0..9).chain([0]) {
        f.bind(index).await.unwrap();
    }
    assert_eq!(f.state().fetched.len(), 10);
    let started = Arc::new(tokio::sync::Notify::new());
    f.state().started = started.clone();
    f.state().stalled = Some(url_secret(9));
    tokio::select! {
        result = f.bind(9) => panic!("stalled fetch completed: {result:?}"),
        result = async {
            tokio::time::timeout(Duration::from_secs(1), started.notified()).await.unwrap();
            tokio::time::timeout(Duration::from_secs(1), f.bind(10)).await.expect("independent fetch")
        } => assert!(result.is_ok()),
    }
    assert_eq!(f.state().fetched.len(), 12);
}

#[tokio::test]
async fn privatemode_active_secret_non_success_responses_preserve_status_and_retry_after() {
    for upstream_status in [
        StatusCode::BAD_REQUEST,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        let mut outcomes = Vec::new();
        for enabled in [false, true] {
            let f = Fixture::new(enabled).await;
            f.state().retry_after = Some("5".to_string());
            f.state().statuses.extend([StatusCode::OK, upstream_status]);
            assert_eq!(f.send(request(false)).await.0, StatusCode::OK);
            let body = serde_json::to_vec(&request(false)).unwrap();
            let result = f
                .service
                .forward_chat_completion(&body, None, true, None)
                .await
                .unwrap();
            assert_eq!(result.upstream_headers["retry-after"], "5");
            if enabled {
                assert!(receipt_event(&result.receipt, EVENT_UPSTREAM_VERIFIED)
                    .get("session_id")
                    .is_none());
            }
            assert_eq!(f.state().fetched.len(), usize::from(enabled));
            assert_eq!(f.state().forwarded.len(), 2);
            outcomes.push((result.upstream_status, result.upstream_body));
        }
        assert_eq!(outcomes[0], outcomes[1]);
        assert_eq!(outcomes[0].0, upstream_status.as_u16());
    }
}

#[tokio::test]
async fn privatemode_active_secret_middleware_preserves_capacity_retry_and_failover() {
    for streaming in [false, true] {
        for first_status in [
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            let f = Fixture::new(true).await;
            f.state().statuses.extend([first_status, StatusCode::OK]);
            f.state().retry_after = Some("5".to_string());
            let body = serde_json::to_vec(&request(streaming)).unwrap();
            let started = tokio::time::Instant::now();
            let count = if first_status == StatusCode::TOO_MANY_REQUESTS {
                1
            } else {
                2
            };
            let (result, _) =
                forward_privatemode_candidates(&f.service, &body, streaming, count).await;
            let (session_id, failed) = match result {
                MiddlewareForwardResult::Forwarded(r) => (r.session_id, r.failed_attempts),
                MiddlewareForwardResult::Stream(mut r) => {
                    while let Some(chunk) = r.body.next().await {
                        chunk.unwrap();
                    }
                    (r.session_id, r.failed_attempts)
                }
                _ => panic!("ordinary retry/failover must serve the next response"),
            };
            if first_status == StatusCode::TOO_MANY_REQUESTS {
                assert!(started.elapsed() >= Duration::from_secs(5));
            }
            assert_eq!(failed.len(), 1);
            assert_eq!(failed[0].status, first_status.as_u16());
            assert_eq!(f.state().forwarded.len(), 2);
            assert_eq!(f.state().fetched, [url_secret(1)]);
            assert_eq!(
                f.service
                    .get_attested_session(&session_id.unwrap())
                    .unwrap()
                    .document()
                    .claims
                    .extra["secret_id"],
                secret(1)
            );
        }
    }
}
