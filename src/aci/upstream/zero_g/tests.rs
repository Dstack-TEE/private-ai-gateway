//! Backend tests against a mock 0G router and broker that run the enclave side
//! of the protocol (`protocol::broker`).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use super::protocol::broker;
use super::*;

const MODEL: &str = "glm-5.3";
const RESPONSE_KEY: &str = "res-key_1";

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Ok,
    WrongSigner,
    SignatureMissing,
    SignatureRedirect,
    Unsealed,
    Truncated,
    Reordered,
    Tampered,
}

struct Broker {
    mode: Mode,
    secret: <hpke::kem::X25519HkdfSha256 as hpke::Kem>::PrivateKey,
    signing_key: k256::ecdsa::SigningKey,
    requests: Mutex<Vec<(HeaderMap, Value)>>,
    signed_text: Mutex<Option<String>>,
    redirect_hits: AtomicUsize,
}

struct Fixture {
    broker: Arc<Broker>,
    backend: ZeroGProviderBackend,
    event: UpstreamVerifiedEvent,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

const ADDRESS: &str = "0x7dcfe6aea70350c2090041524c9b4a9262dce87d";

fn verified_event(origin: &str, enc_pub: &[u8; 32], signer: &str) -> UpstreamVerifiedEvent {
    UpstreamVerifiedEvent {
        upstream_name: "zero-g".into(),
        provider_type: Some(ZERO_G_PROVIDER.into()),
        model_id: MODEL.into(),
        url_origin: Some(origin.into()),
        verifier_id: "test".into(),
        result: VerificationResult::Verified,
        required: true,
        channel_bindings: vec![ChannelBinding::E2eePublicKeySha256 {
            provider: ZERO_G_PROVIDER.into(),
            key_id: Some(ADDRESS.into()),
            algorithm: ZERO_G_E2EE_ALGORITHM.into(),
            public_key_sha256: hex::encode(Sha256::digest(enc_pub)),
        }],
        provider_claims: Some(json!({
            "instances": {
                ADDRESS: VerifiedProvider {
                    provider_address: ADDRESS.into(),
                    provider_identity: None,
                    endpoint: origin.into(),
                    model: MODEL.into(),
                    signing_address: signer.into(),
                    e2ee_public_key: hex::encode(enc_pub),
                }
            }
        })),
        ..Default::default()
    }
}

impl Fixture {
    async fn new(mode: Mode) -> Self {
        let (secret, enc_pub) = broker::keypair(1);
        let (signing_key, signer) = broker::signer(2);
        let broker = Arc::new(Broker {
            mode,
            secret,
            signing_key: if mode == Mode::WrongSigner {
                broker::signer(3).0
            } else {
                signing_key
            },
            requests: Mutex::new(Vec::new()),
            signed_text: Mutex::new(None),
            redirect_hits: AtomicUsize::new(0),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/v1/chat/completions", post(chat))
            .route("/v1/proxy/signature/:key", get(signature))
            .route("/elsewhere", get(elsewhere))
            .with_state(broker.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            broker,
            backend: ZeroGProviderBackend::new_with_timeouts(&origin, 5, 5)
                .unwrap()
                .with_bearer_token("zg-test-key"),
            event: verified_event(&origin, &enc_pub, &signer),
            server,
        }
    }

    fn request(&self, stream: bool) -> PreparedUpstreamRequest {
        let body = json!({
            "model": MODEL,
            "stream": stream,
            "messages": [{"role": "user", "content": "secret prompt"}],
        });
        PreparedUpstreamRequest {
            request: UpstreamRequest {
                body: serde_json::to_vec(&body).unwrap(),
                // Caller-supplied 0G controls must never reach the router.
                headers: HashMap::from([
                    (TRUST_MODE_HEADER.to_string(), "standard".to_string()),
                    (PROVIDER_ADDRESS_HEADER.to_string(), "0xevil".to_string()),
                ]),
                path: Some(CHAT_COMPLETIONS_PATH.into()),
                target_route_id: None,
            },
            upstream_name: "zero-g".into(),
            url_origin: None,
            model_id: MODEL.into(),
            route_id: None,
            is_tee: Some(true),
        }
    }

    async fn stream_items(&self) -> Vec<Result<String, String>> {
        let response = self
            .backend
            .forward_stream_verified_prepared(self.request(true), &self.event)
            .await
            .expect("stream starts");
        assert_eq!(response.status_code, 200);
        response
            .body
            .map(|item| {
                item.map(|bytes| String::from_utf8(bytes.to_vec()).unwrap())
                    .map_err(|e| e.to_string())
            })
            .collect()
            .await
    }
}

fn completion_frames(stream: bool) -> Vec<Map<String, Value>> {
    let frame = |value: Value| value.as_object().unwrap().clone();
    if !stream {
        return vec![frame(json!({
            "id": "chatcmpl-1", "object": "chat.completion", "model": MODEL,
            "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4},
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
        }))];
    }
    vec![
        frame(
            json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "model": MODEL,
            "choices": [{"index": 0, "delta": {"content": "o"}}]}),
        ),
        frame(
            json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "model": MODEL,
            "choices": [{"index": 0, "delta": {"content": "k"}, "finish_reason": "stop"}]}),
        ),
        frame(
            json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "model": MODEL,
            "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}, "choices": []}),
        ),
    ]
}

async fn chat(
    State(broker): State<Arc<Broker>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    broker
        .requests
        .lock()
        .unwrap()
        .push((headers, body.clone()));
    let opened =
        broker::open_request(&broker.secret, body.as_object().unwrap()).expect("request opens");
    let stream = opened.request["stream"] == json!(true);
    let plain = completion_frames(stream);
    if broker.mode == Mode::Unsealed {
        let mut response = Json(Value::Object(plain[0].clone())).into_response();
        response
            .headers_mut()
            .insert(RESPONSE_KEY_HEADER, RESPONSE_KEY.parse().unwrap());
        return response;
    }
    let mut sealer = broker::ResponseSealer::new(&opened.client_key, opened.request_hash);
    let last = plain.len() - 1;
    let mut frames: Vec<Value> = plain
        .into_iter()
        .enumerate()
        .map(|(index, frame)| sealer.seal(frame, index == last))
        .collect();
    let text = if stream {
        sealer.stream_text()
    } else {
        sealer.buffered_text()
    };
    *broker.signed_text.lock().unwrap() = Some(text);
    match broker.mode {
        Mode::Truncated => {
            frames.pop();
        }
        Mode::Reordered => frames.swap(0, 1),
        Mode::Tampered => frames[1]["id"] = "chatcmpl-forged".into(),
        _ => {}
    }
    let mut response = if stream {
        let mut body = String::new();
        for frame in &frames {
            body.push_str(&format!("data: {frame}\n\n"));
        }
        body.push_str("data: [DONE]\n\n");
        ([("content-type", "text/event-stream")], body).into_response()
    } else {
        Json(frames.remove(0)).into_response()
    };
    response
        .headers_mut()
        .insert(RESPONSE_KEY_HEADER, RESPONSE_KEY.parse().unwrap());
    response
}

fn signature_body(broker: &Broker) -> Json<Value> {
    let text = broker
        .signed_text
        .lock()
        .unwrap()
        .clone()
        .expect("a response was signed");
    Json(json!({
        "text": text,
        "signature": broker::sign(&broker.signing_key, &text),
        "signing_address": "0x0000000000000000000000000000000000000000",
        "signing_algo": "ecdsa",
    }))
}

async fn signature(
    State(broker): State<Arc<Broker>>,
    axum::extract::Path(key): axum::extract::Path<String>,
) -> Response {
    assert_eq!(key, RESPONSE_KEY);
    match broker.mode {
        Mode::SignatureMissing => StatusCode::NOT_FOUND.into_response(),
        Mode::SignatureRedirect => {
            (StatusCode::TEMPORARY_REDIRECT, [("location", "/elsewhere")]).into_response()
        }
        _ => signature_body(&broker).into_response(),
    }
}

async fn elsewhere(State(broker): State<Arc<Broker>>) -> Response {
    broker.redirect_hits.fetch_add(1, Ordering::SeqCst);
    signature_body(&broker).into_response()
}

#[tokio::test]
async fn buffered_request_is_sealed_pinned_and_verified() {
    let fixture = Fixture::new(Mode::Ok).await;
    let response = fixture
        .backend
        .forward_verified_prepared(fixture.request(false), &fixture.event)
        .await
        .expect("verified response");
    let body: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "ok");
    assert!(body.get("_e2ee").is_none());
    assert_eq!(response.served_instance_id.as_deref(), Some(ADDRESS));

    let requests = fixture.broker.requests.lock().unwrap();
    let (headers, sent) = &requests[0];
    assert_eq!(headers[TRUST_MODE_HEADER], TRUST_MODE_PRIVATE);
    assert_eq!(headers[PROVIDER_ADDRESS_HEADER], ADDRESS);
    assert_eq!(headers[ALLOW_FALLBACKS_HEADER], "false");
    assert_eq!(headers["authorization"], "Bearer zg-test-key");
    assert!(headers.get(PROVIDER_IDENTITY_HEADER).is_none());
    assert!(
        sent.get("messages").is_none(),
        "the prompt never travels in the clear"
    );
    assert!(!sent.to_string().contains("secret prompt"));
}

#[tokio::test]
async fn buffered_response_fails_closed_without_a_verified_signature() {
    for (mode, expected) in [
        (Mode::WrongSigner, "signature invalid"),
        (Mode::SignatureMissing, "signature unavailable"),
        (Mode::SignatureRedirect, "signature unavailable"),
        (Mode::Unsealed, "not sealed"),
    ] {
        let fixture = Fixture::new(mode).await;
        let err = fixture
            .backend
            .forward_verified_prepared(fixture.request(false), &fixture.event)
            .await
            .expect_err("must fail closed");
        assert!(err.to_string().contains(expected), "{err}");
        assert_eq!(fixture.broker.redirect_hits.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn stream_releases_done_only_after_the_signature_verifies() {
    let fixture = Fixture::new(Mode::Ok).await;
    let items = fixture.stream_items().await;
    let events: Vec<String> = items.into_iter().map(Result::unwrap).collect();
    assert_eq!(events.len(), 4);
    assert!(events[0].contains(r#""content":"o""#) && !events[0].contains("_e2ee"));
    assert_eq!(events[3], "data: [DONE]\n\n");
}

#[tokio::test]
async fn stream_failures_end_in_an_error_and_never_in_done() {
    for (mode, delivered) in [
        (Mode::WrongSigner, 3),
        (Mode::SignatureMissing, 3),
        (Mode::Truncated, 2),
        (Mode::Reordered, 0),
        (Mode::Tampered, 1),
    ] {
        let fixture = Fixture::new(mode).await;
        let items = fixture.stream_items().await;
        assert_eq!(items.len(), delivered + 1, "mode {}", delivered);
        assert!(items[..delivered].iter().all(Result::is_ok));
        assert!(items[delivered].is_err());
        assert!(items
            .iter()
            .flatten()
            .all(|event| !event.contains("[DONE]")));
    }
}

#[tokio::test]
async fn refuses_before_sending_without_a_matching_binding_or_with_unknown_fields() {
    let fixture = Fixture::new(Mode::Ok).await;

    let mut forged = fixture.event.clone();
    let ChannelBinding::E2eePublicKeySha256 {
        public_key_sha256, ..
    } = &mut forged.channel_bindings[0]
    else {
        unreachable!()
    };
    *public_key_sha256 = "00".repeat(32);
    let err = fixture
        .backend
        .forward_verified_prepared(fixture.request(false), &forged)
        .await
        .unwrap_err();
    assert!(matches!(err, UpstreamError::ChannelBindingMismatch(_)));

    let mut request = fixture.request(false);
    request.request.body =
        serde_json::to_vec(&json!({"model": MODEL, "messages": [], "audio": {}})).unwrap();
    let err = fixture
        .backend
        .forward_verified_prepared(request, &fixture.event)
        .await
        .unwrap_err();
    assert!(matches!(err, UpstreamError::Routing(_)));

    assert!(fixture.broker.requests.lock().unwrap().is_empty());
}

#[test]
fn derives_the_broker_api_base_like_the_0g_client() {
    for (endpoint, base) in [
        ("https://cn-19.example", "https://cn-19.example/v1"),
        ("https://cn-19.example/v1/", "https://cn-19.example/v1"),
        (
            "https://cn-19.example:8443/v1/chat/completions",
            "https://cn-19.example:8443/v1",
        ),
    ] {
        assert_eq!(broker_v1_base(endpoint).unwrap(), base);
    }
    assert!(broker_v1_base("https://user@cn-19.example").is_err());
}
