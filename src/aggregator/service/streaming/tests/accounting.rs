//! Accounting regression tests use the same typed stages as the HTTP pipeline.
use super::*;
use crate::middleware::config::MiddlewareConfig;
use crate::middleware::control::ControlClient;
use crate::middleware::sse::StreamReport;
use crate::middleware::types::{OrganizationScope, TenantIdentity};
use axum::{routing::post, Json, Router};
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Control {
    client: ControlClient,
    reports: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Control {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Control {
    async fn new() -> Self {
        let reports = Arc::new(Mutex::new(Vec::new()));
        let captured = reports.clone();
        let app = Router::new().route(
            "/consult/post",
            post(move |Json(value): Json<Value>| {
                let reports = captured.clone();
                async move {
                    reports.lock().unwrap().push(value);
                    axum::http::StatusCode::OK
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = ControlClient::new(&MiddlewareConfig {
            control_url: url,
            control_token: None,
            control_timeout_ms: Some(2000),
            control_post_timeout_ms: Some(2000),
            sse_keepalive_ms: None,
            send_request_features: None,
            tee_only_domains: Vec::new(),
        })
        .unwrap();
        Self {
            client,
            reports,
            task,
        }
    }
    fn report(&self) -> StreamReport {
        StreamReport {
            control: self.client.clone(),
            request_id: "fixture-request".into(),
            endpoint: "/v1/chat/completions".into(),
            request_model: "fixture".into(),
            pricing: Some(json!({"inputCostPerToken":"1", "outputCostPerToken":"2"})),
            spend_mode: None,
            tenant: TenantIdentity {
                user_id: Some(9),
                organization: Some(OrganizationScope {
                    organization_id: 11,
                    workspace_id: 13,
                }),
            },
            virtual_key_id: None,
            selected_route_id: Some("fixture:route".into()),
            attempt_index: 0,
            upstream_status: 200,
            prefix_hash: None,
            session_kind: None,
            started: Instant::now(),
        }
    }
    async fn captured(&self) -> Value {
        for _ in 0..100 {
            if let Some(value) = self.reports.lock().unwrap().first() {
                return value.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("accounting report not received")
    }
}
fn owner(inner: ServiceResponseStream, report: StreamReport) -> ResponseOwner {
    owner_for(inner, report, SseProtocol::OpenaiChat)
}
fn owner_for(
    inner: ServiceResponseStream,
    mut report: StreamReport,
    protocol: SseProtocol,
) -> ResponseOwner {
    let endpoint = match protocol {
        SseProtocol::OpenaiChat => "/v1/chat/completions",
        SseProtocol::AnthropicMessages => "/v1/messages",
        SseProtocol::OpenaiResponses => "/v1/responses",
    };
    report.endpoint = endpoint.into();
    let accounting = Accounting::new(report);
    let events = decode_source(
        inner,
        protocol,
        MiddlewareReceiptJournal::default(),
        accounting.handle.clone(),
        accounting.control.clone(),
    );
    ResponseOwner::new(
        ResponseInput::Events(events),
        ResponseMode::Middleware(accounting),
        None,
        endpoint.into(),
        Some(protocol),
        None,
    )
}

#[tokio::test]
async fn typed_accounting_injects_cost_reports_original_usage_and_tenant() {
    let control = Control::new().await;
    let wire = b"data:{\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":20,\"reasoning_tokens\":5,\"completion_tokens_details\":null}}\n\ndata: ping\n\ndata:[DONE]\n\n";
    let chunks: Vec<_> = wire
        .chunks(7)
        .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
        .collect();
    let source = Box::pin(futures_util::stream::iter(chunks));
    let output: Vec<_> = owner(source, control.report()).collect().await;
    let bytes: Vec<_> = output.into_iter().flat_map(Result::unwrap).collect();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("\"cost\":50"));
    assert!(text.contains("\"completion_tokens_details\":{\"reasoning_tokens\":5}"));
    assert!(text.contains("data: ping\n\n"));
    let report = control.captured().await;
    assert_eq!(report["status"], 200);
    assert_eq!(report["usage"]["prompt_tokens"], 10);
    assert_eq!(report["usage"]["completion_tokens"], 20);
    assert_eq!(
        report["usage"]["completion_tokens_details"]["reasoning_tokens"],
        5
    );
    assert!(report["usage"].get("cost").is_none());
    assert!(report["ttftMs"].is_number());
    assert_eq!(
        (
            report["userId"].clone(),
            report["organizationId"].clone(),
            report["workspaceId"].clone()
        ),
        (json!(9), json!(11), json!(13))
    );
}

#[tokio::test]
async fn only_an_exact_dispatched_terminal_rescues_a_read_timeout() {
    use SseProtocol::{
        AnthropicMessages as Messages, OpenaiChat as Chat, OpenaiResponses as Responses,
    };
    for (protocol, wire, status) in [
        (Chat, "data: [DONE]\n\n", 200),
        (Chat, "data: [DONE]\n", 504),
        (
            Chat,
            "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n",
            504,
        ),
        (Chat, "data: [DONE] \n\n", 504),
        (Chat, "data:\ndata: [DONE]\n\n", 504),
        (Chat, "[DONE]\n\n", 504),
        (Chat, "{\"type\":\"message_stop\"}\n\n", 504),
        (Messages, "data: [DONE]\n\n", 504),
        (
            Messages,
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            504,
        ),
        (
            Messages,
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            200,
        ),
        (
            Responses,
            "data: {\"type\":\"response.completed\",\"sequence_number\":7}\n\n",
            200,
        ),
    ] {
        let control = Control::new().await;
        let source = Box::pin(futures_util::stream::iter([
            Ok(Bytes::from(wire)),
            Err(ServiceError::Upstream(
                crate::aci::upstream::UpstreamError::Timeout("read deadline".into()),
            )),
        ]));
        let mut stream = owner_for(source, control.report(), protocol);
        while stream.next().await.is_some() {}
        let report = control.captured().await;
        assert_eq!(report["status"], status, "{protocol:?} {wire:?}");
        if status == 504 {
            assert_eq!(report["errorMessage"], "upstream_timeout");
        }
    }
}

#[tokio::test]
async fn typed_accounting_preserves_eof_boundaries_and_inband_failures_on_every_protocol() {
    use SseProtocol::{
        AnthropicMessages as Messages, OpenaiChat as Chat, OpenaiResponses as Responses,
    };
    for (protocol, wire, status, class) in [
        (Chat, "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n", 200, None),
        (Chat, "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n", 200, None),
        (Messages, "event: content_block_delta\ndata: {\"type\":\"content_block_delta\"}\n\n", 200, None),
        (Responses, "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":1}\n\n", 200, None),
        (Chat, "data: {\"choices\":[{\"delta\":{}}]}\n", 200, None),
        (Chat, "data: {\"choices\":[", 502, Some("stream_truncated")),
        (Chat, "event: custom\n", 200, None),
        (Chat, "event: custom\n\n", 200, None),
        (Chat, "data: {\"choices\":[{\"finish_reason\":\"upstream_error\"}]}\n\ndata: [DONE]\n\n", 502, Some("stream_inband_error")),
        (Messages, "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"error\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n", 502, Some("stream_inband_error")),
        (Responses, "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"CANARY-private at https://provider.test\"}}}\n\n", 502, Some("stream_inband_error")),
    ] {
        let control = Control::new().await;
        let chunks: Vec<_> = wire.as_bytes().chunks(7).map(|b| Ok(Bytes::copy_from_slice(b))).collect();
        let source = Box::pin(futures_util::stream::iter(chunks));
        let output: Vec<_> = owner_for(source, control.report(), protocol).collect().await;
        let bytes: Vec<_> = output.into_iter().flat_map(Result::unwrap).collect();
        let report = control.captured().await;
        assert_eq!(report["status"], status, "{protocol:?} {wire:?}");
        assert_eq!(report["errorMessage"].as_str(), class, "{protocol:?} {wire:?}");
        assert!(!report.to_string().contains("CANARY"));
        if status == 200 && protocol == Chat {
            assert!(bytes.ends_with(b"data: [DONE]\n\n"));
        } else if status == 200 {
            assert_eq!(bytes, wire.as_bytes(), "no fabricated terminal after {wire:?}");
        } else if class == Some("stream_inband_error") {
            let text = String::from_utf8(bytes).unwrap();
            assert!(!text.contains("CANARY"));
            match protocol {
                Chat | Messages => assert_eq!(text, wire, "native failure content is preserved"),
                Responses => assert!(text.starts_with("event: response.failed\n")),
            }
        } else {
            assert!(String::from_utf8(bytes).unwrap().contains("\"error\""), "discard pending data and emit the protocol error tail");
        }
        assert_eq!(control.reports.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn fragmented_comments_do_not_start_ttft() {
    for content in [false, true] {
        let control = Control::new().await;
        let mut chunks = vec![
            Ok(Bytes::from_static(b": keep-al")),
            Ok(Bytes::from_static(b"ive\n")),
        ];
        if content {
            chunks.push(Ok(Bytes::from_static(b"data: {\"choices\":[]}\n\n")));
        }
        let source = Box::pin(futures_util::stream::iter(chunks));
        let output: Vec<_> = owner(source, control.report()).collect().await;
        let bytes: Vec<_> = output.into_iter().flat_map(Result::unwrap).collect();
        assert!(!String::from_utf8(bytes).unwrap().contains("keep-alive"));
        let report = control.captured().await;
        assert_eq!(report["status"], 200);
        assert_eq!(report["ttftMs"].is_number(), content);
    }
}

#[tokio::test]
async fn event_overflow_reports_an_upstream_failure_and_retains_partial_usage() {
    let control = Control::new().await;
    let mut oversized = b"data: {}\n".to_vec();
    oversized.resize(crate::sse::MAX_EVENT_BYTES + 1, b'x');
    let source = Box::pin(futures_util::stream::iter([
        Ok(Bytes::from_static(
            b"data: {\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":9}}\n\n",
        )),
        Ok(Bytes::from(oversized)),
    ]));
    let output: Vec<_> = owner(source, control.report()).collect().await;
    let bytes: Vec<_> = output.into_iter().flat_map(Result::unwrap).collect();
    assert!(bytes.ends_with(b"data: [DONE]\n\n"));
    assert!(String::from_utf8(bytes).unwrap().contains("\"error\""));
    let report = control.captured().await;
    assert_eq!(report["status"], 502);
    assert_eq!(report["errorMessage"], "stream_line_overflow");
    assert_ne!(report["errorSource"], "gateway");
    assert_eq!(report["usage"]["prompt_tokens"], 3);
    assert_eq!(report["usage"]["completion_tokens"], 9);
    assert_eq!(control.reports.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn response_owner_settles_once_on_cancellation_completion_and_e2ee_failure() {
    for kind in ["cancel", "complete", "e2ee"] {
        let control = Control::new().await;
        let source: ServiceResponseStream = if kind == "cancel" {
            Box::pin(futures_util::stream::pending())
        } else {
            Box::pin(futures_util::stream::iter([Ok(Bytes::from(
                if kind == "complete" {
                    "data: [DONE]\n\n"
                } else {
                    "data: {\"choices\":[]}\n\ndata: invalid JSON\n\n"
                },
            ))]))
        };
        let mut stream = owner(source, control.report());
        if kind == "e2ee" {
            stream.e2ee = Some(E2eeRequestContext {
                version: "2".into(),
                algo: crate::aci::e2ee::E2EE_ALGO_X25519_AESGCM.into(),
                aad_mode: E2eeAadMode::AciV2,
                request_model: "fixture".into(),
                client_public_key_hex: "11".repeat(32),
                nonce: None,
                timestamp: None,
            });
            assert!(stream.next().await.unwrap().is_ok());
            assert!(
                control.reports.lock().unwrap().is_empty(),
                "source progress cannot settle the response"
            );
            assert!(matches!(
                stream.next().await,
                Some(Err(ServiceError::E2ee(_)))
            ));
        } else if kind == "complete" {
            while stream.next().await.is_some() {}
        }
        drop(stream);
        let report = control.captured().await;
        assert_eq!(
            report["status"],
            match kind {
                "cancel" => 499,
                "complete" => 200,
                _ => 502,
            }
        );
        if kind == "e2ee" {
            assert_eq!(report["errorSource"], "gateway");
            assert_eq!(report["errorMessage"], "downstream_finalizer_failed");
        }
        assert_eq!(control.reports.lock().unwrap().len(), 1);
    }
}

async fn transformed_response(
    control: &Control,
    wire: String,
    source_protocol: SseProtocol,
    endpoint: crate::middleware::request_transform::Endpoint,
    read_timeout: bool,
) -> String {
    use crate::middleware::request_transform::Endpoint;
    use crate::middleware::response_transform::ResponseIdentity;
    use crate::middleware::stream_transform::{EventTransformStream, StreamTransform};
    let endpoint_path = match endpoint {
        Endpoint::CreateModelResponse => "/v1/responses",
        Endpoint::Messages => "/v1/messages",
        Endpoint::Complete => "/v1/completions",
        _ => "/v1/chat/completions",
    };
    let mut report = control.report();
    report.endpoint = endpoint_path.into();
    let accounting = Accounting::new(report);
    let mut chunks = vec![Ok(Bytes::from(wire))];
    if read_timeout {
        chunks.push(Err(ServiceError::Upstream(
            crate::aci::upstream::UpstreamError::Timeout("read deadline".into()),
        )));
    }
    let body = Box::pin(futures_util::stream::iter(chunks));
    let identity = Arc::new(ResponseIdentity {
        request_id: "req-fixture".into(),
        user_model: Some("fixture".into()),
    });
    let mut events = decode_source(
        body,
        source_protocol,
        MiddlewareReceiptJournal::default(),
        accounting.handle.clone(),
        accounting.control.clone(),
    );
    if endpoint == Endpoint::Complete {
        let bridge = EventTransformStream::new(events, StreamTransform::AnthropicCompleteToOpenai);
        accounting.control.bridge(bridge.bridge());
        events = Box::pin(bridge);
    } else if source_protocol == SseProtocol::AnthropicMessages && endpoint != Endpoint::Messages {
        let bridge = EventTransformStream::new(events, StreamTransform::AnthropicToOpenaiChat);
        accounting.control.bridge(bridge.bridge());
        events = Box::pin(bridge);
    }
    if endpoint == Endpoint::Messages && source_protocol != SseProtocol::AnthropicMessages {
        let bridge = EventTransformStream::new(events, StreamTransform::OpenaiToAnthropicMessages);
        accounting.control.bridge(bridge.bridge());
        events = Box::pin(bridge);
    }
    if endpoint == Endpoint::CreateModelResponse && source_protocol != SseProtocol::OpenaiResponses
    {
        let bridge = EventTransformStream::new(
            events,
            StreamTransform::OpenaiChatToResponses(Arc::new(json!({})), identity.clone()),
        );
        accounting.control.bridge(bridge.bridge());
        events = Box::pin(bridge);
    }
    let sanitized = Box::pin(EventTransformStream::new(
        events,
        StreamTransform::SanitizeResponse(identity, endpoint),
    ));
    let output: Vec<_> = ResponseOwner::new(
        ResponseInput::Events(sanitized),
        ResponseMode::Middleware(accounting),
        None,
        endpoint_path.into(),
        Some(SseProtocol::from(
            crate::middleware::request_transform::Endpoint::from_path(endpoint_path),
        )),
        None,
    )
    .collect()
    .await;
    String::from_utf8(output.into_iter().flat_map(Result::unwrap).collect()).unwrap()
}

#[tokio::test]
async fn discarded_continuations_use_bridge_validation_and_keep_source_usage() {
    use crate::middleware::request_transform::Endpoint;
    for endpoint in [Endpoint::Messages, Endpoint::CreateModelResponse] {
        for source_terminated in [false, true] {
            let control = Control::new().await;
            let terminal = if source_terminated {
                "data: [DONE]\n\n"
            } else {
                ""
            };
            let source = format!(concat!(
                "data: {{\"error\":{{\"message\":\"private\"}}}}\n\n{}",
                "data: not JSON\n\n",
                "data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":2,\"completion_tokens\":3}}}}\n\n",
                "data: [DONE]\n\n",
            ), terminal);
            let output =
                transformed_response(&control, source, SseProtocol::OpenaiChat, endpoint, false)
                    .await;
            let terminal = if endpoint == Endpoint::Messages {
                "event: error\n"
            } else {
                "event: response.failed\n"
            };
            assert!(output.starts_with(terminal), "{output}");
            assert_eq!(output.matches("\n\n").count(), 1, "{output}");
            let report = control.captured().await;
            assert_eq!(report["status"], 502);
            assert_eq!(
                report["errorMessage"],
                if source_terminated {
                    "stream_inband_error"
                } else {
                    "upstream_transport"
                }
            );
            assert_eq!(
                report["usage"],
                json!({"prompt_tokens":2,"completion_tokens":3})
            );
            assert_eq!(control.reports.lock().unwrap().len(), 1);
        }
    }
    for endpoint in [Endpoint::ChatComplete, Endpoint::CreateModelResponse] {
        for malformed in [false, true] {
            let control = Control::new().await;
            let continuation = if malformed {
                "data: not JSON\n\n"
            } else {
                "event: ping\ndata: not JSON\n\nevent: content_block_stop\ndata: not JSON\n\n"
            };
            let source = format!(concat!(
                "event: error\ndata: {{\"type\":\"error\",\"error\":{{\"message\":\"private\"}}}}\n\n{}",
                "event: message_delta\ndata: {{\"type\":\"message_delta\",\"usage\":{{\"output_tokens\":3}}}}\n\n",
                "event: message_stop\ndata: not JSON\n\n",
            ), continuation);
            let output = transformed_response(
                &control,
                source,
                SseProtocol::AnthropicMessages,
                endpoint,
                false,
            )
            .await;
            assert!(!output.contains("not JSON"), "{output}");
            if endpoint == Endpoint::ChatComplete {
                assert_eq!(output.matches("data: [DONE]\n\n").count(), 1);
                assert!(output.ends_with("data: [DONE]\n\n"));
                assert_eq!(output.matches("\n\n").count(), 2, "{output}");
            } else {
                assert!(output.starts_with("event: response.failed\n"), "{output}");
                assert_eq!(output.matches("\n\n").count(), 1, "{output}");
            }
            let report = control.captured().await;
            assert_eq!(report["status"], 502);
            assert_eq!(
                report["errorMessage"],
                if malformed {
                    "upstream_transport"
                } else {
                    "stream_inband_error"
                }
            );
            assert_eq!(report["usage"], json!({"output_tokens":3}));
            assert_eq!(control.reports.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn payload_response_error_stays_failed_with_default_or_unknown_event_name() {
    use crate::middleware::request_transform::Endpoint;
    for (name, prefix, status) in [
        ("message", "", 502), ("extension", "", 502),
        ("message", "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n", 200),
    ] {
        let control = Control::new().await;
        let source = format!("{prefix}event: {name}\ndata: {{\"type\":\"response.error\",\"sequence_number\":2,\"message\":\"private upstream detail\"}}\n\n");
        let output = transformed_response(
            &control,
            source,
            SseProtocol::OpenaiResponses,
            Endpoint::CreateModelResponse,
            false,
        )
        .await;
        assert!(output.contains("event: error\n"), "{output}");
        assert!(output.contains("\"type\":\"error\""), "{output}");
        if prefix.is_empty() { assert!(!output.contains("response.completed")); }
        assert_eq!(control.captured().await["status"], status);
        assert_eq!(control.reports.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn filtered_anthropic_error_evidence_reaches_client_and_single_report() {
    use crate::middleware::request_transform::Endpoint;
    for source in [
        "event: error\ndata:\n\n",
        "event: ping\ndata: {\"type\":\"ping\",\"error\":{\"message\":\"private detail\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ] {
        let control = Control::new().await;
        let output = transformed_response(&control, source.into(), SseProtocol::AnthropicMessages, Endpoint::ChatComplete, false).await;
        assert!(output.contains("\"error\":"), "{output}");
        assert!(output.ends_with("data: [DONE]\n\n"), "{output}");
        assert_eq!(output.matches("data: [DONE]").count(), 1);
        let report = control.captured().await;
        assert_eq!(report["status"], 502);
        assert_eq!(report["errorMessage"], "stream_inband_error");
        assert_eq!(control.reports.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn failed_anthropic_terminal_becomes_responses_failure_terminal() {
    use crate::middleware::request_transform::Endpoint;
    let control = Control::new().await;
    let source = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"fixture\",\"usage\":{\"input_tokens\":1}}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\",\"error\":{\"message\":\"private detail\"}}\n\n";
    let output = transformed_response(
        &control,
        source.into(),
        SseProtocol::AnthropicMessages,
        Endpoint::CreateModelResponse,
        false,
    )
    .await;
    assert!(output.contains("event: response.failed\n"), "{output}");
    assert!(output.contains("\"status\":\"failed\""), "{output}");
    assert!(!output.contains("response.completed"));
    assert_eq!(control.captured().await["status"], 502);
    assert_eq!(control.reports.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn data_only_anthropic_terminal_rescues_timeout_after_bridge() {
    use crate::middleware::request_transform::Endpoint;
    let control = Control::new().await;
    let output = transformed_response(
        &control,
        "data: {\"type\":\"message_stop\"}\n\n".into(),
        SseProtocol::AnthropicMessages,
        Endpoint::ChatComplete,
        true,
    )
    .await;
    assert_eq!(output, "data: [DONE]\n\n");
    let report = control.captured().await;
    assert_eq!(report["status"], 200);
    assert!(report["errorMessage"].is_null());
    assert_eq!(control.reports.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn native_chat_event_under_ingress_cap_is_counted_once_by_transforms() {
    use crate::middleware::request_transform::Endpoint;
    let control = Control::new().await;
    let content = "x".repeat(9 * 1024 * 1024);
    let source = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{content}\"}}}}]}}\n\ndata: [DONE]\n\n"
    );
    let output = transformed_response(
        &control,
        source,
        SseProtocol::OpenaiChat,
        Endpoint::ChatComplete,
        false,
    )
    .await;
    assert!(output.contains(&content));
    assert!(output.ends_with("data: [DONE]\n\n"));
    assert!(!output.contains("\"error\":"));
    assert_eq!(control.captured().await["status"], 200);
    assert_eq!(control.reports.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn empty_and_legacy_failures_keep_destination_failure_terminals() {
    use crate::middleware::request_transform::Endpoint;
    for (source_protocol, endpoint) in [
        (SseProtocol::OpenaiChat, Endpoint::CreateModelResponse),
        (SseProtocol::OpenaiChat, Endpoint::Complete),
    ] {
        let control = Control::new().await;
        let output = transformed_response(
            &control,
            "event: error\ndata:\n\n".into(),
            source_protocol,
            endpoint,
            false,
        )
        .await;
        if endpoint == Endpoint::CreateModelResponse {
            assert!(output.contains("event: response.failed\n"), "{output}");
            assert!(!output.contains("response.completed"));
        } else {
            assert!(output.contains("\"error\":"), "{output}");
            assert!(output.ends_with("data: [DONE]\n\n"));
        }
        assert_eq!(control.captured().await["status"], 502);
        assert_eq!(control.reports.lock().unwrap().len(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn early_non_sse_handoff_disables_subsequent_keepalives() {
    use crate::middleware::sse::KeepAliveStream;
    use futures_util::FutureExt;
    let (release, pending) = tokio::sync::oneshot::channel();
    let source = Box::pin(async_stream::stream! {
        yield Ok(Record::Heartbeat);
        yield Ok(Record::NonSse(Bytes::new()));
        pending.await.unwrap();
        yield Ok(Record::NonSse(Bytes::from_static(b"opaque body")));
    });
    let mut stream = KeepAliveStream::new(source, Some(Duration::from_millis(10)));
    assert_eq!(
        serialize(&stream.next().await.unwrap().unwrap()).unwrap(),
        ": PROCESSING\n\n"
    );
    assert!(serialize(&stream.next().await.unwrap().unwrap())
        .unwrap()
        .is_empty());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(stream.next().now_or_never().is_none());
    release.send(()).unwrap();
    assert_eq!(
        serialize(&stream.next().await.unwrap().unwrap()).unwrap(),
        "opaque body"
    );
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn rejected_empty_bridge_terminal_is_a_failure_with_destination_error() {
    use crate::middleware::request_transform::Endpoint;
    let control = Control::new().await;
    let output = transformed_response(
        &control,
        "data: [DONE]\n\n".into(),
        SseProtocol::OpenaiChat,
        Endpoint::Messages,
        false,
    )
    .await;
    assert!(output.starts_with("event: error\n"), "{output}");
    assert!(!output.contains("event: message_stop\n"));
    assert_eq!(control.captured().await["status"], 502);
    assert_eq!(control.reports.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn failed_anthropic_delta_retains_its_usage_and_cost() {
    use crate::middleware::request_transform::Endpoint;
    let control = Control::new().await;
    let source = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"error\"},\"usage\":{\"output_tokens\":3}}\n\n";
    let output = transformed_response(
        &control,
        source.into(),
        SseProtocol::AnthropicMessages,
        Endpoint::ChatComplete,
        false,
    )
    .await;
    assert!(output.contains("\"error\":"), "{output}");
    assert!(output.contains("\"cost\":8"), "{output}");
    assert!(output.ends_with("data: [DONE]\n\n"));
    let report = control.captured().await;
    assert_eq!(report["status"], 502);
    assert_eq!(report["usage"]["input_tokens"], 2);
    assert_eq!(report["usage"]["output_tokens"], 3);
    assert_eq!(control.reports.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn numeric_chat_payload_uses_exact_decoded_size_in_every_transform() {
    use crate::middleware::request_transform::Endpoint;
    let control = Control::new().await;
    let numbers = "1234567890123,".repeat(1_107_142);
    let source = format!("data: {{\"choices\":[{numbers}1234567890123]}}\n\ndata: [DONE]\n\n");
    assert!(source.len() > 15_500_000 && source.len() < crate::sse::MAX_EVENT_BYTES);
    let output = transformed_response(
        &control,
        source,
        SseProtocol::OpenaiChat,
        Endpoint::ChatComplete,
        false,
    )
    .await;
    assert_eq!(output.matches("1234567890123").count(), 1_107_143);
    assert!(output.ends_with("data: [DONE]\n\n"));
    assert!(!output.contains("\"error\":"));
    assert_eq!(control.captured().await["status"], 200);
    assert_eq!(control.reports.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn named_tool_delta_without_payload_type_keeps_arguments() {
    use crate::middleware::request_transform::Endpoint;
    let control = Control::new().await;
    let source = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"search\",\"input\":{}}}\n\nevent: content_block_delta\ndata: {\"delta\":{\"partial_json\":\"{\\\"q\\\":\\\"test\\\"}\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let output = transformed_response(
        &control,
        source.into(),
        SseProtocol::AnthropicMessages,
        Endpoint::ChatComplete,
        false,
    )
    .await;
    assert!(
        output.contains("\"arguments\":\"{\\\"q\\\":\\\"test\\\"}\""),
        "{output}"
    );
    assert!(output.ends_with("data: [DONE]\n\n"));
    assert_eq!(control.captured().await["status"], 200);
    assert_eq!(control.reports.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn sanitize_renames_the_sse_event_with_the_rebuilt_payload() {
    use crate::middleware::request_transform::Endpoint;
    let control = Control::new().await;
    let wire = transformed_response(&control, concat!(
        "event: response.error\n",
        "data: {\"type\":\"response.error\",\"sequence_number\":2,\"error\":{\"type\":\"insufficient_quota\",\"code\":\"credit_balance_exhausted\",\"message\":\"no credits, top up at https://console.acme.ai/billing\"}}\n\n",
    ).into(), SseProtocol::OpenaiResponses, Endpoint::CreateModelResponse, false).await;
    assert!(wire.starts_with("event: error\n"), "{wire}");
    assert!(!wire.contains("response.error"), "{wire}");
    assert!(wire.contains("\"type\":\"error\""), "{wire}");
    assert!(wire.contains("\"sequence_number\":2"), "{wire}");
    assert!(!wire.contains("credit_balance_exhausted"), "{wire}");
    assert!(!wire.contains("console.acme.ai"), "{wire}");
    assert_eq!(control.captured().await["status"], 502);
}

#[tokio::test]
async fn anthropic_stream_error_reaches_the_chat_client_as_an_error_member() {
    use crate::middleware::request_transform::Endpoint;
    let control = Control::new().await;
    let wire = transformed_response(&control, "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"billing_error\",\"message\":\"credit balance too low\"}}\n\n".into(), SseProtocol::AnthropicMessages, Endpoint::ChatComplete, false).await;
    assert!(wire.contains("\"error\""), "no error member: {wire}");
    assert!(!wire.contains("billing_error"), "leaked kind: {wire}");
    assert!(!wire.contains("credit balance"), "leaked message: {wire}");
    assert!(wire.ends_with("data: [DONE]\n\n"), "{wire}");
    assert_eq!(control.captured().await["status"], 502);
}

#[tokio::test]
async fn named_anthropic_error_crosses_bridge_with_failure_and_terminal() {
    use crate::middleware::request_transform::Endpoint;
    let control = Control::new().await;
    let wire = transformed_response(
        &control,
        "event: error\ndata: {\"error\":{\"type\":\"api_error\",\"message\":\"failed\"}}\n\n"
            .into(),
        SseProtocol::AnthropicMessages,
        Endpoint::ChatComplete,
        false,
    )
    .await;
    assert_eq!(wire.matches("data: [DONE]").count(), 1);
    assert!(wire.contains("\"error\":"));
    assert_eq!(
        control.captured().await["errorMessage"],
        "stream_inband_error"
    );
}

#[tokio::test]
async fn sanitize_renaming_preserves_terminal_failure_metadata() {
    use crate::middleware::request_transform::Endpoint;
    for (timeout, status, class) in [
        (false, 502, "stream_inband_error"),
        (true, 504, "upstream_timeout"),
    ] {
        let control = Control::new().await;
        let wire = transformed_response(&control, "event: response.error\ndata: {\"type\":\"response.error\",\"error\":{\"type\":\"api_error\",\"message\":\"failed\"}}\n\n".into(), SseProtocol::OpenaiResponses, Endpoint::CreateModelResponse, timeout).await;
        assert_eq!(wire.matches("event: error\n").count(), 1);
        let report = control.captured().await;
        assert_eq!(report["status"], status);
        assert_eq!(report["errorMessage"], class);
        assert!(report["errorSource"].is_null());
        assert_eq!(control.reports.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn owner_e2ee_failure_observes_later_usage_until_eof_or_disconnect() {
    use futures_util::FutureExt;
    for disconnect in [false, true] {
        let control = Control::new().await;
        let (send, receive) = tokio::sync::mpsc::unbounded_channel();
        let source = futures_util::stream::unfold(receive, |mut receive| async {
            receive.recv().await.map(|record| (record, receive))
        });
        let mut stream = owner(Box::pin(source), control.report());
        stream.e2ee = Some(E2eeRequestContext {
            version: "2".into(),
            algo: crate::aci::e2ee::E2EE_ALGO_X25519_AESGCM.into(),
            aad_mode: E2eeAadMode::AciV2,
            request_model: "fixture".into(),
            client_public_key_hex: "11".repeat(32),
            nonce: None,
            timestamp: None,
        });
        send.send(Ok(Bytes::from_static(b"data: invalid JSON\n\n")))
            .unwrap();
        assert!(stream.next().now_or_never().is_none());
        assert!(control.reports.lock().unwrap().is_empty());
        send.send(Ok(Bytes::from_static(b"data: {\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"provider_trace\":\"PRIVATE_UPSTREAM_DETAIL\"}}\n\n"))).unwrap();
        assert!(stream.next().now_or_never().is_none());
        if disconnect {
            drop(stream);
        } else {
            drop(send);
            assert!(matches!(
                stream.next().await,
                Some(Err(ServiceError::E2ee(_)))
            ));
            assert!(stream.next().await.is_none());
        }
        let report = control.captured().await;
        assert_eq!(report["status"], 502);
        assert_eq!(report["errorSource"], "gateway");
        assert_eq!(report["errorMessage"], "downstream_finalizer_failed");
        assert_eq!(
            report["usage"],
            json!({"prompt_tokens":2,"completion_tokens":3})
        );
        assert_eq!(control.reports.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn contradictory_source_control_fails_output_but_retains_its_and_later_usage() {
    let control = Control::new().await;
    let source = concat!(
        "event: message_stop\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2}}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    let inner = Box::pin(futures_util::stream::iter([Ok(Bytes::from_static(
        source.as_bytes(),
    ))]));
    let bytes: Vec<_> = owner_for(inner, control.report(), SseProtocol::AnthropicMessages)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flat_map(Result::unwrap)
        .collect();
    let expected: Vec<_> = error_records(SseProtocol::AnthropicMessages, 502, None, None)
        .iter()
        .flat_map(|r| serialize(r).unwrap())
        .collect();
    assert_eq!(bytes, expected);
    let report = control.captured().await;
    assert_eq!(report["status"], 502);
    assert_eq!(report["errorMessage"], "upstream_transport");
    assert!(report["errorSource"].is_null());
    assert_eq!(report["usage"], json!({"input_tokens":2,"output_tokens":3}));
    assert_eq!(control.reports.lock().unwrap().len(), 1);
}
