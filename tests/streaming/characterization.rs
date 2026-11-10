//! Exact entity-byte and settlement snapshots, recorded against 0edde219.
//! Run with UPDATE_STREAMING_CORPUS=1 only when intentionally recording a baseline.
use crate::common;

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use axum::{routing::post, Json, Router};
use bytes::Bytes;
use futures_util::{task::AtomicWaker, FutureExt, Stream, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::Notify;

use super::*;
use crate::aci::keys::verify_receipt_signature;
use crate::aci::receipt::receipt_signing_input;
use crate::aci::upstream::{
    PreparedUpstreamRequest, UpstreamError, UpstreamRequest, UpstreamResponse,
    UpstreamStreamResponse,
};
use crate::middleware::errors::Surface;
use crate::middleware::request_transform::Endpoint;
use crate::middleware::{CompletionInput, Middleware, MiddlewareConfig, PrefixHashKey};
use crate::streaming_test_support::FixedInputs;

#[derive(Default)]
struct Gate {
    eof: AtomicBool,
    waker: AtomicWaker,
    headers: Notify,
}

struct CheckpointStream {
    chunks: VecDeque<Bytes>,
    gate: Arc<Gate>,
}

impl Stream for CheckpointStream {
    type Item = Result<Bytes, UpstreamError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(bytes) = self.chunks.pop_front() {
            return Poll::Ready(Some(Ok(bytes)));
        }
        self.gate.waker.register(cx.waker());
        if self.gate.eof.load(Ordering::Acquire) {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
}

#[derive(Default)]
struct TrackingSessions {
    inner: crate::aggregator::session_store::InMemorySessionStore,
    id: Mutex<Option<String>>,
    fail_retention: bool,
}
impl crate::aggregator::session_store::SessionStore for TrackingSessions {
    fn get_session(
        &self,
        id: &str,
        now: u64,
    ) -> Option<crate::aggregator::session::AttestedSession> {
        self.inner.get_session(id, now)
    }
    fn current_or_seal(
        &self,
        fingerprint: &str,
        retention_until: u64,
        now: u64,
        seal: &mut dyn FnMut() -> std::io::Result<crate::aggregator::session::AttestedSession>,
    ) -> std::io::Result<crate::aggregator::session_store::SessionLease> {
        let lease = self
            .inner
            .current_or_seal(fingerprint, retention_until, now, seal)?;
        *self.id.lock().unwrap() = Some(lease.session_id().to_string());
        Ok(lease)
    }
    fn extend_retention(
        &self,
        lease: &crate::aggregator::session_store::SessionLease,
        deadline: u64,
    ) -> std::io::Result<()> {
        if self.fail_retention {
            return Err(std::io::Error::other(
                "fixture retention persistence failed",
            ));
        }
        self.inner.extend_retention(lease, deadline)
    }
}

struct Backend {
    source: Vec<u8>,
    content_type: &'static str,
    gate: Arc<Gate>,
    early: bool,
}

#[async_trait]
impl UpstreamBackend for Backend {
    fn name(&self) -> &str {
        "characterization"
    }
    fn url_origin(&self) -> Option<&str> {
        Some("https://upstream.test")
    }
    async fn forward(&self, _: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        Ok(UpstreamResponse {
            status_code: 200,
            body: self.source.clone(),
            headers: HashMap::from([("content-type".into(), self.content_type.into())]),
            served_instance_id: None,
        })
    }
    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        _: &crate::aci::receipt::UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.forward_prepared(req).await
    }
    async fn forward_stream_verified_prepared(
        &self,
        _: PreparedUpstreamRequest,
        _: &crate::aci::receipt::UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        if self.early {
            self.gate.headers.notified().await;
        }
        Ok(UpstreamStreamResponse {
            status_code: 200,
            headers: HashMap::from([("content-type".into(), self.content_type.into())]),
            body: Box::pin(CheckpointStream {
                chunks: self
                    .source
                    .chunks(if self.source.len() > 1024 * 1024 {
                        64 * 1024
                    } else {
                        7
                    })
                    .map(Bytes::copy_from_slice)
                    .collect(),
                gate: self.gate.clone(),
            }),
            served_instance_id: None,
        })
    }
}

struct Verifier;
#[async_trait]
impl UpstreamVerifier for Verifier {
    async fn verify(
        &self,
        req: UpstreamVerificationRequest,
    ) -> crate::aci::receipt::UpstreamVerifiedEvent {
        common::event_from_request(&req, crate::aci::receipt::VerificationResult::Verified)
    }
}

struct Case {
    name: String,
    endpoint: Endpoint,
    source: String,
    format: &'static str,
    buffered: bool,
    early: bool,
    disconnect: bool,
    e2ee: bool,
    non_sse: bool,
    lease_handoff: bool,
    fail_publish: bool,
}

impl Case {
    fn new(name: &str, endpoint: Endpoint, source: &str, format: &'static str) -> Self {
        Self {
            name: name.into(),
            endpoint,
            source: source.into(),
            format,
            buffered: false,
            early: false,
            disconnect: false,
            e2ee: false,
            non_sse: false,
            lease_handoff: false,
            fail_publish: false,
        }
    }
}

async fn capture(case: &Case) -> Value {
    let _fixed = FixedInputs::enter();
    let (sent, mut reports) = tokio::sync::mpsc::unbounded_channel();
    let path = match case.endpoint {
        Endpoint::Messages => MESSAGES_PATH,
        Endpoint::CreateModelResponse => RESPONSES_PATH,
        Endpoint::Complete => COMPLETIONS_PATH,
        _ => CHAT_COMPLETIONS_PATH,
    };
    let candidate = if case.endpoint == Endpoint::CreateModelResponse
        && case.name.starts_with("native_responses")
    {
        json!({"routeId":"fixture:model", "format":case.format, "supportedEndpoints":[RESPONSES_PATH]})
    } else {
        json!({"routeId":"fixture:model", "format":case.format})
    };
    let pre = json!({"allow":true,"candidates":[candidate],"pricing":{"inputCostPerToken":"1","outputCostPerToken":"2"}});
    let app = Router::new()
        .route(
            "/consult/pre",
            post(move || {
                let pre = pre.clone();
                async move { Json(pre) }
            }),
        )
        .route(
            "/consult/post",
            post(move |Json(body): Json<Value>| {
                let sent = sent.clone();
                async move {
                    sent.send(body).unwrap();
                    axum::http::StatusCode::OK
                }
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let gate = Arc::new(Gate::default());
    let sessions = Arc::new(TrackingSessions {
        fail_retention: case.fail_publish,
        ..TrackingSessions::default()
    });
    let clock = Arc::new(common::admission::TestClock(
        std::sync::atomic::AtomicU64::new(1_700_000_000),
    ));
    let service = Arc::new(
        AciService::new_with_upstream_verifier(
            Arc::new(common::StaticKeyProvider::default()),
            Arc::new(common::StubQuoter::default()),
            Arc::new(Backend {
                source: case.source.as_bytes().to_vec(),
                content_type: if case.non_sse || case.buffered {
                    "application/json"
                } else {
                    "text/event-stream"
                },
                gate: gate.clone(),
                early: case.early,
            }),
            Arc::new(Verifier),
            Arc::new(InMemoryReceiptStore::default()),
            AciServiceConfig::for_test(),
            clock.clone(),
        )
        .unwrap()
        .with_session_store(sessions.clone()),
    );
    let config = MiddlewareConfig {
        control_url: format!("http://{addr}"),
        control_token: None,
        control_timeout_ms: Some(2_000),
        control_post_timeout_ms: Some(2_000),
        sse_keepalive_ms: Some(if case.early { 1 } else { 0 }),
        send_request_features: Some(false),
        tee_only_domains: vec![],
    };
    let mw = Middleware::new(PrefixHashKey::new([7; 32]), &config).unwrap();
    let params = match case.endpoint {
        Endpoint::CreateModelResponse => {
            json!({"model":"fixture-model","input":"hi","stream":!case.buffered})
        }
        Endpoint::Complete => {
            json!({"model":"fixture-model","prompt":"hi","stream":!case.buffered})
        }
        _ => {
            json!({"model":"fixture-model","messages":[{"role":"user","content":"hi"}],"max_tokens":16,"stream":!case.buffered})
        }
    };
    let e2ee = case.e2ee.then(|| E2eeRequestContext {
        version: "2".into(),
        algo: crate::aci::e2ee::E2EE_ALGO_X25519_AESGCM.into(),
        aad_mode: wire::E2eeAadMode::AciV2,
        request_model: "fixture-model".into(),
        client_public_key_hex: crate::aci::e2ee::x25519_public_key_hex(
            &crate::aci::e2ee::x25519_secret_key_from_bytes(&[0x37; 32]).unwrap(),
        ),
        nonce: Some("01".repeat(32)),
        timestamp: Some(1_700_000_000),
    });
    let response = mw
        .handle_completion(
            &service,
            CompletionInput {
                audio_form: None,
                upload_permit: None,
                endpoint: case.endpoint,
                endpoint_path: path,
                surface: if case.endpoint == Endpoint::Messages {
                    Surface::Anthropic
                } else {
                    Surface::Openai
                },
                received_body: serde_json::to_vec(&params).unwrap().into(),
                params,
                api_key_hash: Some("fixture-key-hash".into()),
                requester: None,
                e2ee,
                aci_required: false,
                aci_session_ids: vec![],
                request_id: "req-fixture".into(),
                user_model: Some("fixture-model".into()),
                stream: !case.buffered,
                tee_only: false,
            },
        )
        .await;
    let status = response.status().as_u16();
    let mut body = response.into_body().into_data_stream();
    let mut wire = Vec::new();
    if case.early {
        // The initial comment is emitted before forwarding resolves.
        wire.extend_from_slice(&body.next().await.unwrap().unwrap());
        gate.headers.notify_one();
    }
    while let Some(Some(chunk)) = body.next().now_or_never() {
        wire.extend_from_slice(&chunk.unwrap());
    }
    let checkpoint_report = if case.buffered {
        Some(
            tokio::time::timeout(Duration::from_secs(3), reports.recv())
                .await
                .unwrap()
                .unwrap(),
        )
    } else {
        // Give the real control-plane report channel a chance to receive any
        // premature settlement while the upstream EOF gate is still closed.
        tokio::time::timeout(Duration::from_millis(50), reports.recv())
            .await
            .ok()
            .flatten()
    };
    if case.lease_handoff {
        clock.set(1_700_003_601);
        assert!(
            service
                .get_attested_session(sessions.id.lock().unwrap().as_deref().unwrap())
                .is_some(),
            "in-flight source must own its lease"
        );
    }
    let before = json!({"receipt":service.get_receipt_by_receipt_id("rcpt-characterization").is_some(),"report":checkpoint_report.is_some()});
    if case.disconnect {
        drop(body);
    } else {
        gate.eof.store(true, Ordering::Release);
        gate.waker.wake();
        while let Some(chunk) = body.next().await {
            wire.extend_from_slice(&chunk.unwrap());
        }
    }
    let report = match checkpoint_report {
        Some(report) => report,
        None => tokio::time::timeout(Duration::from_secs(3), reports.recv())
            .await
            .unwrap()
            .unwrap(),
    };
    let mut all_reports = vec![report];
    while let Ok(report) = reports.try_recv() {
        all_reports.push(report);
    }
    if case.e2ee {
        let value: Value = String::from_utf8_lossy(&wire)
            .lines()
            .filter_map(|l| serde_json::from_str(l.strip_prefix("data: ")?).ok())
            .next()
            .unwrap();
        let aad=crate::aci::digest::jcs_bytes(&json!({"purpose":"aci.e2ee.response.v2","algo":crate::aci::e2ee::E2EE_ALGO_X25519_AESGCM,"model":"fixture-model","id":"req-fixture","field":"choices.0.delta.content","nonce":"01".repeat(32),"ts":1_700_000_000})).unwrap();
        let secret = crate::aci::e2ee::x25519_secret_key_from_bytes(&[0x37; 32]).unwrap();
        let ciphertext = value["choices"][0]["delta"]["content"].as_str().unwrap();
        assert_eq!(
            crate::aci::e2ee::decrypt_x25519_with_secret_key(&secret, ciphertext, &aad).unwrap(),
            b"hi"
        );
        assert!(crate::aci::e2ee::decrypt_x25519_with_secret_key(
            &secret,
            ciphertext,
            b"tampered AAD"
        )
        .is_err());
    }
    if case.lease_handoff {
        assert!(
            service
                .get_attested_session(sessions.id.lock().unwrap().as_deref().unwrap())
                .is_some(),
            "receipt handoff extends the lease retention"
        );
    }
    let receipt = service.get_receipt_by_receipt_id("rcpt-characterization");
    let (receipt, signature_valid) = receipt.map_or((Value::Null, false), |r| {
        let value = r.document_json().unwrap();
        let key = service
            .keyset()
            .receipt_signing_keys
            .into_iter()
            .find(|k| k.key_id == r.key_id)
            .unwrap();
        let valid = verify_receipt_signature(
            &key,
            &receipt_signing_input(&value).unwrap(),
            &hex::decode(r.signature_hex).unwrap(),
        );
        (value, valid)
    });
    server.abort();
    let _ = server.await;
    let mut snapshot = json!({"name":case.name,"wire":String::from_utf8(wire.clone()).unwrap(),"wire_hash":crate::aci::digest::sha256_hex(&wire),"source_hash":crate::aci::digest::sha256_hex(case.source.as_bytes()),"status":status,"before_eof":before,"receipt":receipt,"signature_valid":signature_valid,"reports":all_reports});
    if case.fail_publish {
        snapshot["metrics"] = String::from_utf8(service.metrics().unwrap().body)
            .unwrap()
            .into();
    }
    snapshot
}

async fn capture_buffered_sse(source: &str, encrypted: bool) -> Value {
    let _fixed = FixedInputs::enter();
    let service = AciService::new(
        Arc::new(common::StaticKeyProvider::default()),
        Arc::new(common::StubQuoter::default()),
        Arc::new(Backend {
            source: vec![],
            content_type: "text/event-stream",
            gate: Arc::new(Gate::default()),
            early: false,
        }),
        Arc::new(InMemoryReceiptStore::default()),
        AciServiceConfig::for_test(),
        Arc::new(FixedClock(1_700_000_000)),
    )
    .unwrap();
    let mut builder = crate::aci::receipt::ReceiptBuilder::new(
        "rcpt-characterization".into(),
        None,
        Some("fixture-model".into()),
        service.workload_keyset_digest(),
        CHAT_COMPLETIONS_PATH.into(),
        "POST".into(),
        1_700_000_000,
    );
    builder.add_request_received(b"{}").unwrap();
    builder.add_request_forwarded(b"{}").unwrap();
    builder.add_response_received(source.as_bytes()).unwrap();
    let draft = MiddlewareReceiptDraft {
        session_lease: None,
        receipt_id: "rcpt-characterization".into(),
        builder,
        endpoint_path: CHAT_COMPLETIONS_PATH.into(),
        request_mode: crate::aggregator::metrics::RequestMode::Buffered,
        response_model: None,
    };
    let ctx = encrypted.then(|| E2eeRequestContext {
        version: "2".into(),
        algo: crate::aci::e2ee::E2EE_ALGO_X25519_AESGCM.into(),
        aad_mode: wire::E2eeAadMode::AciV2,
        request_model: "fixture-model".into(),
        client_public_key_hex: crate::aci::e2ee::x25519_public_key_hex(
            &crate::aci::e2ee::x25519_secret_key_from_bytes(&[0x37; 32]).unwrap(),
        ),
        nonce: Some("01".repeat(32)),
        timestamp: Some(1_700_000_000),
    });
    let result = service
        .finalize_middleware_receipt(
            draft,
            source.as_bytes().to_vec(),
            Some("text/event-stream"),
            None,
            ctx,
        )
        .unwrap();
    let receipt = result.receipt.document_json().unwrap();
    let signature_valid = verify_receipt_signature(
        &service.keyset().receipt_signing_keys[0],
        &receipt_signing_input(&receipt).unwrap(),
        &hex::decode(result.receipt.signature_hex).unwrap(),
    );
    json!({"name":if encrypted {"buffered_sse_e2ee"} else {"buffered_sse"},"source_hash":crate::aci::digest::sha256_hex(source.as_bytes()),"wire_hash":crate::aci::digest::sha256_hex(&result.wire_body),"wire":String::from_utf8(result.wire_body).unwrap(),"receipt":receipt,"signature_valid":signature_valid})
}

fn rate_limit_cases() -> Vec<Case> {
    let chat = concat!(
        "data: {\"error\":{\"code\":429,\"type\":\"rate_limit_error\",\"message\":\"Too many requests\"}}\n\n",
        "data: [DONE]\n\n"
    );
    [
        ("rate_limit_native_chat", Endpoint::ChatComplete, chat, "openai"),
        ("rate_limit_native_messages", Endpoint::Messages,
         "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"Too many requests\"}}\n\n", "anthropic"),
        ("native_responses_rate_limit_error", Endpoint::CreateModelResponse,
         "event: response.error\ndata: {\"type\":\"response.error\",\"sequence_number\":2,\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"Too many requests\"}}\n\n", "openai"),
        ("native_responses_rate_limit_failed", Endpoint::CreateModelResponse,
         "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"id\":\"upstream-id\",\"model\":\"upstream-model\",\"status\":\"failed\",\"output\":[],\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"Too many requests\"},\"usage\":{\"input_tokens\":2,\"output_tokens\":3,\"total_tokens\":5}}}\n\n", "openai"),
        ("rate_limit_chat_messages", Endpoint::Messages, chat, "openai"),
    ]
    .into_iter()
    .map(|(name, endpoint, source, format)| Case::new(name, endpoint, source, format))
    .collect()
}

fn failed_usage_case() -> Case {
    Case::new(
        "failed_chat_usage_filter",
        Endpoint::ChatComplete,
        "data: {\"error\":{\"type\":\"api_error\",\"message\":\"PRIVATE_UPSTREAM_DETAIL\"},\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5,\"provider_trace\":\"PRIVATE_UPSTREAM_DETAIL\"}}\n\ndata: [DONE]\n\n",
        "openai",
    )
}

fn usage_after_failure_case() -> Case {
    Case::new(
        "chat_usage_after_failure",
        Endpoint::ChatComplete,
        concat!(
            "data: {\"error\":{\"type\":\"api_error\",\"message\":\"Upstream failed\"}}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"provider_trace\":\"PRIVATE_UPSTREAM_DETAIL\"}}\n\n",
            "data: [DONE]\n\n"
        ),
        "openai",
    )
}

fn review_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for early in [false, true] {
        let source = "x".repeat(16 * 1024 * 1024 + 1);
        let mut case = Case::new(
            if early {
                "early_non_sse_over_cap"
            } else {
                "non_sse_over_cap"
            },
            Endpoint::ChatComplete,
            &source,
            "openai",
        );
        case.non_sse = true;
        case.early = early;
        cases.push(case);
    }
    cases.push(Case::new("late_filtered_failure", Endpoint::ChatComplete,
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\nevent: ping\ndata: {\"type\":\"ping\",\"error\":{\"type\":\"api_error\",\"message\":\"failure\"}}\n\n", "anthropic"));
    cases.push(Case::new("native_post_terminal_usage", Endpoint::ChatComplete,
        "data: [DONE]\n\ndata: {\"id\":\"upstream-id\",\"model\":\"upstream-model\",\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5}}\n\n", "openai"));
    cases.push(Case::new("anthropic_post_terminal_usage", Endpoint::ChatComplete,
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"upstream-model\",\"usage\":{\"input_tokens\":1}}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n", "anthropic"));
    let chat_error = "data: {\"error\":{\"type\":\"api_error\",\"message\":\"PRIVATE_UPSTREAM_DETAIL\"}}\n\ndata: [DONE]\n\n";
    let messages_error = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"PRIVATE_UPSTREAM_DETAIL\"}}\n\n";
    for (name, endpoint, source, format) in [
        ("failure_native_chat", Endpoint::ChatComplete, chat_error, "openai"),
        ("failure_native_messages", Endpoint::Messages, messages_error, "anthropic"),
        ("native_responses_failure", Endpoint::CreateModelResponse, "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"id\":\"upstream-id\",\"model\":\"upstream-model\",\"status\":\"failed\",\"output\":[],\"error\":{\"type\":\"api_error\",\"message\":\"PRIVATE_UPSTREAM_DETAIL\"},\"usage\":{\"input_tokens\":2,\"output_tokens\":3,\"total_tokens\":5}}}\n\n", "openai"),
        ("native_responses_error", Endpoint::CreateModelResponse, "event: response.error\ndata: {\"type\":\"response.error\",\"sequence_number\":2,\"error\":{\"type\":\"api_error\",\"message\":\"PRIVATE_UPSTREAM_DETAIL\"}}\n\n", "openai"),
        ("failure_messages_chat", Endpoint::ChatComplete, messages_error, "anthropic"),
        ("failure_chat_messages", Endpoint::Messages, chat_error, "openai"),
        ("failure_chat_responses", Endpoint::CreateModelResponse, chat_error, "openai"),
        ("failure_messages_responses", Endpoint::CreateModelResponse, messages_error, "anthropic"),
        ("failure_legacy_completion", Endpoint::Complete, messages_error, "anthropic"),
    ] {
        cases.push(Case::new(name, endpoint, source, format));
    }

    cases
}

#[tokio::test]
async fn exact_wire_characterization_corpus() {
    const CHAT:&str="data: {\"id\":\"upstream-id\",\"created\":1700000000,\"model\":\"upstream-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5}}\n\ndata: [DONE]\n\n";
    let mut cases = vec![
        Case::new("native_chat", Endpoint::ChatComplete, CHAT, "openai"),
        Case::new("chat_messages", Endpoint::Messages, CHAT, "openai"),
        Case::new(
            "chat_responses",
            Endpoint::CreateModelResponse,
            CHAT,
            "openai",
        ),
    ];
    let mut lease = Case::new("lease_handoff", Endpoint::ChatComplete, CHAT, "openai");
    lease.lease_handoff = true;
    cases.push(lease);
    let mut e2ee = Case::new("e2ee_chat", Endpoint::ChatComplete, CHAT, "openai");
    e2ee.e2ee = true;
    cases.push(e2ee);
    let mut early = Case::new("early_commit", Endpoint::ChatComplete, CHAT, "openai");
    early.early = true;
    cases.push(early);
    let mut disconnect = Case::new(
        "partial_usage_disconnect",
        Endpoint::ChatComplete,
        CHAT,
        "openai",
    );
    disconnect.disconnect = true;
    cases.push(disconnect);
    let mut buffered = Case::new(
        "buffered_json",
        Endpoint::ChatComplete,
        r#"{"id":"upstream-id","model":"upstream-model","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}"#,
        "openai",
    );
    buffered.buffered = true;
    cases.push(buffered);
    let mut non_sse = Case::new(
        "declared_non_sse",
        Endpoint::ChatComplete,
        r#"{"id":"upstream-id","model":"upstream-model","choices":[],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}"#,
        "openai",
    );
    non_sse.non_sse = true;
    cases.push(non_sse);
    for (name, source) in [
        ("declared_non_sse_invalid", "not JSON"),
        ("declared_non_sse_array", "[1, 2]"),
        (
            "early_declared_non_sse",
            r#"{"id":"upstream-id","model":"upstream-model","choices":[],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}"#,
        ),
    ] {
        let mut case = Case::new(name, Endpoint::ChatComplete, source, "openai");
        case.non_sse = true;
        case.early = name == "early_declared_non_sse";
        cases.push(case);
    }
    let messages="event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"upstream-id\",\"model\":\"upstream-model\",\"role\":\"assistant\",\"content\":[],\"usage\":{\"input_tokens\":2}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    for (name, endpoint) in [
        ("native_messages", Endpoint::Messages),
        ("messages_chat", Endpoint::ChatComplete),
        ("messages_responses", Endpoint::CreateModelResponse),
    ] {
        cases.push(Case::new(name, endpoint, messages, "anthropic"));
    }
    cases.push(Case::new("legacy_completion",Endpoint::Complete,"data: {\"log_id\":\"upstream-id\",\"model\":\"upstream-model\",\"completion\":\"hi\",\"stop_reason\":\"stop_sequence\"}\n\ndata: [DONE]\n\n","anthropic"));
    cases.push(Case::new("native_responses",Endpoint::CreateModelResponse,"event: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":0,\"response\":{\"id\":\"upstream-id\",\"model\":\"upstream-model\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":2,\"output_tokens\":3,\"total_tokens\":5}}}\n\n","openai"));
    for (name, source) in crate::aci::upstream::characterization_provider_sources().await {
        cases.push(Case::new(name, Endpoint::ChatComplete, &source, "openai"));
    }
    cases.extend(review_cases());
    cases.extend(rate_limit_cases());
    cases.push(failed_usage_case());
    cases.push(usage_after_failure_case());
    let mut actual = Vec::new();
    for case in cases {
        actual.push(capture(&case).await);
    }
    actual.push(capture_buffered_sse(CHAT, false).await);
    actual.push(capture_buffered_sse(CHAT, true).await);
    if std::env::var_os("RECORD_STREAMING_ACTUAL").is_some() {
        std::fs::write(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/streaming/actual.tmp"),
            serde_json::to_vec_pretty(&actual).unwrap(),
        )
        .unwrap();
    }
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/streaming_wire.json");
    if std::env::var_os("UPDATE_STREAMING_CORPUS").is_some() {
        std::fs::write(
            path,
            format!("{}\n", serde_json::to_string_pretty(&actual).unwrap()),
        )
        .unwrap();
    } else {
        let expected: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let corrections: Value =
            serde_json::from_str(include_str!("../fixtures/streaming_corrections.json")).unwrap();
        for (actual, baseline) in actual.iter().zip(expected.as_array().unwrap()) {
            let name = actual["name"].as_str().unwrap();
            let expected = if let Some(correction) = corrections.get(name) {
                assert_eq!(
                    correction["baseline_wire"], baseline["wire"],
                    "original baseline for {name}"
                );
                assert!(!correction["reason"].as_str().unwrap().is_empty());
                &correction["expected"]
            } else {
                baseline
            };
            assert_eq!(actual, expected, "case {name}");
        }
        assert_eq!(actual.len(), expected.as_array().unwrap().len());
    }
}

#[tokio::test]
async fn receipt_retention_failure_settles_once_with_usage_and_preserves_metrics() {
    let mut case = Case::new("receipt_failure", Endpoint::ChatComplete,
        "data: {\"id\":\"upstream-id\",\"model\":\"upstream-model\",\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5}}\n\ndata: [DONE]\n\n", "openai");
    case.fail_publish = true;
    let snapshot = capture(&case).await;
    assert_eq!(
        snapshot["before_eof"],
        json!({"receipt":false,"report":false})
    );
    assert!(snapshot["receipt"].is_null());
    let wire = snapshot["wire"].as_str().unwrap();
    assert!(wire.contains("\"cost\":8"), "{wire}");
    assert!(wire.ends_with("data: [DONE]\n\n"));
    let reports = snapshot["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["status"], 502);
    assert_eq!(reports[0]["errorSource"], "gateway");
    assert_eq!(reports[0]["errorMessage"], "downstream_finalizer_failed");
    assert_eq!(reports[0]["usage"]["prompt_tokens"], 2);
    assert_eq!(reports[0]["usage"]["completion_tokens"], 3);
    assert!(reports[0]["usage"].get("cost").is_none());
    let metrics = snapshot["metrics"].as_str().unwrap();
    for (prefix, label) in [
        (
            "private_ai_gateway_upstream_responses_total{",
            "model_id=\"upstream-model\"",
        ),
        (
            "private_ai_gateway_stream_errors_total{",
            "kind=\"receipt_finalize\"",
        ),
    ] {
        let series: Vec<_> = metrics
            .lines()
            .filter(|line| line.starts_with(prefix))
            .collect();
        assert_eq!(series.len(), 1, "{metrics}");
        assert!(series[0].contains(label), "{metrics}");
        assert!(series[0].ends_with(" 1"), "{metrics}");
    }
    assert!(!metrics
        .lines()
        .any(|line| line.starts_with("private_ai_gateway_receipts_issued_total{")));
}

#[tokio::test]
async fn early_declared_non_sse_opaque_bodies_use_unbilled_byte_path() {
    for source in ["not JSON", "[1, 2]"] {
        let mut case = Case::new("early_opaque", Endpoint::ChatComplete, source, "openai");
        case.early = true;
        case.non_sse = true;
        let snapshot = capture(&case).await;
        assert_eq!(snapshot["wire"], format!(": PROCESSING\n\n{source}"));
        assert_eq!(
            snapshot["before_eof"],
            json!({"receipt":false,"report":false})
        );
        assert!(snapshot["signature_valid"].as_bool().unwrap());
        let reports = snapshot["reports"].as_array().unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0]["status"], 200);
        assert!(reports[0]["usage"].is_null());
        assert!(reports[0]["errorMessage"].is_null());
    }
}

#[tokio::test]
async fn over_cap_non_sse_matches_main_on_normal_and_early_paths() {
    let baselines: Value =
        serde_json::from_str(include_str!("../fixtures/streaming_wire.json")).unwrap();
    let corrections: Value =
        serde_json::from_str(include_str!("../fixtures/streaming_corrections.json")).unwrap();
    for case in review_cases().into_iter().take(2) {
        let actual = capture(&case).await;
        let baseline = baselines
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["name"] == case.name)
            .unwrap();
        let expected = corrections
            .get(&case.name)
            .map_or(baseline, |value| &value["expected"]);
        assert_eq!(&actual, expected);
        assert_eq!(actual["reports"].as_array().unwrap().len(), 1);
        assert_eq!(actual["reports"][0]["status"], 502);
        assert_eq!(actual["reports"][0]["errorMessage"], "upstream_transport");
        assert!(actual["reports"][0]["usage"].is_null());
        assert!(actual["reports"][0]["ttftMs"].is_null());
        assert!(actual["receipt"].is_null());
    }
}

#[tokio::test]
async fn native_messages_extension_failure_never_exposes_upstream_text() {
    let case = Case::new(
        "messages_extension_failure",
        Endpoint::Messages,
        "event: extension\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"PRIVATE_UPSTREAM_DETAIL\"}}\n\n",
        "anthropic",
    );
    let snapshot = capture(&case).await;
    assert_eq!(snapshot["wire"], "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"The upstream provider returned an error\"},\"request_id\":\"req-fixture\"}\n\n");
    let reports = snapshot["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["status"], 502);
    assert_eq!(reports[0]["errorMessage"], "stream_inband_error");
    assert!(reports[0]["errorSource"].is_null());
}

#[tokio::test]
async fn native_responses_extension_failure_keeps_usage_and_cost() {
    let case = Case::new(
        "native_responses_extension_failure",
        Endpoint::CreateModelResponse,
        "event: extension\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"message\":\"PRIVATE_UPSTREAM_DETAIL from https://provider.test\"},\"usage\":{\"input_tokens\":2,\"output_tokens\":3,\"total_tokens\":5}}}\n\n",
        "openai",
    );
    let snapshot = capture(&case).await;
    let wire = snapshot["wire"].as_str().unwrap();
    assert!(wire.starts_with("event: response.failed\n"));
    assert!(!wire.contains("PRIVATE_UPSTREAM_DETAIL"));
    let payload: Value =
        serde_json::from_str(wire.lines().nth(1).unwrap().strip_prefix("data: ").unwrap()).unwrap();
    assert_eq!(payload["response"]["status"], "failed");
    assert_eq!(
        payload["response"]["usage"],
        json!({"input_tokens":2,"output_tokens":3,"total_tokens":5,"cost":8})
    );
    let reports = snapshot["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["status"], 502);
    assert_eq!(reports[0]["errorMessage"], "stream_inband_error");
    assert!(reports[0]["errorSource"].is_null());
    assert_eq!(
        reports[0]["usage"],
        json!({"input_tokens":2,"output_tokens":3,"total_tokens":5})
    );
}

#[tokio::test]
async fn owner_preserves_sanitized_rate_limit_codes_and_failure_terminals() {
    for case in rate_limit_cases() {
        let snapshot = capture(&case).await;
        let wire = snapshot["wire"].as_str().unwrap();
        let error_line = wire
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .unwrap();
        let payload: Value = serde_json::from_str(error_line).unwrap();
        let error = match case.endpoint {
            Endpoint::ChatComplete => {
                assert!(wire.ends_with("data: [DONE]\n\n"), "{wire}");
                assert_eq!(wire.matches("\n\n").count(), 2);
                assert_eq!(payload["error"]["code"], 429);
                &payload["error"]
            }
            Endpoint::Messages => {
                assert!(wire.starts_with("event: error\n"), "{wire}");
                &payload["error"]
            }
            Endpoint::CreateModelResponse if case.name.ends_with("failed") => {
                assert!(wire.starts_with("event: response.failed\n"), "{wire}");
                assert_eq!(payload["response"]["status"], "failed");
                &payload["response"]["error"]
            }
            Endpoint::CreateModelResponse => {
                assert!(wire.starts_with("event: error\n"), "{wire}");
                assert_eq!(payload["sequence_number"], 2);
                &payload
            }
            _ => panic!("unexpected rate-limit fixture {}", case.name),
        };
        if case.endpoint == Endpoint::CreateModelResponse {
            assert_eq!(error["code"], "rate_limit_exceeded");
        } else {
            assert_eq!(error["type"], "rate_limit_error");
        }
        if case.endpoint != Endpoint::ChatComplete {
            assert_eq!(wire.matches("\n\n").count(), 1);
        }
        assert_eq!(
            error["message"],
            "Rate limit exceeded. Please retry after some time."
        );
        let reports = snapshot["reports"].as_array().unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0]["status"], 502);
        assert_eq!(reports[0]["errorMessage"], "stream_inband_error");
    }
}

#[tokio::test]
async fn owner_filters_failed_usage_before_reporting_and_pricing() {
    let snapshot = capture(&failed_usage_case()).await;
    let wire = snapshot["wire"].as_str().unwrap();
    assert!(wire.ends_with("data: [DONE]\n\n"), "{wire}");
    assert!(!wire.contains("PRIVATE_UPSTREAM_DETAIL"));
    assert!(wire.contains("\"cost\":8"), "{wire}");
    let reports = snapshot["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["status"], 502);
    assert_eq!(reports[0]["errorMessage"], "stream_inband_error");
    assert_eq!(
        reports[0]["usage"],
        json!({"prompt_tokens":2,"completion_tokens":3,"total_tokens":5})
    );
}

#[tokio::test]
async fn conversion_overflow_after_destination_terminal_is_a_gateway_failure() {
    let text = "x".repeat(crate::sse::MAX_EVENT_BYTES - 100);
    let event =
        format!("event: content_block_delta\ndata: {{\"delta\":{{\"text\":\"{text}\"}}}}\n\n");
    assert!(event.len() < crate::sse::MAX_EVENT_BYTES);
    let case = Case::new(
        "late_conversion_overflow",
        Endpoint::ChatComplete,
        &format!("event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n{event}"),
        "anthropic",
    );
    let snapshot = capture(&case).await;
    assert_eq!(snapshot["wire"], "data: [DONE]\n\n");
    let reports = snapshot["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["status"], 502);
    assert_eq!(reports[0]["errorMessage"], "internal_error");
    assert_eq!(reports[0]["errorSource"], "gateway");
    assert!(snapshot["receipt"].is_null());
}
