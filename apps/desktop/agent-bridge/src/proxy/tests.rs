use super::*;
use crate::tokens::LOCAL_TOOLS_AGENT;

struct HttpService {
    base_url: String,
    client: reqwest::Client,
}

impl VerifiedService for HttpService {
    fn call(
        self: Arc<Self>,
        request: Request<Body>,
        _context: Option<ForwardContext>,
    ) -> VerifiedResponse {
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let bytes = to_bytes(body, MAX_BODY_BYTES).await.unwrap();
            let mut upstream = self
                .client
                .request(parts.method, format!("{}{}", self.base_url, parts.uri));
            for (name, value) in &parts.headers {
                upstream = upstream.header(name, value);
            }
            match upstream.body(bytes).send().await {
                Ok(response) => {
                    let status = response.status();
                    let headers = response.headers().clone();
                    let mut builder = Response::builder().status(status);
                    for (name, value) in headers {
                        if let Some(name) = name {
                            builder = builder.header(name, value);
                        }
                    }
                    builder
                        .body(Body::from_stream(response.bytes_stream()))
                        .unwrap()
                }
                Err(_) => Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(Body::empty())
                    .unwrap(),
            }
        })
    }
}

fn http_service(base_url: &str) -> Arc<dyn VerifiedService> {
    Arc::new(HttpService {
        base_url: base_url.to_string(),
        client: reqwest::Client::builder().no_proxy().build().unwrap(),
    })
}

async fn spawn(router: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{addr}")
}

/// A stand-in verified upstream that echoes what it received.
async fn mock_upstream() -> String {
    let echo = |uri: Uri, headers: HeaderMap, body: Bytes| async move {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string()
        };
        (
            [("x-receipt-id", "rcpt-1")],
            Json(json!({
                "authorization": header("authorization"),
                "x-api-key": header("x-api-key"),
                "anthropic-beta": header("anthropic-beta"),
                "x-aci-tag": header("x-aci-tag"),
                "proxy-connection": header("proxy-connection"),
                "content-type": header("content-type"),
                "x-aci-provider": header("x-aci-provider"),
                "path": uri.to_string(),
                "body": String::from_utf8_lossy(&body),
                "body_bytes": body.as_ref(),
            })),
        )
    };
    let app = Router::new()
        .route("/v1/chat/completions", post(echo))
        .route("/v1/responses", post(echo))
        .route("/v1/messages/count_tokens", post(echo))
        .route("/v1/systemone", post(echo))
        .route("/v1/images/generations", post(echo))
        .route("/v1/audio/transcriptions", post(echo))
        .route(
            "/v1/models",
            get(|| async {
                Json(json!({
                    "data": [{ "id": "openai/gpt-oss-20b" }]
                }))
            }),
        );
    spawn(app).await
}

fn state() -> (Arc<ProxyState>, mpsc::Receiver<ProxyEvent>) {
    let (sender, receiver) = mpsc::channel(4);
    (ProxyState::new(sender).unwrap(), receiver)
}

fn tokens() -> TokenSet {
    let mut set = TokenSet::default();
    set.insert("codex-token".to_string(), "codex".to_string());
    set.insert("claude-token".to_string(), "claude-code".to_string());
    set.insert("opencode-token".to_string(), "opencode".to_string());
    set.insert(
        "local-tools-token".to_string(),
        LOCAL_TOOLS_AGENT.to_string(),
    );
    set
}

#[tokio::test]
async fn discovery_and_request_admission_share_endpoint_capabilities() {
    let (state, _events) = state();
    state.set_tokens(tokens());
    let mut catalog = Catalog::from_remote(
        &json!({"data": [
            {"id": "responses-only"}, {"id": "messages-only"}, {"id": "chat-only"}
        ]}),
        1,
    )
    .unwrap();
    for (model, surface) in catalog.models.iter_mut().zip([
        Surface::Responses,
        Surface::Messages,
        Surface::ChatCompletions,
    ]) {
        model.supported_surfaces = Some(vec![surface]);
    }
    state.publish(Session {
        verified: true,
        catalog: Some(catalog),
        ..Session::default()
    });
    for (token, expected) in [
        ("codex-token", "responses-only"),
        ("claude-token", "messages-only"),
        ("opencode-token", "chat-only"),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        let response = models(State(state.clone()), headers).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["data"].as_array().unwrap().len(), 1);
        assert_eq!(body["data"][0]["id"], expected);
    }
    assert!(check_catalog(&state, Some("messages-only"), Surface::Messages).is_ok());
    let error = check_catalog(&state, Some("messages-only"), Surface::Responses).unwrap_err();
    assert_eq!(error.code, "model_endpoint_unavailable");
}

async fn verified(state: &ProxyState, upstream: &str, generation: u64, epoch: u64) {
    state.publish(Session {
        generation,
        epoch,
        session_id: Some("test-session".to_string()),
        service: Some(http_service(upstream)),
        verified: false,
        catalog: None,
    });
    let catalog = state.fetch_catalog(generation, epoch).await.unwrap();
    state.publish(Session {
        generation,
        epoch,
        session_id: Some("test-session".to_string()),
        service: Some(http_service(upstream)),
        verified: true,
        catalog: Some(catalog),
    });
}

#[test]
fn connection_named_headers_are_hop_by_hop() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "connection",
        HeaderValue::from_static("close, X-Secret-Hop"),
    );
    headers.insert("x-secret-hop", HeaderValue::from_static("1"));
    let dropped = hop_by_hop_names(&headers);
    assert!(dropped.contains("x-secret-hop"));
    assert!(dropped.contains("proxy-connection"));
    assert!(dropped.contains("transfer-encoding"));
    assert!(!dropped.contains("anthropic-beta"));
}

#[test]
fn a_squatted_port_is_refused_before_anything_starts() {
    // Other tests in this binary spawn child processes. On macOS std marks a
    // new socket close-on-exec in a second call after creating it
    // (rust-lang/rust#24237), so a child spawned in between inherits the
    // squatter and keeps its port bound after it is dropped here. The rebind
    // assertion therefore runs in a process of its own.
    const CHILD: &str = "PAP_TEST_SQUATTED_PORT";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "proxy::tests::a_squatted_port_is_refused_before_anything_starts",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = squatter.local_addr().unwrap();
    let error = bind_std(addr).unwrap_err();
    assert!(error.contains("Cannot listen"));
    drop(squatter);
    assert!(bind_std(addr).is_ok());
}

#[tokio::test]
async fn anonymous_wrong_and_cross_agent_tokens_are_refused() {
    let (state, mut events) = state();
    state.set_tokens(tokens());
    let proxy = spawn(router(state)).await;
    let client = reqwest::Client::new();
    let anonymous = client
        .post(format!("{proxy}/v1/chat/completions"))
        .json(&json!({ "model": "openai/gpt-oss-20b" }))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status().as_u16(), 401);
    assert!(anonymous.headers().contains_key("www-authenticate"));
    let body: Value = anonymous.json().await.unwrap();
    assert_eq!(body["error"]["code"], "unauthorized");
    assert_eq!(
        body["error"]["message"],
        format!("Missing API key. Use the Local API key shown in {PRODUCT_NAME}.")
    );
    let wrong = client
        .post(format!("{proxy}/v1/messages"))
        .header("x-api-key", "guess")
        .json(&json!({ "model": "openai/gpt-oss-20b" }))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status().as_u16(), 401);
    let body: Value = wrong.json().await.unwrap();
    assert_eq!(body["type"], json!("error"));
    let anonymous_count = client
        .post(format!("{proxy}/v1/messages/count_tokens"))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous_count.status().as_u16(), 401);
    for path in [
        "/v1/messages",
        "/v1/messages/count_tokens",
        "/v1/chat/completions",
    ] {
        let cross = client
            .post(format!("{proxy}{path}"))
            .bearer_auth("codex-token")
            .json(&json!({ "model": "openai/gpt-oss-20b" }))
            .send()
            .await
            .unwrap();
        assert_eq!(cross.status().as_u16(), 403, "{path}");
    }
    assert_eq!(events.recv().await.unwrap().status, 401);
}

#[tokio::test]
async fn requests_fail_closed_until_a_verified_session_with_a_catalog_and_key() {
    let (state, _events) = state();
    state.set_tokens(tokens());
    let upstream = mock_upstream().await;
    let proxy = spawn(router(state.clone())).await;
    let client = reqwest::Client::new();
    let send = |client: reqwest::Client, proxy: String| async move {
        client
            .post(format!("{proxy}/v1/chat/completions"))
            .bearer_auth("opencode-token")
            .json(&json!({ "model": "openai/gpt-oss-20b", "messages": [] }))
            .send()
            .await
            .unwrap()
    };
    assert_eq!(
        send(client.clone(), proxy.clone()).await.status().as_u16(),
        503
    );

    state.publish(Session {
        generation: 1,
        epoch: 1,
        session_id: Some("test-session".to_string()),
        service: Some(http_service(&upstream)),
        verified: true,
        catalog: None,
    });
    assert_eq!(
        send(client.clone(), proxy.clone()).await.status().as_u16(),
        503
    );

    verified(&state, &upstream, 1, 1).await;
    let response = send(client.clone(), proxy.clone()).await;
    assert_eq!(response.status().as_u16(), 503);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], json!("api_key_missing"));

    // A new epoch (identity update) or generation (restart) revokes the
    // session, and a fetch started under the old epoch is refused.
    state.set_api_key(Some("sk-real".to_string()));
    state.publish(Session {
        generation: 1,
        epoch: 2,
        session_id: Some("test-session".to_string()),
        service: Some(http_service(&upstream)),
        verified: false,
        catalog: None,
    });
    assert_eq!(
        send(client.clone(), proxy.clone()).await.status().as_u16(),
        503
    );
    assert!(
        state.fetch_catalog(1, 1).await.is_err(),
        "stale epoch refused"
    );
    assert!(state.fetch_catalog(1, 2).await.is_ok());
}

#[tokio::test]
async fn verified_catalog_models_are_forwarded_with_the_real_key() {
    let (state, mut events) = state();
    state.set_tokens(tokens());
    let upstream = mock_upstream().await;
    verified(&state, &upstream, 1, 1).await;
    state.set_api_key(Some("sk-real".to_string()));
    let proxy = spawn(router(state.clone())).await;
    let client = reqwest::Client::new();

    let unknown = client
        .post(format!("{proxy}/v1/chat/completions"))
        .bearer_auth("opencode-token")
        .json(&json!({ "model": "gpt-5", "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status().as_u16(), 404);
    assert_eq!(events.recv().await.unwrap().model.as_deref(), Some("gpt-5"));

    let responses = client
        .post(format!("{proxy}/v1/responses"))
        .bearer_auth("codex-token")
        .json(&json!({ "model": "openai/gpt-oss-20b", "input": "hi" }))
        .send()
        .await
        .unwrap();
    assert_eq!(responses.status().as_u16(), 200);

    let forwarded = client
        .post(format!("{proxy}/v1/chat/completions"))
        .bearer_auth("opencode-token")
        .header("anthropic-beta", "keep-me")
        .header("x-aci-tag", "client-controlled")
        .header("proxy-connection", "keep-alive")
        .json(&json!({ "model": "openai/gpt-oss-20b", "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(forwarded.status().as_u16(), 200);
    assert_eq!(forwarded.headers()["x-receipt-id"], "rcpt-1");
    let echo: Value = forwarded.json().await.unwrap();
    assert_eq!(echo["authorization"], json!("Bearer sk-real"));
    assert_eq!(echo["x-api-key"], json!(""));
    assert_eq!(echo["anthropic-beta"], json!("keep-me"));
    assert_eq!(echo["x-aci-tag"], json!(""));
    assert_eq!(echo["proxy-connection"], json!(""));

    let counted = client
        .post(format!("{proxy}/v1/messages/count_tokens"))
        .bearer_auth("claude-token")
        .json(&json!({ "model": "openai/gpt-oss-20b" }))
        .send()
        .await
        .unwrap();
    assert_eq!(counted.status().as_u16(), 200);
    let echo: Value = counted.json().await.unwrap();
    assert_eq!(echo["authorization"], json!("Bearer sk-real"));
}

#[tokio::test]
async fn systemone_is_local_tools_only_and_uses_catalog_membership() {
    let (state, _events) = state();
    state.set_tokens(tokens());
    let upstream = mock_upstream().await;
    verified(&state, &upstream, 1, 1).await;
    state.set_api_key(Some("sk-real".to_string()));

    // Endpoint inventory observations cover only the three agent surfaces;
    // System One still admits any model present in the verified catalog.
    let mut session = state.session();
    session.catalog.as_mut().unwrap().models[0].supported_surfaces = Some(vec![Surface::Responses]);
    session.catalog.as_mut().unwrap().models[0]
        .remote
        .extra
        .insert("output_modalities".to_string(), json!(["decisions"]));
    state.publish(session);

    let proxy = spawn(router(state.clone())).await;
    let client = reqwest::Client::new();
    let body = r#"{"model":"openai/gpt-oss-20b","state":{"step":1},"questions":[]}"#;
    let response = client
        .post(format!("{proxy}/v1/systemone"))
        .bearer_auth("local-tools-token")
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let forwarded: Value = response.json().await.unwrap();
    assert_eq!(forwarded["authorization"], json!("Bearer sk-real"));
    assert_eq!(forwarded["path"], "/v1/systemone");
    assert_eq!(forwarded["body"], body);

    let mut session = state.session();
    session.catalog.as_mut().unwrap().models[0]
        .remote
        .extra
        .insert("output_modalities".to_string(), json!(["text"]));
    state.publish(session);
    let chat_model = client
        .post(format!("{proxy}/v1/systemone"))
        .bearer_auth("local-tools-token")
        .json(&json!({"model":"openai/gpt-oss-20b"}))
        .send()
        .await
        .unwrap();
    assert_eq!(chat_model.status(), StatusCode::BAD_REQUEST);
    let error: Value = chat_model.json().await.unwrap();
    assert_eq!(error["error"]["code"], "model_endpoint_unavailable");

    let coding_agent = client
        .post(format!("{proxy}/v1/systemone"))
        .bearer_auth("codex-token")
        .json(&json!({"model":"openai/gpt-oss-20b"}))
        .send()
        .await
        .unwrap();
    assert_eq!(coding_agent.status(), StatusCode::FORBIDDEN);
    let error: Value = coding_agent.json().await.unwrap();
    assert_eq!(error["error"]["code"], "forbidden");

    let unknown = client
        .post(format!("{proxy}/v1/systemone"))
        .bearer_auth("local-tools-token")
        .json(&json!({"model":"unknown/model","state":{},"questions":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let error: Value = unknown.json().await.unwrap();
    assert_eq!(error["error"]["code"], "model_not_found");
}

#[tokio::test]
async fn images_are_local_tools_only_and_require_advertised_image_output() {
    let (state, mut events) = state();
    state.set_tokens(tokens());
    let upstream = mock_upstream().await;
    verified(&state, &upstream, 1, 1).await;
    state.set_api_key(Some("sk-real".to_string()));
    let mut session = state.session();
    let model = &mut session.catalog.as_mut().unwrap().models[0];
    model.supported_surfaces = Some(vec![Surface::Responses]);
    model
        .remote
        .extra
        .insert("output_modalities".into(), json!(["image"]));
    state.publish(session);

    let proxy = spawn(router(state.clone())).await;
    let client = reqwest::Client::new();
    let body =
        r#"{"model":"openai/gpt-oss-20b", "prompt":"A quiet lake", "response_format":"b64_json"}"#;
    let response = client
        .post(format!("{proxy}/v1/images/generations?trace=true"))
        .bearer_auth("local-tools-token")
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-receipt-id"], "rcpt-1");
    let forwarded: Value = response.json().await.unwrap();
    assert_eq!(forwarded["authorization"], "Bearer sk-real");
    assert_eq!(forwarded["path"], "/v1/images/generations?trace=true");
    assert_eq!(forwarded["body"], body);
    let initial = events.recv().await.unwrap();
    let usage = events.recv().await.unwrap();
    assert_eq!(usage.request_id, initial.request_id);
    assert_eq!(usage.input_tokens, None);
    assert_eq!(usage.output_tokens, None);
    assert_eq!(usage.cost_usd, None);

    for token in ["codex-token", "claude-token", "opencode-token"] {
        let response = client
            .post(format!("{proxy}/v1/images/generations"))
            .bearer_auth(token)
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["error"]["code"], "forbidden");
    }
    for modalities in [Some(json!(["text"])), None] {
        let mut session = state.session();
        let extra = &mut session.catalog.as_mut().unwrap().models[0].remote.extra;
        if let Some(modalities) = modalities {
            extra.insert("output_modalities".into(), modalities);
        } else {
            extra.remove("output_modalities");
        }
        state.publish(session);
        let response = client
            .post(format!("{proxy}/v1/images/generations"))
            .bearer_auth("local-tools-token")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["error"]["code"], "model_endpoint_unavailable");
    }
    let unknown = client
        .post(format!("{proxy}/v1/images/generations"))
        .bearer_auth("local-tools-token")
        .json(&json!({"model":"unknown/model", "prompt":"A quiet lake"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let error: Value = unknown.json().await.unwrap();
    assert_eq!(error["error"]["code"], "model_not_found");
}

#[tokio::test]
async fn transcriptions_gate_tokens_and_models_and_preserve_multipart_bytes_and_carrier() {
    let (state, mut events) = state();
    state.set_tokens(tokens());
    let upstream = mock_upstream().await;
    verified(&state, &upstream, 1, 1).await;
    state.set_api_key(Some("sk-real".to_string()));
    let mut session = state.session();
    let model = &mut session.catalog.as_mut().unwrap().models[0];
    model.supported_surfaces = Some(vec![]);
    model
        .remote
        .extra
        .insert("input_modalities".into(), json!(["audio"]));
    model
        .remote
        .extra
        .insert("output_modalities".into(), json!(["text"]));
    state.publish(session);
    let proxy = spawn(router(state.clone())).await;
    let client = reqwest::Client::new();
    // Put the binary file first to exercise draining before model extraction.
    let mut body = b"--audio-boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"clip.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF\x00\xff\x80\r\n".to_vec();
    body.extend_from_slice(b"--audio-boundary\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nopenai/gpt-oss-20b\r\n--audio-boundary--\r\n");
    let content_type = "multipart/form-data; boundary=\"audio-boundary\"";
    let carrier = r#"{"aci_verified":true,"aci_session_ids":["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]}"#;
    let response = client
        .post(format!("{proxy}/v1/audio/transcriptions?trace=true"))
        .bearer_auth("local-tools-token")
        .header("content-type", content_type)
        .header("x-aci-provider", carrier)
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-receipt-id"], "rcpt-1");
    let forwarded: Value = response.json().await.unwrap();
    assert_eq!(forwarded["path"], "/v1/audio/transcriptions?trace=true");
    assert_eq!(forwarded["authorization"], "Bearer sk-real");
    assert_eq!(forwarded["content-type"], content_type);
    assert_eq!(forwarded["x-aci-provider"], carrier);
    assert_eq!(forwarded["body_bytes"], json!(body));
    let initial = events.recv().await.unwrap();
    assert_eq!(initial.model.as_deref(), Some("openai/gpt-oss-20b"));
    assert!(initial.left_device);
    let _usage = events.recv().await.unwrap();

    for token in ["codex-token", "claude-token", "opencode-token"] {
        let response = client
            .post(format!("{proxy}/v1/audio/transcriptions"))
            .bearer_auth(token)
            .header("content-type", content_type)
            .body(body.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!events.recv().await.unwrap().left_device);
    }
    // Reject a missing or incompatible modality even when the model is listed.
    for (input, output) in [
        (Some(json!(["text"])), Some(json!(["text"]))),
        (Some(json!(["audio", "text"])), Some(json!(["text"]))),
        (None, Some(json!(["text"]))),
        (Some(json!(["audio"])), None),
        (Some(json!(["audio"])), Some(json!(["image"]))),
    ] {
        let mut session = state.session();
        let extra = &mut session.catalog.as_mut().unwrap().models[0].remote.extra;
        for (key, value) in [("input_modalities", input), ("output_modalities", output)] {
            match value {
                Some(value) => {
                    extra.insert(key.into(), value);
                }
                None => {
                    extra.remove(key);
                }
            }
        }
        state.publish(session);
        let response = client
            .post(format!("{proxy}/v1/audio/transcriptions"))
            .bearer_auth("local-tools-token")
            .header("content-type", content_type)
            .body(body.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["error"]["code"], "model_endpoint_unavailable");
        assert!(!events.recv().await.unwrap().left_device);
    }

    for connection in ["X-ACI-Provider", "Content-Type"] {
        let response = client
            .post(format!("{proxy}/v1/audio/transcriptions"))
            .bearer_auth("local-tools-token")
            .header("content-type", content_type)
            .header("x-aci-provider", carrier)
            .header("connection", connection)
            .body(body.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!events.recv().await.unwrap().left_device);
    }
    for path in [
        "/v1/chat/completions",
        "/v1/messages",
        "/v1/messages/count_tokens",
        "/v1/responses",
        "/v1/responses/compact",
        "/v1/systemone",
        "/v1/images/generations",
    ] {
        let response = client
            .post(format!("{proxy}{path}"))
            .bearer_auth("local-tools-token")
            .header("content-type", content_type)
            .body(body.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        assert!(!events.recv().await.unwrap().left_device);
    }
}

#[tokio::test]
async fn large_image_responses_pass_through_and_record_usage() {
    let (state, mut events) = state();
    state.set_tokens(tokens());
    let payload = Bytes::from(format!(
        r#"{{"created":1,"data":[{{"b64_json":"{}"}}],"usage":{{"input_tokens":12,"output_tokens":34,"cost":"0.02"}}}}"#,
        "A".repeat(MAX_USAGE_CAPTURE_BYTES + 1024)
    ));
    let expected = payload.clone();
    let upstream = spawn(
        Router::new()
            .route(
                "/v1/images/generations",
                post(move || {
                    let payload = payload.clone();
                    async move {
                        Response::builder()
                            .status(StatusCode::CREATED)
                            .header(header::CONTENT_TYPE, "application/json")
                            .header("x-receipt-id", "image-receipt")
                            .body(Body::from(payload))
                            .unwrap()
                    }
                }),
            )
            .route(
                "/v1/models",
                get(|| async {
                    Json(json!({"data":[{"id":"image-model", "output_modalities":["image"]}]}))
                }),
            ),
    )
    .await;
    verified(&state, &upstream, 1, 1).await;
    state.set_api_key(Some("sk-real".to_string()));
    let proxy = spawn(router(state)).await;
    let response = reqwest::Client::new()
        .post(format!("{proxy}/v1/images/generations"))
        .bearer_auth("local-tools-token")
        .json(&json!({"model":"image-model", "prompt":"A quiet lake"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()["content-type"], "application/json");
    assert_eq!(response.headers()["x-receipt-id"], "image-receipt");
    assert_eq!(response.bytes().await.unwrap(), expected);
    let initial = events.recv().await.unwrap();
    let usage = events.recv().await.unwrap();
    assert_eq!(usage.request_id, initial.request_id);
    assert_eq!(usage.path, "/v1/images/generations");
    assert_eq!(usage.agent.as_deref(), Some(LOCAL_TOOLS_AGENT));
    assert_eq!(usage.receipt_id.as_deref(), Some("image-receipt"));
    assert!(!usage.streamed);
    assert!(usage.left_device);
    assert_eq!(usage.input_tokens, Some(12));
    assert_eq!(usage.output_tokens, Some(34));
    assert_eq!(usage.cost_usd, Some(0.02));
}

#[tokio::test]
async fn send_failures_are_not_reported_as_local_rejections() {
    let (state, mut events) = state();
    state.set_tokens(tokens());
    let upstream = mock_upstream().await;
    verified(&state, &upstream, 1, 1).await;
    state.set_api_key(Some("sk-real".to_string()));

    // Drop every connection unanswered. A closed port would do on Unix, but Windows
    // takes seconds to refuse a loopback connection, past the test call timeout.
    let unavailable = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unavailable_url = format!("http://{}", unavailable.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((connection, _)) = unavailable.accept().await {
            drop(connection);
        }
    });
    state.publish(Session {
        service: Some(http_service(&unavailable_url)),
        ..state.session()
    });

    let proxy = spawn(router(state)).await;
    let response = reqwest::Client::new()
        .post(format!("{proxy}/v1/responses"))
        .bearer_auth("codex-token")
        .json(&json!({ "model": "openai/gpt-oss-20b", "input": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 502);
    let event = events.recv().await.unwrap();
    assert!(event.left_device);
    assert_eq!(event.session_id, "test-session");
    assert_eq!(event.agent.as_deref(), Some("codex"));
    assert_eq!(event.model.as_deref(), Some("openai/gpt-oss-20b"));
}

struct PanicService;

impl VerifiedService for PanicService {
    fn call(
        self: Arc<Self>,
        _request: Request<Body>,
        _context: Option<ForwardContext>,
    ) -> VerifiedResponse {
        panic!("synthetic verifier panic")
    }
}

struct HangingService;

impl VerifiedService for HangingService {
    fn call(
        self: Arc<Self>,
        _request: Request<Body>,
        _context: Option<ForwardContext>,
    ) -> VerifiedResponse {
        Box::pin(std::future::pending())
    }
}

async fn verifier_failure(service: Arc<dyn VerifiedService>) -> (Response, ProxyEvent) {
    let (state, mut events) = state();
    let response = forward(
        state,
        service,
        7,
        "test-session",
        "test-key",
        "codex",
        Surface::Responses,
        "/v1/responses",
        PathAndQuery::from_static("/v1/responses"),
        &HeaderMap::new(),
        Bytes::new(),
        Some("test-model".into()),
        CancellationToken::new(),
    )
    .await;
    (response, events.recv().await.unwrap())
}

#[tokio::test]
async fn verifier_panic_returns_502_and_records_activity() {
    let (response, event) = verifier_failure(Arc::new(PanicService)).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], "upstream_unreachable");
    assert_eq!(event.status, 502);
    assert!(event.left_device);
    assert_eq!(event.generation, 7);
}

#[tokio::test]
async fn verifier_hang_returns_504_and_records_activity() {
    let (response, event) = verifier_failure(Arc::new(HangingService)).await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], "upstream_timeout");
    assert_eq!(event.status, 504);
    assert!(event.left_device);
}

/// The proxy relays: for every inference path the upstream sees the same
/// method, path, query, and body bytes the agent sent, and the agent gets
/// the upstream's status, content type, and streamed bytes back unchanged.
#[tokio::test]
async fn every_path_is_relayed_without_rewriting_request_or_response() {
    let (state, _events) = state();
    state.set_tokens(tokens());
    let echo = |method: axum::http::Method,
                uri: axum::http::Uri,
                _headers: HeaderMap,
                body: Bytes| async move {
        let body = String::from_utf8_lossy(&body);
        (
            StatusCode::ACCEPTED,
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("event: echo\ndata: {method} {uri}\n\ndata: {body}\n\n"),
        )
    };
    let upstream = spawn(
        Router::new()
            .route("/v1/chat/completions", post(echo))
            .route("/v1/messages", post(echo))
            .route("/v1/responses", post(echo))
            .route(
                "/v1/models",
                get(|| async { Json(json!({ "data": [{ "id": "openai/gpt-oss-20b" }] })) }),
            ),
    )
    .await;
    verified(&state, &upstream, 1, 1).await;
    state.set_api_key(Some("sk-real".to_string()));
    let proxy = spawn(router(state.clone())).await;
    let client = reqwest::Client::new();
    // Field order, whitespace, and unknown members are the agent's; the
    // proxy only reads `model`.
    let body =
        r#"{"stream": true, "model":"openai/gpt-oss-20b", "input": [{"x": 1}], "extra": null}"#;
    for (path, token) in [
        ("/v1/chat/completions", "opencode-token"),
        ("/v1/messages", "claude-token"),
        ("/v1/responses", "codex-token"),
    ] {
        let response = client
            .post(format!("{proxy}{path}?beta=true&v=2"))
            .bearer_auth(token)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 202, "{path}");
        assert_eq!(
            response.headers()["content-type"],
            "text/event-stream",
            "{path}"
        );
        let text = response.text().await.unwrap();
        assert!(
            text.starts_with(&format!("event: echo\ndata: POST {path}?beta=true&v=2")),
            "{path}: {text}"
        );
        assert!(
            text.contains(&format!("\n\ndata: {body}\n\n")),
            "{path}: {text}"
        );
    }
}

#[test]
fn usage_capture_parses_streamed_and_json_usage_without_inventing_values() {
    let mut stream = UsageCapture::new(true, Surface::Responses);
    stream.push(b"event: response.completed\ndata: {\"response\":{\"usage\":{\"input_tokens\":12,");
    stream.push(
        b"\"output_tokens\":5,\"input_tokens_details\":{\"cached_tokens\":3},\"cost\":\"0.004\"}}}\n\n",
    );
    let usage = stream.finish();
    assert_eq!(usage.input_tokens, Some(12));
    assert_eq!(usage.output_tokens, Some(5));
    assert_eq!(usage.cache_read_tokens, Some(3));
    assert_eq!(usage.cost_usd, Some(0.004));

    let mut json = UsageCapture::new(false, Surface::ChatCompletions);
    json.push(br#"{"usage":{"prompt_tokens":21,"completion_tokens":8,"cache_creation_input_tokens":4,"prompt_tokens_details":{"cached_tokens":7},"cost":-1}}"#);
    let usage = json.finish();
    assert_eq!(usage.input_tokens, Some(21));
    assert_eq!(usage.output_tokens, Some(8));
    assert_eq!(usage.cache_read_tokens, Some(7));
    assert_eq!(usage.cache_write_tokens, Some(4));
    assert_eq!(usage.cost_usd, None);

    let mut messages = UsageCapture::new(true, Surface::Messages);
    messages.push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":144,\"cache_read_input_tokens\":32,\"cache_creation_input_tokens\":8}}}\n\n");
    messages.push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":21}}\n\n");
    let usage = messages.finish();
    assert_eq!(usage.input_tokens, Some(144));
    assert_eq!(usage.output_tokens, Some(21));
    assert_eq!(usage.cache_read_tokens, Some(32));
    assert_eq!(usage.cache_write_tokens, Some(8));
}

#[test]
fn oversized_usage_body_stays_unknown() {
    let mut capture = UsageCapture::new(false, Surface::Responses);
    capture.push(&vec![b'x'; MAX_USAGE_CAPTURE_BYTES + 1]);
    let usage = capture.finish();
    assert_eq!(usage.input_tokens, None);
    assert_eq!(usage.output_tokens, None);
    assert_eq!(usage.cost_usd, None);
}

#[test]
fn image_usage_capture_accepts_the_response_limit_and_discards_overflow() {
    let prefix = br#"{"data":[{"b64_json":""#;
    let suffix = br#""}],"usage":{"cost":"0.03"}}"#;
    let chunk = [b'A'; 64 * 1024];
    for overflow in [false, true] {
        let mut capture = UsageCapture::new(false, Surface::Images);
        capture.push(prefix);
        let mut remaining = MAX_IMAGE_USAGE_CAPTURE_BYTES - prefix.len() - suffix.len();
        while remaining > 0 {
            let len = remaining.min(chunk.len());
            capture.push(&chunk[..len]);
            remaining -= len;
        }
        capture.push(suffix);
        if overflow {
            capture.push(b" ");
            assert_eq!(capture.body.capacity(), 0);
        }
        let usage = capture.finish();
        assert_eq!(usage.cost_usd, if overflow { None } else { Some(0.03) });
        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.output_tokens, None);
    }
}

#[test]
fn large_responses_preserve_usage_without_reading_output_as_usage() {
    let mut stream = UsageCapture::new(true, Surface::Responses);
    let event = format!(
        "data: {}\n\n",
        json!({
            "response": {"output": "x".repeat(256 * 1024),
                "usage": {"input_tokens": 1200, "output_tokens": 450}}
        })
    );
    for chunk in event.as_bytes().chunks(997) {
        stream.push(chunk);
    }
    let usage = stream.finish();
    assert_eq!(usage.input_tokens, Some(1200));
    assert_eq!(usage.output_tokens, Some(450));

    let mut body = UsageCapture::new(false, Surface::Responses);
    body.push(
        json!({"output": "x".repeat(2 * 1024 * 1024),
        "usage": {"input_tokens": 800, "output_tokens": 400}})
        .to_string()
        .as_bytes(),
    );
    assert_eq!(body.finish().output_tokens, Some(400));
    assert!(decode_usage(br#"{"output":{"usage":{"input_tokens":99}}}"#).is_none());
}

#[test]
fn oversized_sse_lines_are_discarded_before_the_next_event() {
    let mut stream = UsageCapture::new(true, Surface::Responses);
    stream.push(b"data: {\"usage\":{\"input_tokens\":999}}");
    stream.push(&vec![b' '; desktop_core::sse::MAX_LINE_BYTES]);
    stream.push(b"invalid\n\n");
    assert!(stream.latest.input_tokens.is_none());
    stream.push(b"data: {\"usage\":{\"input_tokens\":42}}\n\n");
    assert_eq!(stream.finish().input_tokens, Some(42));
}

#[tokio::test]
async fn usage_is_recorded_when_the_consumer_stops_before_eof() {
    let (state, mut events) = state();
    let payload = Bytes::from_static(
        b"data: {\"response\":{\"usage\":{\"input_tokens\":123,\"output_tokens\":45}}}\n\n",
    );
    let expected = payload.clone();
    let upstream = spawn(Router::new().route(
        "/v1/responses",
        post(move || {
            let payload = payload.clone();
            async move {
                let stream =
                    futures_util::stream::once(async move { Ok::<_, std::io::Error>(payload) })
                        .chain(futures_util::stream::pending());
                Response::builder()
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(stream))
                    .unwrap()
            }
        }),
    ))
    .await;
    let response = forward(
        state,
        http_service(&upstream),
        1,
        "test-session",
        "test-key",
        "codex",
        Surface::Responses,
        "/v1/responses",
        PathAndQuery::from_static("/v1/responses"),
        &HeaderMap::new(),
        Bytes::new(),
        Some("test-model".into()),
        CancellationToken::new(),
    )
    .await;
    let initial = events.recv().await.unwrap();
    let mut body = response.into_body().into_data_stream();
    let received = tokio::time::timeout(Duration::from_secs(3), async {
        let mut bytes = Vec::new();
        while bytes.len() < expected.len() {
            bytes.extend_from_slice(&body.next().await.unwrap().unwrap());
        }
        bytes
    })
    .await
    .unwrap();
    assert_eq!(received.as_slice(), expected.as_ref());
    drop(body);
    let recorded = events
        .try_recv()
        .expect("Dropping the response must publish captured usage");
    assert_eq!(recorded.request_id, initial.request_id);
    assert_eq!(recorded.session_id, initial.session_id);
    assert_eq!(recorded.input_tokens, Some(123));
    assert_eq!(recorded.output_tokens, Some(45));
    assert!(recorded.verified.is_none());
}

/// A token revoked (or a key removed) while the body is still arriving
/// must fail the request before it is sent upstream.
#[tokio::test]
async fn credentials_revoked_mid_body_fail_before_send() {
    let (state, _events) = state();
    state.set_tokens(tokens());
    let upstream = mock_upstream().await;
    verified(&state, &upstream, 1, 1).await;
    state.set_api_key(Some("sk-real".to_string()));
    let proxy = spawn(router(state.clone())).await;

    // Stream a body slowly: first chunk now, the rest after revocation.
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(2);
    tx.send(Ok(Bytes::from_static(
        b"{\"model\":\"openai/gpt-oss-20b\",",
    )))
    .await
    .unwrap();
    let revoke_state = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        revoke_state.set_tokens(revoke_state.tokens().without("opencode"));
        tx.send(Ok(Bytes::from_static(b"\"messages\":[]}")))
            .await
            .unwrap();
    });
    let response = reqwest::Client::new()
        .post(format!("{proxy}/v1/chat/completions"))
        .bearer_auth("opencode-token")
        .header("content-type", "application/json")
        .body(reqwest::Body::wrap_stream(tokio_stream_from_receiver(rx)))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 503);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], json!("credentials_changed"));
}

fn tokio_stream_from_receiver(
    mut rx: mpsc::Receiver<Result<Bytes, std::io::Error>>,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))
}

/// Helpers are gated like inference: unknown or missing model, and a
/// surface the service stopped declaring, are refused.
#[tokio::test]
async fn helper_endpoints_require_a_verified_catalog_model() {
    let (state, _events) = state();
    state.set_tokens(tokens());
    let upstream = mock_upstream().await;
    verified(&state, &upstream, 1, 1).await;
    state.set_api_key(Some("sk-real".to_string()));
    let proxy = spawn(router(state.clone())).await;
    let client = reqwest::Client::new();
    let count = |body: Value| {
        client
            .post(format!("{proxy}/v1/messages/count_tokens"))
            .bearer_auth("claude-token")
            .json(&body)
            .send()
    };
    assert_eq!(
        count(json!({ "model": "gpt-5" }))
            .await
            .unwrap()
            .status()
            .as_u16(),
        404
    );
    assert_eq!(
        count(json!({ "messages": [] }))
            .await
            .unwrap()
            .status()
            .as_u16(),
        400
    );
    assert_eq!(
        count(json!({ "model": "openai/gpt-oss-20b" }))
            .await
            .unwrap()
            .status()
            .as_u16(),
        200
    );
}

#[tokio::test]
async fn token_revocation_interrupts_a_stream_after_its_first_chunk() {
    let finish = Arc::new(tokio::sync::Notify::new());
    let upstream = spawn(
        Router::new()
            .route(
                "/v1/models",
                get(|| async { Json(json!({"data":[{"id":"model"}]})) }),
            )
            .route(
                "/v1/chat/completions",
                post(move || {
                    let finish = finish.clone();
                    async move {
                        Body::from_stream(async_stream::stream! {
                            yield Ok::<_, std::io::Error>(Bytes::from_static(b"first"));
                            finish.notified().await;
                            yield Ok::<_, std::io::Error>(Bytes::from_static(b"last"));
                        })
                    }
                }),
            ),
    )
    .await;
    let (state, mut events) = state();
    state.set_tokens(tokens());
    state.set_api_key(Some("test-key".into()));
    verified(&state, &upstream, 1, 1).await;
    let proxy = spawn(router(state.clone())).await;
    let response = reqwest::Client::new()
        .post(format!("{proxy}/v1/chat/completions"))
        .bearer_auth("opencode-token")
        .json(&json!({"model":"model"}))
        .send()
        .await
        .unwrap();
    let mut stream = response.bytes_stream();
    assert_eq!(stream.next().await.unwrap().unwrap().as_ref(), b"first");
    state.set_tokens(state.tokens().without("opencode"));
    let next = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap();
    assert!(next.is_none_or(|item| item.is_err()));
    drop(stream);
    let failure = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.status == 502 {
                break event;
            }
        }
    })
    .await
    .unwrap();
    assert!(failure.left_device);
    assert!(failure.detail.contains("revoked"));
}

#[tokio::test]
async fn agent_scan_and_catalog_refresh_preserve_admitted_requests() {
    let (state, _events) = state();
    state.set_tokens(tokens());
    let upstream = mock_upstream().await;
    verified(&state, &upstream, 1, 1).await;
    state.set_api_key(Some("sk-real".to_string()));
    let reached = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    *state.pause.lock().unwrap() = Some((reached.clone(), resume.clone()));
    let proxy = spawn(router(state.clone())).await;
    let request = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("{proxy}/v1/chat/completions"))
            .bearer_auth("opencode-token")
            .json(&json!({ "model": "openai/gpt-oss-20b", "messages": [] }))
            .send()
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(5), reached.notified())
        .await
        .unwrap();
    state.set_tokens(tokens());
    let mut session = state.session();
    session.catalog =
        Some(Catalog::from_remote(&json!({"data": [{"id": "replacement"}]}), 2).unwrap());
    state.publish(session);
    assert!(check_catalog(&state, Some("openai/gpt-oss-20b"), Surface::ChatCompletions).is_err());
    resume.notify_one();
    let response = tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// A revocation that lands after the final checks but before the send
/// must deliver nothing to the upstream.
#[tokio::test]
async fn revocation_after_the_final_check_delivers_nothing() {
    let revocations: [fn(&ProxyState); 4] = [
        |state| state.set_api_key(None),
        |state| state.set_tokens(state.tokens().without("opencode")),
        |state| {
            let mut session = state.session();
            session.epoch += 1;
            state.publish(session);
        },
        |state| {
            let mut session = state.session();
            session.verified = false;
            session.catalog = None;
            state.publish(session);
        },
    ];
    for revoke in revocations {
        let (state, _events) = state();
        state.set_tokens(tokens());
        let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = delivered.clone();
        let upstream = spawn(
            Router::new()
                .route(
                    "/v1/chat/completions",
                    post(move || {
                        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        async { Json(json!({})) }
                    }),
                )
                .route(
                    "/v1/models",
                    get(|| async {
                        Json(json!({
                            "data": [{ "id": "openai/gpt-oss-20b" }]
                        }))
                    }),
                ),
        )
        .await;
        verified(&state, &upstream, 1, 1).await;
        state.set_api_key(Some("sk-real".to_string()));
        let reached = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        *state.pause.lock().unwrap() = Some((reached.clone(), resume.clone()));
        let proxy = spawn(router(state.clone())).await;
        let request = tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{proxy}/v1/chat/completions"))
                .bearer_auth("opencode-token")
                .json(&json!({ "model": "openai/gpt-oss-20b", "messages": [] }))
                .send()
                .await
                .unwrap()
        });
        reached.notified().await;
        revoke(&state);
        resume.notify_one();
        let response = request.await.unwrap();
        assert_eq!(response.status().as_u16(), 503);
        assert_eq!(delivered.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
