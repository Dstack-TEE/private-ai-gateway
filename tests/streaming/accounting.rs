//! Accounting regression tests use the same typed stages as the HTTP pipeline.
use super::*;
use crate::middleware::config::MiddlewareConfig;
use crate::middleware::control::ControlClient;
use crate::middleware::sse::{MeterEvents, StreamReport};
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
    let accounting = Accounting::new(report, None);
    let source = Box::pin(DecodedStream::new(inner, Some(SseProtocol::OpenaiChat)));
    let events = Box::pin(MeterEvents::new(source, accounting.handle.clone()));
    ResponseOwner::new(
        ResponseInput::Events(events),
        Some(accounting),
        None,
        "/v1/chat/completions".into(),
        Some(SseProtocol::OpenaiChat),
        None,
    )
}

#[tokio::test]
async fn typed_accounting_injects_cost_reports_original_usage_and_tenant() {
    let control = Control::new().await;
    let source = Box::pin(futures_util::stream::iter([Ok(Bytes::from_static(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":20}}\n\ndata: [DONE]\n\n"))]));
    let output: Vec<_> = owner(source, control.report()).collect().await;
    let bytes: Vec<_> = output.into_iter().flat_map(Result::unwrap).collect();
    assert!(String::from_utf8(bytes).unwrap().contains("\"cost\":50"));
    let report = control.captured().await;
    assert_eq!(report["status"], 200);
    assert_eq!(report["usage"]["prompt_tokens"], 10);
    assert_eq!(report["usage"]["completion_tokens"], 20);
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
    for (wire, status) in [
        ("data: [DONE]\n\n", 200),
        ("data: [DONE] \n\n", 504),
        ("data:\ndata: [DONE]\n\n", 504),
        ("[DONE]\n\n", 504),
        ("{\"type\":\"message_stop\"}\n\n", 504),
    ] {
        let control = Control::new().await;
        let source = Box::pin(futures_util::stream::iter([
            Ok(Bytes::from(wire)),
            Err(ServiceError::Upstream(
                crate::aci::upstream::UpstreamError::Timeout("read deadline".into()),
            )),
        ]));
        let mut stream = owner(source, control.report());
        while stream.next().await.is_some() {}
        let report = control.captured().await;
        assert_eq!(report["status"], status, "{wire:?}");
        if status == 504 {
            assert_eq!(report["errorMessage"], "upstream_timeout");
        }
    }
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
                aad_mode: super::super::wire::E2eeAadMode::AciV2,
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
