//! Terminal publication requires an accepted source terminal or clean EOF.
use super::*;

static PATCHES: std::sync::LazyLock<HashMap<String, Value>> = std::sync::LazyLock::new(|| {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/streaming_terminal_corrections.jsonl"
    ))
    .lines()
    .map(|line| serde_json::from_str::<Value>(line).unwrap())
    .collect::<Vec<_>>()
    .into_iter()
    .map(|correction| (correction["name"].as_str().unwrap().to_string(), correction))
    .collect()
});

pub(super) fn patch_count() -> usize {
    PATCHES.len()
}

/// Small field patches retain #563's exact snapshots and name every intentional
/// terminal-publication difference, without recording a second complete corpus.
pub(super) fn expected(name: &str, prior: &Value) -> Value {
    let mut expected = prior.clone();
    if let Some(correction) = PATCHES.get(name) {
        assert!(!correction["reason"].as_str().unwrap().is_empty());
        for (path, value) in correction["patch"].as_object().unwrap() {
            assert_ne!(
                expected.pointer(path),
                Some(value),
                "stale terminal patch {name} {path}"
            );
            *expected
                .pointer_mut(path)
                .unwrap_or_else(|| panic!("missing snapshot path {name} {path}")) = value.clone();
        }
    }
    normalize_snapshot(&mut expected);
    expected
}

const CHAT: &str = concat!(
    "data: {\"id\":\"upstream-id\",\"model\":\"upstream-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5}}\n\n",
    "data: [DONE]\n\n"
);
const MESSAGES: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"upstream-id\",\"model\":\"upstream-model\",\"usage\":{\"input_tokens\":2}}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
);

fn event_hash<'a>(snapshot: &'a Value, kind: &str) -> &'a Value {
    &snapshot["receipt"]["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["type"] == kind)
        .unwrap()["body_hash"]
}

fn assert_published(snapshot: &Value, received: Option<&[u8]>, status: u16, direct: bool) {
    assert_eq!(snapshot["signature_valid"], true, "{snapshot}");
    assert_eq!(
        event_hash(snapshot, "response.returned"),
        &snapshot["wire_hash"]
    );
    if let Some(received) = received {
        assert_eq!(
            event_hash(snapshot, "response.received"),
            &json!(crate::aci::digest::sha256_hex(received))
        );
    }
    assert_eq!(snapshot["before_eof"]["receipt"], true);
    let reports = snapshot["reports"].as_array().unwrap();
    if direct {
        assert!(reports.is_empty());
    } else {
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0]["status"], status);
    }
}

#[tokio::test]
async fn terminal_poll_publishes_before_disconnect_on_every_native_and_bridged_path() {
    let chat_eof = CHAT.strip_suffix("data: [DONE]\n\n").unwrap();
    let responses = "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"upstream-id\",\"model\":\"upstream-model\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":2,\"output_tokens\":3}}}\n\n";
    let legacy = "data: {\"log_id\":\"upstream-id\",\"model\":\"upstream-model\",\"completion\":\"hi\",\"stop_reason\":\"stop_sequence\"}\n\ndata: [DONE]\n\n";
    for (name, endpoint, source, format) in [
        ("terminal_chat", Endpoint::ChatComplete, CHAT, "openai"),
        ("terminal_chat_messages", Endpoint::Messages, CHAT, "openai"),
        (
            "terminal_chat_responses",
            Endpoint::CreateModelResponse,
            CHAT,
            "openai",
        ),
        (
            "terminal_chat_messages_eof",
            Endpoint::Messages,
            chat_eof,
            "openai",
        ),
        (
            "terminal_chat_responses_eof",
            Endpoint::CreateModelResponse,
            chat_eof,
            "openai",
        ),
        ("terminal_chat_complete", Endpoint::Complete, CHAT, "openai"),
        (
            "terminal_messages",
            Endpoint::Messages,
            MESSAGES,
            "anthropic",
        ),
        (
            "terminal_messages_chat",
            Endpoint::ChatComplete,
            MESSAGES,
            "anthropic",
        ),
        (
            "terminal_messages_responses",
            Endpoint::CreateModelResponse,
            MESSAGES,
            "anthropic",
        ),
        ("terminal_legacy", Endpoint::Complete, legacy, "anthropic"),
        (
            "native_responses_terminal",
            Endpoint::CreateModelResponse,
            responses,
            "openai",
        ),
    ] {
        for early in [false, true] {
            let mut case = Case::new(name, endpoint, source, format);
            case.drop_at_terminal = true;
            case.early = early;
            case.eof_at_source_end = name.ends_with("_eof");
            let snapshot = capture(&case).await;
            assert_published(&snapshot, Some(source.as_bytes()), 200, false);
            let alias = match name {
                "terminal_chat_messages"
                | "terminal_chat_responses"
                | "terminal_chat_messages_eof"
                | "terminal_chat_responses_eof" => Some("upstream-id"),
                "terminal_messages"
                | "terminal_messages_responses"
                | "native_responses_terminal" => None,
                _ => Some("req-fixture"),
            };
            assert_eq!(snapshot["receipt"]["chat_id"], json!(alias), "{name}");
        }
    }
}

#[tokio::test]
async fn candidate_format_must_match_the_configured_upstream_protocol() {
    use crate::aci::upstream::tests::{
        ZeroGTerminalFixture as TerminalFixture, ZeroGTerminalMode as TerminalMode,
    };

    for endpoint in [Endpoint::ChatComplete, Endpoint::Messages] {
        for (mode, format) in [
            (TerminalMode::ForgedMessagesTerminal, "anthropic"),
            (TerminalMode::Ok, "openai"),
        ] {
            let fixture = TerminalFixture::failure(mode, "fixture-model").await;
            let mut router = ModelRouterBackend::new("terminal-test");
            router
                .add_route(
                    ModelRoute::new(
                        "fixture-model",
                        "fixture-model",
                        fixture.backend.clone(),
                        "fixture:model",
                    )
                    .unwrap(),
                )
                .unwrap();
            let snapshot = capture_with_upstream(
                &Case::new("configured_protocol", endpoint, "", format),
                Some((
                    Arc::new(router),
                    Arc::new(BoundVerifier(fixture.event.clone())),
                )),
            )
            .await;
            let matches = format == "openai";
            assert_eq!(
                snapshot["status"],
                if matches { 200 } else { 404 },
                "{snapshot}"
            );
            assert_eq!(snapshot["signature_valid"], matches, "{snapshot}");
            assert_eq!(fixture.request_calls(), usize::from(matches));
            assert_eq!(fixture.signature_calls(), usize::from(matches));
            let wire = snapshot["wire"].as_str().unwrap();
            if matches {
                assert!(wire.contains("ok"), "{snapshot}");
            } else {
                assert!(snapshot["receipt"].is_null(), "{snapshot}");
                assert!(!wire.contains("message_stop"), "{snapshot}");
                assert!(!wire.contains("[DONE]"), "{snapshot}");
                assert!(!wire.contains("ATTACKER OUTPUT"), "{snapshot}");
            }
            assert_eq!(snapshot["reports"].as_array().unwrap().len(), 1);
            assert_eq!(
                snapshot["reports"][0]["status"],
                if matches { 200 } else { 502 }
            );
            assert_eq!(
                snapshot["reports"][0]["selectedRouteId"], "fixture:model",
                "{snapshot}"
            );
        }
    }
}

#[tokio::test]
async fn configured_provider_evidence_uses_the_shaped_wire_operation() {
    for (provider, endpoint, format, source, early) in [
        (
            "openai-compatible",
            Endpoint::ChatComplete,
            "openai",
            CHAT,
            true,
        ),
        (
            "openai-compatible",
            Endpoint::Messages,
            "openai",
            CHAT,
            true,
        ),
        (
            "anthropic",
            Endpoint::ChatComplete,
            "anthropic",
            MESSAGES,
            true,
        ),
        ("anthropic", Endpoint::Messages, "anthropic", MESSAGES, true),
        (
            "openai-compatible",
            Endpoint::ChatComplete,
            "anthropic",
            MESSAGES,
            false,
        ),
        (
            "openai-compatible",
            Endpoint::Messages,
            "anthropic",
            MESSAGES,
            false,
        ),
    ] {
        let (sent, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let app = Router::new().route(
            "/custom/infer",
            post(move |Json(body): Json<Value>| {
                sent.send(body).unwrap();
                async move {
                    let source = futures_util::stream::once(async move {
                        Ok::<_, std::io::Error>(Bytes::from_static(source.as_bytes()))
                    });
                    let body = if early {
                        axum::body::Body::from_stream(source.chain(futures_util::stream::pending()))
                    } else {
                        axum::body::Body::from_stream(source)
                    };
                    ([("content-type", "text/event-stream")], body)
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = json!([{
            "name": "fixture", "provider": provider,
            "base_url": format!("http://{addr}"), "path": "/custom/infer",
            "models": {"model": "fixture-model"}
        }]);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("upstreams.json");
        std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        use crate::aggregator::upstream_config::{
            UpstreamConfigManager, UpstreamRuntimeOptions, UpstreamVerifierMode,
        };
        let manager = UpstreamConfigManager::load(
            &path,
            UpstreamRuntimeOptions {
                max_buffered_response_bytes:
                    crate::aci::upstream::MAX_BUFFERED_UPSTREAM_RESPONSE_BYTES,
                verifier_mode: UpstreamVerifierMode::None,
                accepted_subjects: vec![],
                accepted_image_digests: vec![],
                accepted_dstack_kms_root_public_keys: vec![],
                pccs_url: None,
                verifier_cache_seconds: 300,
                connect_timeout_seconds: 10,
                read_timeout_seconds: 600,
                verifier_request_timeout_seconds: 60,
                privatemode_proxy: None,
            },
        )
        .unwrap();
        let mut case = Case::new("real_backend_custom_path", endpoint, source, format);
        case.drop_at_terminal = early;
        let snapshot = capture_with_upstream(
            &case,
            Some((
                manager.backend(),
                Arc::new(crate::aci::verifier::StaticUpstreamVerifier::failed(
                    "local-http",
                    "local fixture is not attested",
                )),
            )),
        )
        .await;
        assert_eq!(snapshot["status"], 200, "{snapshot}");
        if early {
            assert_published(&snapshot, Some(source.as_bytes()), 200, false);
        } else {
            assert_eq!(snapshot["before_eof"]["receipt"], false, "{snapshot}");
            assert_eq!(snapshot["signature_valid"], true, "{snapshot}");
            assert_eq!(snapshot["reports"][0]["status"], 200, "{snapshot}");
        }
        let usage = &snapshot["reports"][0]["usage"];
        let (input, output) = if format == "anthropic" {
            ("input_tokens", "output_tokens")
        } else {
            ("prompt_tokens", "completion_tokens")
        };
        assert_eq!(usage[input], 2, "{snapshot}");
        assert_eq!(usage[output], 3, "{snapshot}");
        assert_eq!(requests.try_recv().unwrap()["model"], "fixture-model");
        server.abort();
        let _ = server.await;
    }
}

struct IncorrectResponsesDeclaration;
#[async_trait]
impl UpstreamBackend for IncorrectResponsesDeclaration {
    fn name(&self) -> &str {
        "mismatched-responses"
    }
    fn url_origin(&self) -> Option<&str> {
        None
    }
    fn response_protocol(
        &self,
        _req: &PreparedUpstreamRequest,
        operation: Endpoint,
    ) -> Option<SseProtocol> {
        assert_eq!(operation, Endpoint::CreateModelResponse);
        Some(SseProtocol::AnthropicMessages)
    }
    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        panic!("contradictory candidates must never be forwarded")
    }
}

#[tokio::test]
async fn mismatching_evidence_rejects_a_non_chat_operation() {
    let case = Case::new(
        "native_responses_mismatch",
        Endpoint::CreateModelResponse,
        "",
        "openai",
    );
    let snapshot = capture_with_upstream(
        &case,
        Some((
            Arc::new(IncorrectResponsesDeclaration),
            Arc::new(crate::aci::verifier::StaticUpstreamVerifier::failed(
                "fixture", "fixture",
            )),
        )),
    )
    .await;
    assert_eq!(snapshot["status"], 404, "{snapshot}");
    assert!(snapshot["receipt"].is_null(), "{snapshot}");
    assert_eq!(snapshot["reports"][0]["status"], 502, "{snapshot}");
    assert_eq!(
        snapshot["reports"][0]["selectedRouteId"], "fixture:model",
        "{snapshot}"
    );
    assert!(!snapshot["wire"]
        .as_str()
        .unwrap()
        .contains("response.completed"));
}

#[tokio::test]
async fn disconnect_after_published_terminal_keeps_the_continuation_failure_label() {
    let source = concat!(
        "event: error\ndata: {\"type\":\"error\",\"error\":{\"message\":\"fixture\"}}\n\n",
        "event: content_block_delta\ndata: invalid-json\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3}}\n\n",
    );
    let mut case = Case::new(
        "published_continuation_failure",
        Endpoint::ChatComplete,
        source,
        "anthropic",
    );
    case.disconnect = true;
    let snapshot = capture(&case).await;
    assert_eq!(snapshot["before_eof"]["receipt"], true, "{snapshot}");
    assert_eq!(snapshot["reports"][0]["status"], 502, "{snapshot}");
    assert_eq!(
        snapshot["reports"][0]["errorMessage"], "upstream_transport",
        "{snapshot}"
    );
    assert_eq!(
        snapshot["reports"][0]["usage"]["output_tokens"], 3,
        "{snapshot}"
    );
}

#[tokio::test]
async fn backend_protocol_declarations_accept_only_the_matching_format() {
    for (path, source, format) in [
        ("/chat/completions", CHAT, "openai"),
        ("/v1/chat/completions", CHAT, "openai"),
        ("/custom/v2/chat/completions", CHAT, "openai"),
        ("/custom/infer", CHAT, "openai"),
        ("/custom/v2/messages", CHAT, "openai"),
        ("/messages", MESSAGES, "anthropic"),
        ("/v1/messages", MESSAGES, "anthropic"),
        ("/custom/v2/messages", MESSAGES, "anthropic"),
        ("/custom/infer", MESSAGES, "anthropic"),
        ("/custom/v2/chat/completions", MESSAGES, "anthropic"),
    ] {
        for endpoint in [Endpoint::ChatComplete, Endpoint::Messages] {
            for declared in ["openai", "anthropic"] {
                let mut case = Case::new("backend_protocol", endpoint, source, declared);
                case.upstream_path = Some(path);
                case.source_protocol = Some(if format == "openai" {
                    SseProtocol::OpenaiChat
                } else {
                    SseProtocol::AnthropicMessages
                });
                case.drop_at_terminal = format == declared;
                let snapshot = capture(&case).await;
                if format == declared {
                    assert_published(&snapshot, Some(source.as_bytes()), 200, false);
                } else {
                    assert_eq!(snapshot["status"], 404, "{path}: {snapshot}");
                    assert!(snapshot["receipt"].is_null());
                    assert_eq!(snapshot["reports"].as_array().unwrap().len(), 1);
                    assert_eq!(snapshot["reports"][0]["status"], 502);
                    assert_eq!(snapshot["reports"][0]["selectedRouteId"], "fixture:model");
                }
            }
        }
    }
}

#[tokio::test]
async fn unknown_backend_protocol_keeps_forwarding_and_requires_clean_source_eof() {
    for (format, source) in [("openai", CHAT), ("anthropic", MESSAGES)] {
        for path in [
            "/custom/infer",
            "/custom/messages-extra",
            "/custom/completions",
            "/v1/chat/completions",
            "/v1/messages",
        ] {
            for disconnect in [false, true] {
                let mut case = Case::new("unknown_path", Endpoint::ChatComplete, source, format);
                case.upstream_path = Some(path);
                case.source_protocol = None;
                case.disconnect = disconnect;
                let snapshot = capture(&case).await;
                assert_eq!(snapshot["status"], 200, "{path}: {snapshot}");
                assert_eq!(
                    snapshot["before_eof"],
                    json!({"receipt":false,"report":false})
                );
                assert_eq!(snapshot["signature_valid"], !disconnect, "{snapshot}");
                assert_eq!(!snapshot["receipt"].is_null(), !disconnect);
                assert_eq!(snapshot["reports"].as_array().unwrap().len(), 1);
                assert_eq!(
                    snapshot["reports"][0]["status"],
                    if disconnect { 499 } else { 200 }
                );
            }
        }
    }
    // An opaque path can still complete in the poll that returns a bridge's
    // EOF-generated terminal: the adapter's clean end is sufficient evidence.
    for endpoint in [Endpoint::Messages, Endpoint::CreateModelResponse] {
        let source = CHAT.strip_suffix("data: [DONE]\n\n").unwrap();
        let mut case = Case::new("unknown_path_eof", endpoint, source, "openai");
        case.upstream_path = Some("/custom/infer");
        case.source_protocol = None;
        case.eof_at_source_end = true;
        case.drop_at_terminal = true;
        assert_published(&capture(&case).await, Some(source.as_bytes()), 200, false);
    }
}

// An opaque path cannot reach 0G in production: its adapter accepts only
// /v1/chat/completions. This test-only alias deliberately leaves its protocol
// unknown to exercise the generic EOF policy with the real authentication barrier.
struct PathAliasBackend(Arc<dyn UpstreamBackend>);

#[async_trait]
impl UpstreamBackend for PathAliasBackend {
    fn response_protocol(
        &self,
        _req: &crate::aci::upstream::PreparedUpstreamRequest,
        _operation: crate::middleware::request_transform::Endpoint,
    ) -> Option<crate::aci::upstream::SseProtocol> {
        None
    }

    fn name(&self) -> &str {
        self.0.name()
    }
    fn url_origin(&self) -> Option<&str> {
        self.0.url_origin()
    }
    fn requires_verified_binding(&self, req: &PreparedUpstreamRequest) -> bool {
        self.0.requires_verified_binding(req)
    }
    async fn forward(&self, req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        self.0.forward(req).await
    }
    async fn forward_stream_verified_prepared(
        &self,
        mut req: PreparedUpstreamRequest,
        event: &crate::aci::receipt::UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        assert_eq!(req.request.path.as_deref(), Some("/custom/infer"));
        req.request.path = Some(CHAT_COMPLETIONS_PATH.into());
        self.0.forward_stream_verified_prepared(req, event).await
    }
}

#[tokio::test]
async fn unknown_backend_protocol_cannot_bypass_the_real_adapter_authentication_barrier() {
    use crate::aci::upstream::tests::{
        ZeroGTerminalFixture as TerminalFixture, ZeroGTerminalMode as TerminalMode,
    };
    for endpoint in [Endpoint::ChatComplete, Endpoint::Messages] {
        let fixture =
            TerminalFixture::failure(TerminalMode::ForgedMessagesTerminal, "fixture-model").await;
        let mut router = ModelRouterBackend::new("terminal-test");
        router
            .add_route(
                ModelRoute::new(
                    "fixture-model",
                    "fixture-model",
                    Arc::new(PathAliasBackend(fixture.backend.clone())),
                    "fixture:model",
                )
                .unwrap()
                .with_path(Some("/custom/infer".into())),
            )
            .unwrap();
        let snapshot = capture_with_upstream(
            &Case::new("unknown_path_authentication", endpoint, "", "anthropic"),
            Some((
                Arc::new(router),
                Arc::new(BoundVerifier(fixture.event.clone())),
            )),
        )
        .await;
        assert_eq!(snapshot["status"], 200, "{snapshot}");
        assert_eq!(snapshot["before_eof"]["receipt"], false);
        assert!(snapshot["receipt"].is_null(), "{snapshot}");
        assert_eq!(fixture.request_calls(), 1);
        assert_eq!(fixture.signature_calls(), 0);
        assert_eq!(snapshot["reports"].as_array().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn terminal_source_hash_excludes_suffix_while_billing_keeps_observed_usage() {
    let suffix = "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":99,\"completion_tokens\":99}}\n\ndata: not JSON\n\n";
    for ending in ["\n", "\r\n", "\r"] {
        let prefix = CHAT.replace('\n', ending);
        let source = format!("{prefix}{suffix}");
        // SSE dispatches at CR; the LF following that CR is outside the accepted
        // terminal block, including when both bytes arrive in one network chunk.
        let received = if ending == "\r\n" {
            &prefix[..prefix.len() - 1]
        } else {
            &prefix
        };
        for chunk_size in [1, 2, 7, prefix.len() - 1, prefix.len(), source.len()] {
            for drop_at_terminal in [false, true] {
                let mut case =
                    Case::new("terminal_suffix", Endpoint::ChatComplete, &source, "openai");
                case.chunk_size = chunk_size;
                case.drop_at_terminal = drop_at_terminal;
                let snapshot = capture(&case).await;
                assert_published(&snapshot, Some(received.as_bytes()), 200, false);
                assert_eq!(snapshot["before_eof"]["report"], drop_at_terminal);
                assert_eq!(
                    snapshot["reports"][0]["usage"]["prompt_tokens"],
                    if drop_at_terminal { 2 } else { 99 }
                );
                assert_eq!(
                    snapshot["reports"][0]["usage"]["completion_tokens"],
                    if drop_at_terminal { 3 } else { 99 }
                );
                assert!(!snapshot["wire"].as_str().unwrap().contains("99"));
            }
        }
    }
    let mut late_id = Case::new(
        "terminal_before_identity",
        Endpoint::ChatComplete,
        &format!("data: [DONE]\n\n{CHAT}"),
        "openai",
    );
    late_id.drop_at_terminal = true;
    let snapshot = capture(&late_id).await;
    assert_published(&snapshot, Some(b"data: [DONE]\n\n"), 200, false);
    assert!(snapshot["receipt"]["chat_id"].is_null());
    assert!(snapshot["reports"][0]["usage"].is_null());
}

#[tokio::test]
async fn post_terminal_e2ee_failure_keeps_observing_source_usage() {
    let source = format!("{CHAT}data: invalid JSON\n\ndata: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":99,\"completion_tokens\":99}}}}\n\n");
    let mut case = Case::new(
        "terminal_e2ee_failure",
        Endpoint::ChatComplete,
        &source,
        "openai",
    );
    case.e2ee = true;
    let snapshot = capture(&case).await;
    assert_published(&snapshot, Some(CHAT.as_bytes()), 502, false);
    assert!(snapshot["wire"]
        .as_str()
        .unwrap()
        .ends_with("data: [DONE]\n\n"));
    assert_eq!(snapshot["reports"][0]["errorSource"], "gateway");
    assert_eq!(
        snapshot["reports"][0]["errorMessage"],
        "downstream_finalizer_failed"
    );
    assert_eq!(snapshot["before_eof"]["report"], false);
    assert_eq!(snapshot["reports"][0]["usage"]["prompt_tokens"], 99);
    assert_eq!(snapshot["reports"][0]["usage"]["completion_tokens"], 99);
}

#[tokio::test]
async fn terminal_publication_keeps_upstream_response_metrics_at_source_end() {
    for direct in [false, true] {
        for drop_at_terminal in [false, true] {
            let mut case = Case::new("terminal_metric", Endpoint::ChatComplete, CHAT, "openai");
            case.direct = direct;
            case.drop_at_terminal = drop_at_terminal;
            case.metrics = true;
            let snapshot = capture(&case).await;
            assert_published(&snapshot, (!direct).then_some(CHAT.as_bytes()), 200, direct);
            let series: Vec<_> = snapshot["metrics"]
                .as_str()
                .unwrap()
                .lines()
                .filter(|line| line.starts_with("private_ai_gateway_upstream_responses_total{"))
                .collect();
            if drop_at_terminal {
                assert!(series.is_empty(), "{snapshot}");
            } else {
                assert_eq!(series.len(), 1, "{snapshot}");
                assert!(
                    series[0].contains("model_id=\"upstream-model\""),
                    "{snapshot}"
                );
                assert!(series[0].ends_with(" 1"), "{snapshot}");
            }
        }
    }
}

#[tokio::test]
async fn failure_terminals_publish_before_immediate_drop() {
    for mut case in review_cases().into_iter().filter(|case| {
        (case.name.starts_with("failure_") || case.name.starts_with("native_responses_"))
            && !matches!(
                case.name.as_str(),
                "failure_chat_messages" | "failure_chat_responses" | "failure_legacy_completion"
            )
    }) {
        case.drop_at_terminal = true;
        let snapshot = capture(&case).await;
        assert_published(&snapshot, None, 502, false);
        assert_eq!(
            snapshot["reports"][0]["errorMessage"],
            "stream_inband_error"
        );
    }
}

#[tokio::test]
async fn bridge_failure_without_source_terminal_keeps_eof_completion_and_usage() {
    let content = CHAT.strip_suffix("data: [DONE]\n\n").unwrap();
    for endpoint in [Endpoint::Messages, Endpoint::CreateModelResponse] {
        for terminal in ["", "data: [DONE]\n\n"] {
            for transport_error in [false, true] {
                for early in [false, true] {
                    let source = format!("{content}data: {{\"error\":{{\"type\":\"api_error\",\"message\":\"failure\"}}}}\n\ndata: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":7,\"completion_tokens\":8,\"total_tokens\":15}}}}\n\n{terminal}");
                    let mut case = Case::new("bridge_failure_eof", endpoint, &source, "openai");
                    case.early = early;
                    case.read_error = transport_error.then_some(ReadError::Reset);
                    let snapshot = capture(&case).await;
                    assert_eq!(
                        snapshot["before_eof"],
                        json!({"receipt":false,"report":false})
                    );
                    assert_eq!(!snapshot["receipt"].is_null(), !transport_error);
                    assert_eq!(snapshot["reports"].as_array().unwrap().len(), 1);
                    assert_eq!(snapshot["reports"][0]["status"], 502);
                    assert_eq!(
                        snapshot["reports"][0]["errorMessage"],
                        if transport_error {
                            "upstream_transport"
                        } else {
                            "stream_inband_error"
                        }
                    );
                    assert_eq!(
                        snapshot["reports"][0]["usage"]["prompt_tokens"], 7,
                        "{snapshot}"
                    );
                    assert_eq!(snapshot["reports"][0]["usage"]["completion_tokens"], 8);
                    if !transport_error {
                        assert_eq!(snapshot["signature_valid"], true);
                        assert_eq!(
                            event_hash(&snapshot, "response.received"),
                            &json!(crate::aci::digest::sha256_hex(source.as_bytes()))
                        );
                    }
                }
            }
        }
    }
}

struct BoundVerifier(crate::aci::receipt::UpstreamVerifiedEvent);

#[async_trait]
impl UpstreamVerifier for BoundVerifier {
    async fn verify(
        &self,
        request: UpstreamVerificationRequest,
    ) -> crate::aci::receipt::UpstreamVerifiedEvent {
        crate::aci::receipt::UpstreamVerifiedEvent {
            upstream_name: request.upstream_name,
            url_origin: request.url_origin,
            required: request.required,
            evidence: Some(common::test_evidence()),
            ..self.0.clone()
        }
    }
}

#[tokio::test]
async fn real_zero_g_failure_terminals_cannot_bypass_response_authentication() {
    use crate::aci::upstream::{
        tests::{ZeroGTerminalFixture as TerminalFixture, ZeroGTerminalMode as TerminalMode},
        ModelRoute, ModelRouterBackend,
    };

    for endpoint in [
        Endpoint::ChatComplete,
        Endpoint::Messages,
        Endpoint::CreateModelResponse,
    ] {
        for mode in [
            TerminalMode::ForgedFailure,
            TerminalMode::ForgedFailureFinal,
            TerminalMode::SignedFailure,
        ] {
            let fixture = TerminalFixture::failure(mode, "fixture-model").await;
            let mut router = ModelRouterBackend::new("terminal-test");
            router
                .add_route(
                    ModelRoute::new(
                        "fixture-model",
                        "fixture-model",
                        fixture.backend.clone(),
                        "fixture:model",
                    )
                    .unwrap(),
                )
                .unwrap();
            let case = Case::new(
                &format!("zero_g_failure_{endpoint:?}_{mode:?}"),
                endpoint,
                "",
                "openai",
            );
            let snapshot = capture_with_upstream(
                &case,
                Some((
                    Arc::new(router),
                    Arc::new(BoundVerifier(fixture.event.clone())),
                )),
            )
            .await;
            let authenticated = mode == TerminalMode::SignedFailure;
            assert_eq!(
                !snapshot["receipt"].is_null(),
                authenticated,
                "{mode:?}: {snapshot}"
            );
            // A missing final frame is rejected before fetching a signature.
            // With a final frame the broker must be consulted, even though a
            // bridge has already emitted its destination failure terminal.
            assert_eq!(
                fixture.signature_calls(),
                usize::from(mode != TerminalMode::ForgedFailure),
                "{mode:?}"
            );
            assert!(
                snapshot["wire"]
                    .as_str()
                    .unwrap()
                    .contains("ATTACKER OUTPUT"),
                "{mode:?}: {snapshot}"
            );
            assert_eq!(snapshot["reports"].as_array().unwrap().len(), 1);
            assert_eq!(snapshot["reports"][0]["status"], 502);
            assert_eq!(
                snapshot["reports"][0]["errorMessage"],
                if authenticated {
                    "stream_inband_error"
                } else {
                    "upstream_transport"
                }
            );
            assert_eq!(snapshot["signature_valid"], authenticated);
        }
    }
}

#[tokio::test]
async fn synthesized_and_encrypted_terminals_publish_in_the_yielding_poll() {
    for direct in [false, true] {
        for e2ee in [false, true] {
            for synthesized in [false, true] {
                let source = if synthesized {
                    CHAT.strip_suffix("data: [DONE]\n\n").unwrap()
                } else {
                    CHAT
                };
                let mut case = Case::new("terminal_e2ee", Endpoint::ChatComplete, source, "openai");
                case.direct = direct;
                case.e2ee = e2ee;
                case.drop_at_terminal = true;
                case.eof_at_source_end = synthesized;
                let snapshot = capture(&case).await;
                assert_published(
                    &snapshot,
                    (!direct).then_some(source.as_bytes()),
                    200,
                    direct,
                );
                if direct {
                    assert_eq!(snapshot["receipt"]["chat_id"], "upstream-id");
                }
            }
        }
    }
    let source = CHAT.strip_suffix("data: [DONE]\n\n").unwrap();
    let mut case = Case::new(
        "early_synthesized",
        Endpoint::ChatComplete,
        source,
        "openai",
    );
    case.early = true;
    case.drop_at_terminal = true;
    case.eof_at_source_end = true;
    assert_published(&capture(&case).await, Some(source.as_bytes()), 200, false);
}

#[tokio::test]
async fn disconnect_before_terminal_and_mid_event_keeps_usage_without_receipt() {
    let content = CHAT.strip_suffix("data: [DONE]\n\n").unwrap();
    for early in [false, true] {
        for tail in ["", "data: [DO", "data: {\"unfinished\":"] {
            let mut case = Case::new(
                "before_terminal_disconnect",
                Endpoint::ChatComplete,
                &format!("{content}{tail}"),
                "openai",
            );
            case.early = early;
            case.disconnect = true;
            let snapshot = capture(&case).await;
            assert!(snapshot["receipt"].is_null());
            assert_eq!(
                snapshot["before_eof"],
                json!({"receipt":false,"report":false})
            );
            assert_eq!(snapshot["reports"].as_array().unwrap().len(), 1);
            assert_eq!(snapshot["reports"][0]["status"], 499);
            assert_eq!(snapshot["reports"][0]["usage"]["completion_tokens"], 3);
        }
    }
}

#[tokio::test]
async fn source_read_and_decoder_failures_preserve_receipt_eligibility() {
    for direct in [false, true] {
        let mut case = Case::new(
            "terminal_read_failure",
            Endpoint::ChatComplete,
            "data: {\"choices\":[]}\n\n",
            "openai",
        );
        case.direct = direct;
        case.read_error = Some(ReadError::Reset);
        let snapshot = capture(&case).await;
        assert_eq!(!snapshot["receipt"].is_null(), direct);
        assert!(snapshot["wire"]
            .as_str()
            .unwrap()
            .ends_with("data: [DONE]\n\n"));
        if !direct {
            assert_eq!(snapshot["reports"].as_array().unwrap().len(), 1);
            assert_eq!(snapshot["reports"][0]["status"], 502);
            assert_eq!(snapshot["reports"][0]["errorMessage"], "upstream_transport");
        }
    }
    for (endpoint, source) in [
        (
            Endpoint::ChatComplete,
            "event: message_stop\ndata: {\"type\":\"error\"}\n\n",
        ),
        (Endpoint::Messages, "data: not JSON\n\n"),
    ] {
        let case = Case::new("terminal_decode_failure", endpoint, source, "openai");
        let snapshot = capture(&case).await;
        assert!(snapshot["receipt"].is_null());
        assert_eq!(snapshot["reports"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["reports"][0]["status"], 502);
        assert_eq!(snapshot["reports"][0]["errorMessage"], "upstream_transport");
    }
}

#[tokio::test]
async fn publication_failure_withholds_native_terminal_on_normal_early_and_encrypted_paths() {
    for (name, endpoint, source, format) in [
        ("terminal_publish_failure", Endpoint::ChatComplete, CHAT, "openai"),
        ("terminal_publish_failure", Endpoint::Messages, MESSAGES, "anthropic"),
        ("native_responses_publish_failure", Endpoint::CreateModelResponse,
         "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3}}}\n\n", "openai"),
    ] {
        for early in [false, true] {
            let mut case = Case::new(name, endpoint, source, format);
            case.early = early;
            case.fail_publish = true;
            let snapshot = capture(&case).await;
            assert!(snapshot["receipt"].is_null());
            assert_eq!(snapshot["before_eof"]["report"], false);
            assert_eq!(snapshot["reports"].as_array().unwrap().len(), 1);
            assert_eq!(snapshot["reports"][0]["status"], 502);
            assert_eq!(snapshot["reports"][0]["errorMessage"], "downstream_finalizer_failed");
            let records = crate::sse::decode_buffered(Bytes::from(snapshot["wire"].as_str().unwrap().to_string()), Some(endpoint.into())).unwrap();
            assert!(!records.iter().any(|r| r.event().is_some_and(|e| e.meaning.terminal.is_some())));
        }
    }
    let mut case = Case::new(
        "encrypted_publish_failure",
        Endpoint::ChatComplete,
        CHAT,
        "openai",
    );
    case.e2ee = true;
    case.fail_publish = true;
    let snapshot = capture(&case).await;
    assert!(snapshot["receipt"].is_null());
    assert!(!snapshot["wire"].as_str().unwrap().contains("[DONE]"));
    assert_eq!(snapshot["reports"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["reports"][0]["status"], 502);
}
