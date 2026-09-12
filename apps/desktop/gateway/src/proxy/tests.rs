use super::*;

async fn spawn(router: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{addr}")
}

/// A stand-in sidecar that echoes what it received.
async fn mock_sidecar() -> String {
    let echo = |headers: HeaderMap, body: Bytes| async move {
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
                "tag": header(TAG_HEADER),
                "anthropic-beta": header("anthropic-beta"),
                "proxy-connection": header("proxy-connection"),
                "body": String::from_utf8_lossy(&body),
            })),
        )
    };
    let app = Router::new()
        .route("/v1/chat/completions", post(echo))
        .route("/v1/responses", post(echo))
        .route("/v1/messages/count_tokens", post(echo))
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
    set
}

#[test]
fn sidecar_requests_ignore_proxy_environment() {
    const CHILD: &str = "PAP_TEST_PROXY_ENV_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Isolate proxy variables from the other concurrently running tests.
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "proxy::tests::sidecar_requests_ignore_proxy_environment",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("HTTP_PROXY", "http://127.0.0.1:1")
            .env("http_proxy", "http://127.0.0.1:1")
            .env("ALL_PROXY", "http://127.0.0.1:1")
            .env("all_proxy", "http://127.0.0.1:1")
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (state, _events) = state();
            let sidecar = mock_sidecar().await;
            assert!(reqwest::Client::builder()
                .timeout(Duration::from_secs(1))
                .build()
                .unwrap()
                .get(format!("{sidecar}/v1/models"))
                .send()
                .await
                .is_err());
            verified(&state, &sidecar, 1, 1).await;
            let response = state
                .client
                .post(format!("{sidecar}/v1/responses"))
                .json(&json!({ "model": "openai/gpt-oss-20b", "input": "fixture" }))
                .send()
                .await
                .unwrap();
            assert!(response.status().is_success());
        });
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

async fn verified(state: &ProxyState, sidecar: &str, generation: u64, epoch: u64) {
    state.publish(Session {
        generation,
        epoch,
        session_id: Some("test-session".to_string()),
        base_url: Some(sidecar.to_string()),
        verified: false,
        catalog: None,
    });
    let catalog = state.fetch_catalog(generation, epoch).await.unwrap();
    state.publish(Session {
        generation,
        epoch,
        session_id: Some("test-session".to_string()),
        base_url: Some(sidecar.to_string()),
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
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = squatter.local_addr().unwrap();
    let error = bind_std(addr).unwrap_err();
    assert!(error.contains("Cannot listen"));
    drop(squatter);
    let rebound = (0..20).find_map(|_| match bind_std(addr) {
        Ok(listener) => Some(listener),
        Err(_) => {
            std::thread::sleep(Duration::from_millis(10));
            None
        }
    });
    assert!(rebound.is_some());
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
    let sidecar = mock_sidecar().await;
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
        base_url: Some(sidecar.clone()),
        verified: true,
        catalog: None,
    });
    assert_eq!(
        send(client.clone(), proxy.clone()).await.status().as_u16(),
        503
    );

    verified(&state, &sidecar, 1, 1).await;
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
        base_url: Some(sidecar.clone()),
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
    let sidecar = mock_sidecar().await;
    verified(&state, &sidecar, 1, 1).await;
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
    let tag = echo["tag"].as_str().unwrap();
    let parts: Vec<_> = tag.split(':').collect();
    assert_eq!(parts.len(), 4);
    assert_eq!(parts[0], "pap");
    assert_eq!(parts[2], "test-session");
    assert_eq!(parts[3], "opencode");
    assert_eq!(echo["anthropic-beta"], json!("keep-me"));
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
    let tag = echo["tag"].as_str().unwrap();
    assert!(tag.starts_with("pap:"));
    assert!(tag.ends_with(":test-session:claude-code"));
}

#[tokio::test]
async fn send_failures_are_not_reported_as_local_rejections() {
    let (state, mut events) = state();
    state.set_tokens(tokens());
    let sidecar = mock_sidecar().await;
    verified(&state, &sidecar, 1, 1).await;
    state.set_api_key(Some("sk-real".to_string()));

    let unavailable = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unavailable_url = format!("http://{}", unavailable.local_addr().unwrap());
    drop(unavailable);
    state.publish(Session {
        base_url: Some(unavailable_url),
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

/// The proxy relays: for every inference path the sidecar sees the same
/// method, path, query, and body bytes the agent sent, and the agent gets
/// the sidecar's status, content type, and streamed bytes back unchanged.
#[tokio::test]
async fn every_path_is_relayed_without_rewriting_request_or_response() {
    let (state, _events) = state();
    state.set_tokens(tokens());
    let echo = |method: axum::http::Method,
                uri: axum::http::Uri,
                headers: HeaderMap,
                body: Bytes| async move {
        let tag = headers
            .get(TAG_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = String::from_utf8_lossy(&body);
        (
            StatusCode::ACCEPTED,
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("event: echo\ndata: {method} {uri} {tag}\n\ndata: {body}\n\n"),
        )
    };
    let sidecar = spawn(
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
    verified(&state, &sidecar, 1, 1).await;
    state.set_api_key(Some("sk-real".to_string()));
    let proxy = spawn(router(state.clone())).await;
    let client = reqwest::Client::new();
    // Field order, whitespace, and unknown members are the agent's; the
    // proxy only reads `model`.
    let body =
        r#"{"stream": true, "model":"openai/gpt-oss-20b", "input": [{"x": 1}], "extra": null}"#;
    for (path, token, agent) in [
        ("/v1/chat/completions", "opencode-token", "opencode"),
        ("/v1/messages", "claude-token", "claude-code"),
        ("/v1/responses", "codex-token", "codex"),
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
            text.starts_with(&format!(
                "event: echo\ndata: POST {path}?beta=true&v=2 pap:"
            )),
            "{path}: {text}"
        );
        assert!(
            text.contains(&format!(":test-session:{agent}\n\ndata: {body}\n\n")),
            "{path}: {text}"
        );
    }
}

#[test]
fn usage_capture_parses_streamed_and_json_usage_without_inventing_values() {
    let mut stream = UsageCapture::new(true);
    stream.push(b"event: response.completed\ndata: {\"response\":{\"usage\":{\"input_tokens\":12,");
    stream.push(
        b"\"output_tokens\":5,\"input_tokens_details\":{\"cached_tokens\":3},\"cost\":\"0.004\"}}}\n\n",
    );
    let usage = stream.finish();
    assert_eq!(usage.input_tokens, Some(12));
    assert_eq!(usage.output_tokens, Some(5));
    assert_eq!(usage.cache_read_tokens, Some(3));
    assert_eq!(usage.cost_usd, Some(0.004));

    let mut json = UsageCapture::new(false);
    json.push(br#"{"usage":{"prompt_tokens":21,"completion_tokens":8,"cache_creation_input_tokens":4,"prompt_tokens_details":{"cached_tokens":7},"cost":-1}}"#);
    let usage = json.finish();
    assert_eq!(usage.input_tokens, Some(21));
    assert_eq!(usage.output_tokens, Some(8));
    assert_eq!(usage.cache_read_tokens, Some(7));
    assert_eq!(usage.cache_write_tokens, Some(4));
    assert_eq!(usage.cost_usd, None);

    let mut messages = UsageCapture::new(true);
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
    let mut capture = UsageCapture::new(false);
    capture.push(&vec![b'x'; MAX_USAGE_CAPTURE_BYTES + 1]);
    let usage = capture.finish();
    assert_eq!(usage.input_tokens, None);
    assert_eq!(usage.output_tokens, None);
    assert_eq!(usage.cost_usd, None);
}

#[test]
fn large_responses_preserve_usage_without_reading_output_as_usage() {
    let mut stream = UsageCapture::new(true);
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

    let mut body = UsageCapture::new(false);
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
    let mut stream = UsageCapture::new(true);
    stream.push(b"data: {\"usage\":{\"input_tokens\":999}}");
    stream.push(&vec![b' '; MAX_SSE_LINE_BYTES]);
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
    let sidecar = spawn(Router::new().route(
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
        &sidecar,
        "test-session",
        "test-key",
        "codex",
        Surface::Responses,
        "/v1/responses",
        None,
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

#[test]
fn attribution_tag_contains_request_session_and_agent() {
    assert_eq!(
        format_tag("request-1", "session-2", "pi"),
        "pap:request-1:session-2:pi"
    );
}

/// A token revoked (or a key removed) while the body is still arriving
/// must fail the request before it is sent upstream.
#[tokio::test]
async fn credentials_revoked_mid_body_fail_before_send() {
    let (state, _events) = state();
    state.set_tokens(tokens());
    let sidecar = mock_sidecar().await;
    verified(&state, &sidecar, 1, 1).await;
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
    let sidecar = mock_sidecar().await;
    verified(&state, &sidecar, 1, 1).await;
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
async fn unchanged_agent_scan_does_not_cancel_an_admitted_request() {
    let (state, _events) = state();
    state.set_tokens(tokens());
    let sidecar = mock_sidecar().await;
    verified(&state, &sidecar, 1, 1).await;
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
    resume.notify_one();
    let response = tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// A revocation that lands after the final checks but before the send
/// must deliver nothing to the sidecar.
#[tokio::test]
async fn revocation_after_the_final_check_delivers_nothing() {
    let revocations: [fn(&ProxyState); 3] = [
        |state| state.set_api_key(None),
        |state| state.set_tokens(state.tokens().without("opencode")),
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
        let sidecar = spawn(
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
        verified(&state, &sidecar, 1, 1).await;
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
