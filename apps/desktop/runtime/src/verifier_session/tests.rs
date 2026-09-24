use super::*;

struct HttpService {
    base_url: String,
    client: reqwest::Client,
}

impl agent_bridge::proxy::VerifiedService for HttpService {
    fn call(
        self: Arc<Self>,
        request: axum::http::Request<axum::body::Body>,
        _context: Option<agent_bridge::proxy::ForwardContext>,
    ) -> agent_bridge::proxy::VerifiedResponse {
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let bytes = axum::body::to_bytes(body, agent_bridge::proxy::MAX_BODY_BYTES)
                .await
                .unwrap();
            let mut upstream = self
                .client
                .request(parts.method, format!("{}{}", self.base_url, parts.uri));
            for (name, value) in &parts.headers {
                upstream = upstream.header(name, value);
            }
            let response = upstream.body(bytes).send().await.unwrap();
            let status = response.status();
            let headers = response.headers().clone();
            let mut builder = axum::response::Response::builder().status(status);
            for (name, value) in headers {
                if let Some(name) = name {
                    builder = builder.header(name, value);
                }
            }
            builder
                .body(axum::body::Body::from_stream(response.bytes_stream()))
                .unwrap()
        })
    }
}

fn http_service(base_url: &str) -> Arc<dyn agent_bridge::proxy::VerifiedService> {
    Arc::new(HttpService {
        base_url: base_url.to_string(),
        client: reqwest::Client::builder().no_proxy().build().unwrap(),
    })
}

#[tokio::test]
async fn compatibility_refresh_is_background_work_and_cannot_resurrect_a_stopped_session() {
    use axum::{routing::get, Json, Router};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut inventory = serde_json::to_value(EndpointInventory::bundled().unwrap()).unwrap();
    inventory["checkedAt"] = "2027-01-01T00:00:00Z".into();
    for observation in inventory["results"].as_array_mut().unwrap() {
        if observation["model"] == "z-ai/glm-5.3" {
            observation["status"] = "inconclusive".into();
        }
    }
    let app = Router::new()
        .route(
            "/v1/models",
            get(|| async { Json(serde_json::json!({"data":[{"id":"z-ai/glm-5.3"}]})) }),
        )
        .route(
            "/inventory",
            get({
                let reached = reached.clone();
                let release = release.clone();
                move || {
                    let reached = reached.clone();
                    let release = release.clone();
                    let inventory = inventory.clone();
                    async move {
                        reached.notify_one();
                        release.notified().await;
                        Json(inventory)
                    }
                }
            }),
        );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let directory = tempfile::tempdir().unwrap();
    let updater = InventoryUpdater::new(directory.path().join("inventory.json"))
        .unwrap()
        .with_source(format!("{base}/inventory"));
    let (events, _) = tokio::sync::mpsc::channel(8);
    let proxy = ProxyState::new(events).unwrap();
    let manager = Arc::new(
        SessionManager::new(
            proxy.clone(),
            Arc::new(UsageStore::memory().unwrap()),
            Arc::new(WaitingSidecar),
            Handle::current(),
            AppState::default(),
        )
        .with_endpoint_inventory(updater),
    );
    {
        let mut runtime = manager.lock().unwrap();
        runtime.generation = 1;
        runtime.epoch = 1;
        runtime.identity_ready = true;
        runtime.service = Some(http_service(&base));
        runtime.state.remote_url = Some("https://tee.redpill.ai".into());
    }
    proxy.publish(Session {
        generation: 1,
        epoch: 1,
        service: Some(http_service(&base)),
        ..Session::default()
    });
    tokio::time::timeout(Duration::from_secs(2), manager.load_catalog(1, 1))
        .await
        .unwrap()
        .unwrap();
    assert!(proxy.session().verified);
    tokio::time::timeout(Duration::from_secs(2), reached.notified())
        .await
        .unwrap();
    assert!(proxy.session().catalog.unwrap().models[0]
        .supports(agent_bridge::catalog::Surface::Responses));
    // A second catalog read also stays responsive while the shared download waits.
    tokio::time::timeout(Duration::from_secs(2), manager.refresh_catalog())
        .await
        .unwrap()
        .unwrap();
    let mut states = manager.subscribe();
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), states.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(proxy
        .session()
        .catalog
        .unwrap()
        .for_surface(agent_bridge::catalog::Surface::Responses)
        .models
        .is_empty());
    assert_eq!(proxy.session().epoch, 1);
    manager.stop().unwrap();
    manager.apply_inventory(1, 1).unwrap();
    assert!(!proxy.session().verified);
    server.abort();
    let _ = server.await;
}

struct WaitingSidecar;

#[tokio::test]
async fn late_catalog_failure_cannot_override_a_newer_success_or_security_stop() {
    use axum::{routing::get, Json, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let app = Router::new().route(
        "/v1/models",
        get({
            let calls = calls.clone();
            let reached = reached.clone();
            let release = release.clone();
            move || {
                let index = calls.fetch_add(1, Ordering::SeqCst);
                let reached = reached.clone();
                let release = release.clone();
                async move {
                    if index.is_multiple_of(2) {
                        reached.notify_one();
                        release.notified().await;
                        return Json(serde_json::json!({"error":"old request failed"}));
                    }
                    Json(serde_json::json!({"data":[{"id":"current-model"}]}))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (events, _) = tokio::sync::mpsc::channel(8);
    let proxy = ProxyState::new(events).unwrap();
    let manager = Arc::new(SessionManager::new(
        proxy.clone(),
        Arc::new(UsageStore::memory().unwrap()),
        Arc::new(WaitingSidecar),
        Handle::current(),
        AppState::default(),
    ));
    {
        let mut runtime = manager.lock().unwrap();
        runtime.identity_ready = true;
        runtime.service = Some(http_service(&base));
    }
    proxy.publish(Session {
        service: Some(http_service(&base)),
        ..Session::default()
    });
    for stop in [false, true] {
        let pending = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.refresh_catalog().await })
        };
        tokio::time::timeout(Duration::from_secs(2), reached.notified())
            .await
            .unwrap();
        if stop {
            manager.stop().unwrap();
        } else {
            manager.refresh_catalog().await.unwrap();
        }
        release.notify_one();
        pending.await.unwrap().unwrap();
        let state = manager.snapshot().unwrap();
        assert!(state.error.is_none());
        assert_eq!(proxy.session().verified, !stop);
        assert_eq!(state.status, if stop { "stopped" } else { "verified" });
        if !stop {
            assert_eq!(state.catalog.unwrap().models[0].id, "current-model");
        }
    }
    server.abort();
    let _ = server.await;
}
struct WaitingTask;
impl VerifierTask for WaitingTask {
    fn stop(&mut self) -> Result<(), String> {
        Ok(())
    }
}

struct StopTrackingTask(Arc<std::sync::atomic::AtomicBool>);

impl VerifierTask for StopTrackingTask {
    fn stop(&mut self) -> Result<(), String> {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

struct StopTrackingLauncher(Arc<std::sync::atomic::AtomicBool>);

impl VerifierLauncher for StopTrackingLauncher {
    fn spawn(
        &self,
        _: VerifierConfig,
        _: VerifierEventSink,
        _: tokio::sync::mpsc::Sender<ProxyEvent>,
    ) -> Result<Box<dyn VerifierTask>, String> {
        Ok(Box::new(StopTrackingTask(self.0.clone())))
    }
}

impl VerifierLauncher for WaitingSidecar {
    fn spawn(
        &self,
        _: VerifierConfig,
        _: VerifierEventSink,
        _: tokio::sync::mpsc::Sender<ProxyEvent>,
    ) -> Result<Box<dyn VerifierTask>, String> {
        Ok(Box::new(WaitingTask))
    }
}

#[tokio::test]
async fn ready_event_loads_catalog_before_opening_the_session() {
    use axum::{routing::get, Json, Router};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/models",
                get(|| async { Json(serde_json::json!({"data":[{"id":"test-model"}]})) }),
            ),
        )
        .await
        .unwrap();
    });
    let (events, _) = tokio::sync::mpsc::channel(8);
    let proxy = ProxyState::new(events).unwrap();
    let manager = Arc::new(SessionManager::new(
        proxy.clone(),
        Arc::new(UsageStore::memory().unwrap()),
        Arc::new(WaitingSidecar),
        Handle::current(),
        AppState::default(),
    ));
    let started = manager
        .start(StartConfig {
            remote_url: "https://inference.phala.com".into(),
            require_production_os: true,
        })
        .unwrap();
    let identity = identity_event(serde_json::json!({
        "tee_type": "tdx",
        "trust_level": "hardware_verified",
        "keyset_digest": "sha256:keyset",
        "keyset_not_after": 2_000_000_000_u64,
        "tls_spki": null,
        "source_provenance": {},
        "service_capabilities": {
            "serving": "aggregator",
            "supported_e2ee_versions": []
        },
        "verification": { "checks": [] }
    }));
    manager
        .handle_event(
            proxy.session().generation,
            VerifierEvent::Ready {
                identity,
                remote_url: "https://inference.phala.com".into(),
                service: http_service(&base),
            },
        )
        .unwrap();
    let state = manager
        .wait_for_verification(
            started.session_id.as_deref().unwrap(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
    assert_eq!(state.status, "verified");
    assert_eq!(state.catalog.unwrap().models[0].id, "test-model");
    assert!(proxy.session().verified);
    manager.stop().unwrap();
    server.abort();
    let _ = server.await;
}

#[test]
fn unexpected_termination_revokes_forwarding_and_requests_reconnect() {
    let executor = tokio::runtime::Runtime::new().unwrap();
    let (events, _) = tokio::sync::mpsc::channel(8);
    let proxy = ProxyState::new(events).unwrap();
    let manager = Arc::new(SessionManager::new(
        proxy.clone(),
        Arc::new(UsageStore::memory().unwrap()),
        Arc::new(WaitingSidecar),
        executor.handle().clone(),
        AppState::default(),
    ));
    manager
        .start(StartConfig {
            remote_url: "https://inference.phala.com".into(),
            require_production_os: true,
        })
        .unwrap();
    let generation = proxy.session().generation;
    manager
        .handle_event(
            generation,
            VerifierEvent::Terminated {
                error: Some("panic".into()),
            },
        )
        .unwrap();
    let state = manager.snapshot().unwrap();
    assert_eq!(state.status, "error");
    assert_eq!(
        state.error.as_deref(),
        Some("Verifier task stopped unexpectedly: panic")
    );
    assert!(state.reconnecting && crate::recovery::should_retry(&state));
    assert!(!manager.is_running().unwrap());
    assert!(!proxy.session().verified);
}

#[test]
fn explicit_stop_stops_the_task_and_ends_the_session() {
    let executor = tokio::runtime::Runtime::new().unwrap();
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (events, _) = tokio::sync::mpsc::channel(8);
    let proxy = ProxyState::new(events).unwrap();
    let usage = Arc::new(UsageStore::memory().unwrap());
    let manager = Arc::new(SessionManager::new(
        proxy.clone(),
        usage.clone(),
        Arc::new(StopTrackingLauncher(stopped.clone())),
        executor.handle().clone(),
        AppState::default(),
    ));
    manager
        .start(StartConfig {
            remote_url: "https://inference.phala.com".into(),
            require_production_os: true,
        })
        .unwrap();
    let state = manager.stop().unwrap();
    assert_eq!(state.status, "stopped");
    assert!(stopped.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!manager.is_running().unwrap());
    assert!(proxy.session().session_id.is_none());
    assert!(usage.active_session().unwrap().is_none());
}

#[tokio::test(start_paused = true)]
async fn silent_verifier_times_out_without_stopping_a_completed_verification() {
    let (events, _) = tokio::sync::mpsc::channel(8);
    let proxy = ProxyState::new(events).unwrap();
    let manager = Arc::new(SessionManager::new(
        proxy.clone(),
        Arc::new(UsageStore::memory().unwrap()),
        Arc::new(WaitingSidecar),
        Handle::current(),
        AppState::default(),
    ));
    let config = StartConfig {
        remote_url: "https://inference.phala.com".into(),
        require_production_os: true,
    };
    manager.start(config.clone()).unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(46)).await;
    tokio::task::yield_now().await;
    assert_eq!(manager.snapshot().unwrap().status, "verifying");
    tokio::time::advance(Duration::from_secs(75)).await;
    tokio::task::yield_now().await;
    assert_eq!(manager.snapshot().unwrap().status, "error");
    assert!(crate::recovery::should_retry(&manager.snapshot().unwrap()));
    assert!(!proxy.session().verified);
    manager.start(config).unwrap();
    let generation = proxy.session().generation;
    let mut complete = manager.snapshot().unwrap();
    complete.status = "verified".into();
    manager.restore_snapshot(complete);
    manager
        .fail_if(generation, Some("verifying"), "late timeout".into())
        .unwrap();
    tokio::time::advance(Duration::from_secs(121)).await;
    tokio::task::yield_now().await;
    assert_eq!(manager.snapshot().unwrap().status, "verified");
    manager.stop().unwrap();
}

#[test]
fn keyset_change_requests_fresh_verification_without_ending_the_session() {
    let executor = tokio::runtime::Runtime::new().unwrap();
    let (events, _) = tokio::sync::mpsc::channel(8);
    let proxy = ProxyState::new(events).unwrap();
    let usage = Arc::new(UsageStore::memory().unwrap());
    let manager = Arc::new(SessionManager::new(
        proxy.clone(),
        usage.clone(),
        Arc::new(WaitingSidecar),
        executor.handle().clone(),
        AppState::default(),
    ));
    manager
        .start(StartConfig {
            remote_url: "https://inference.phala.com".into(),
            require_production_os: true,
        })
        .unwrap();
    manager
        .handle_event(
            proxy.session().generation,
            VerifierEvent::Blocked {
                code: Some("keyset_changed".into()),
                reason: "rotation".into(),
            },
        )
        .unwrap();
    let state = manager.snapshot().unwrap();
    assert_eq!(state.status, "error");
    assert!(state.reconnecting && crate::recovery::should_retry(&state));
    assert!(!manager.is_running().unwrap());
    assert!(!proxy.session().verified);
    assert!(usage.active_session().unwrap().is_some());
}

#[test]
fn security_blocks_survive_process_failure_without_cancelling_candidate_sessions() {
    for verification_only in [false, true] {
        let executor = tokio::runtime::Runtime::new().unwrap();
        let (events, _) = tokio::sync::mpsc::channel(8);
        let proxy = ProxyState::new(events).unwrap();
        let usage = Arc::new(UsageStore::memory().unwrap());
        let manager = Arc::new(SessionManager::new(
            proxy.clone(),
            usage.clone(),
            Arc::new(WaitingSidecar),
            executor.handle().clone(),
            AppState::default(),
        ));
        let config = StartConfig {
            remote_url: "https://inference.phala.com".into(),
            require_production_os: true,
        };
        manager.start(config.clone()).unwrap();
        if verification_only {
            manager.stop_with_reconnect(true).unwrap();
            manager.begin_verification(config, true).unwrap();
        }
        let generation = proxy.session().generation;
        manager
            .handle_event(
                generation,
                VerifierEvent::Blocked {
                    code: None,
                    reason: "identity rejected".into(),
                },
            )
            .unwrap();
        assert_eq!(usage.active_session().unwrap().is_some(), verification_only);
        let running = manager.is_running().unwrap();
        manager
            .handle_event(
                generation,
                VerifierEvent::Blocked {
                    code: Some("keyset_changed".into()),
                    reason: "late rotation".into(),
                },
            )
            .unwrap();
        assert_eq!(manager.snapshot().unwrap().status, "blocked");
        assert_eq!(usage.active_session().unwrap().is_some(), verification_only);
        assert_eq!(manager.is_running().unwrap(), running);
        manager
            .handle_event(
                generation,
                VerifierEvent::Fatal {
                    message: "task failed".into(),
                },
            )
            .unwrap();
        manager.terminated(generation, None).unwrap();
        manager
            .fail(generation, "event sink failed".into())
            .unwrap();
        let state = manager.snapshot().unwrap();
        assert_eq!(state.status, "blocked");
        assert_eq!(state.configuration_verification, verification_only);
        assert!(!crate::recovery::should_retry(&state));
        assert!(!proxy.session().verified);
    }
}

#[test]
fn reconnection_preserves_session_history_but_requires_fresh_verification() {
    let executor = tokio::runtime::Runtime::new().unwrap();
    let (events, _) = tokio::sync::mpsc::channel(8);
    let proxy = ProxyState::new(events).unwrap();
    let manager = Arc::new(SessionManager::new(
        proxy.clone(),
        Arc::new(UsageStore::memory().unwrap()),
        Arc::new(WaitingSidecar),
        executor.handle().clone(),
        AppState::default(),
    ));
    let config = StartConfig {
        remote_url: "https://inference.phala.com".into(),
        require_production_os: true,
    };
    manager.restore_snapshot(AppState {
        status: "verified".into(),
        config: config.clone(),
        session_id: Some("same-session".into()),
        session_active: true,
        protected_since: Some(123),
        session_usage: UsageSummary {
            requests: 7,
            ..Default::default()
        },
        ..Default::default()
    });
    manager.stop_with_reconnect(true).unwrap();
    let retired_generation = proxy.session().generation;
    let resumed = manager.start(config.clone()).unwrap();
    assert_eq!(resumed.session_id.as_deref(), Some("same-session"));
    assert_eq!(resumed.protected_since, Some(123));
    assert_eq!(resumed.session_usage.requests, 7);
    assert!(resumed.reconnecting);
    assert!(resumed.identity.is_none() && resumed.catalog.is_none());
    assert!(!proxy.session().verified);
    assert!(proxy.session().generation > retired_generation);
    manager.terminated(retired_generation, None).unwrap();
    assert_eq!(manager.snapshot().unwrap().status, "verifying");
    manager
        .fail(proxy.session().generation, "Transport interrupted".into())
        .unwrap();
    let recovered = SessionManager::new(
        proxy.clone(),
        manager.usage.clone(),
        Arc::new(WaitingSidecar),
        executor.handle().clone(),
        AppState::default(),
    )
    .snapshot()
    .unwrap();
    assert_eq!(recovered.session_id, resumed.session_id);
    assert_eq!(recovered.protected_since, Some(123));
    assert!(recovered.session_active && recovered.identity.is_none());
    assert_eq!(recovered.status, "stopped");
    let retried = manager.start(config.clone()).unwrap();
    assert_eq!(retried.session_id, resumed.session_id);
    assert_eq!(retried.session_usage.requests, 7);
    assert!(retried.session_active);
    manager.stop_with_reconnect(true).unwrap();
    let candidate = StartConfig {
        remote_url: "https://tee.redpill.ai".into(),
        ..config.clone()
    };
    let verification = manager.begin_verification(candidate.clone(), true).unwrap();
    assert_eq!(verification.session_id, resumed.session_id);
    assert!(verification.configuration_verification && verification.session_active);
    manager.stop_with_reconnect(true).unwrap();
    let switched = manager.start(candidate).unwrap();
    assert_eq!(switched.session_id, resumed.session_id);
    assert_eq!(switched.protected_since, Some(123));
    manager.stop().unwrap();
    assert!(proxy.session().session_id.is_none());
    assert!(manager.usage.active_session().unwrap().is_none());
    let fresh = manager.start(config).unwrap();
    assert_ne!(fresh.session_id.as_deref(), Some("same-session"));
    assert_eq!(fresh.session_usage.requests, 0);
    assert!(fresh.protected_since.is_some() && !fresh.reconnecting);
    assert!(fresh.session_active);
    manager.stop().unwrap();
}

#[test]
fn stopping_preserves_usage_but_not_the_protection_clock() {
    let mut state = AppState {
        protected_since: Some(123),
        ..AppState::default()
    };
    state.session_usage.requests = 7;
    let stopped = SessionManager::carried(&state);
    assert_eq!(stopped.protected_since, None);
    assert_eq!(stopped.session_usage.requests, 7);
}

#[tokio::test]
async fn stale_verifier_generation_cannot_record_activity() {
    let (events, _) = tokio::sync::mpsc::channel(8);
    let proxy = ProxyState::new(events).unwrap();
    let manager = SessionManager::new(
        proxy,
        Arc::new(UsageStore::memory().unwrap()),
        Arc::new(WaitingSidecar),
        Handle::current(),
        AppState::default(),
    );
    manager.lock().unwrap().generation = 2;

    manager.record_proxy_event(ProxyEvent {
        generation: 1,
        request_id: "stale-request".to_string(),
        session_id: "stale-session".to_string(),
        agent: Some("codex".to_string()),
        method: "POST".to_string(),
        path: "/v1/responses".to_string(),
        model: Some("test-model".to_string()),
        status: 200,
        streamed: true,
        receipt_id: Some("stale-receipt".to_string()),
        verified: Some(false),
        detail: "late verdict".to_string(),
        at: 1,
        local_policy_applied: Some(true),
        rewritten: Some(false),
        left_device: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        cost_usd: None,
    });

    assert!(manager.snapshot().unwrap().activity.is_empty());
    assert_eq!(
        manager
            .usage
            .session_summary("stale-session")
            .unwrap()
            .requests,
        0
    );
}
use serde_json::json;

fn identity_event(value: serde_json::Value) -> IdentityEvent {
    serde_json::from_value(value).unwrap()
}

fn receipt_activity(id: &str, session_id: &str, status: u16, detail: &str) -> RequestActivity {
    RequestActivity {
        id: id.to_string(),
        session_id: session_id.to_string(),
        method: "POST".to_string(),
        path: "/v1/messages".to_string(),
        model: Some("openai/gpt-oss-20b".to_string()),
        status,
        streamed: true,
        receipt_id: Some("rcpt-merge".to_string()),
        verified: Some(status < 400),
        detail: detail.to_string(),
        at: 101,
        agent: Some("claude-code".to_string()),
        local_policy_applied: Some(true),
        rewritten: Some(true),
        left_device: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        cost_usd: None,
    }
}

#[test]
fn identity_alone_does_not_verify() {
    let identity = json!({
        "tee_type": "tdx",
        "trust_level": "hardware_verified",
        "keyset_digest": "sha256:keyset",
        "keyset_not_after": 2_000_000_000,
        "tls_spki": null,
        "source_provenance": { "repo_commit": "abc123" },
        "service_capabilities": {
            "serving": "aggregator",
            "supported_e2ee_versions": []
        },
        "verification": { "checks": [{
            "id": "id-1", "section": "9.1(1)", "title": "Hardware quote",
            "status": "pass", "detail": "TDX quote verified"
        }]}
    });
    let mut state = AppState::default();
    apply_identity_event(&mut state, &identity_event(identity));
    assert_eq!(
        state.status, "stopped",
        "status is decided once the catalog is in"
    );
    assert!(state.identity.is_some());
}

#[test]
fn proxy_receipt_and_usage_events_merge_into_one_complete_activity() {
    let mut state = AppState::default();
    merge_activity(
        &mut state,
        RequestActivity {
            id: "req-merge".to_string(),
            session_id: "session-merge".to_string(),
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            model: Some("openai/gpt-oss-20b".to_string()),
            status: 200,
            streamed: true,
            receipt_id: Some("rcpt-merge".to_string()),
            verified: None,
            detail: "Awaiting receipt verification".to_string(),
            at: 100,
            agent: Some("claude-code".to_string()),
            local_policy_applied: None,
            rewritten: None,
            left_device: true,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost_usd: None,
        },
    );

    let verdict = receipt_activity("req-merge", "session-merge", 200, "receipt verified");
    merge_activity(&mut state, verdict.clone());

    merge_activity(
        &mut state,
        RequestActivity {
            id: "req-merge".to_string(),
            session_id: "session-merge".to_string(),
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            model: Some("openai/gpt-oss-20b".to_string()),
            status: 200,
            streamed: true,
            receipt_id: None,
            verified: None,
            detail: String::new(),
            at: 102,
            agent: Some("claude-code".to_string()),
            local_policy_applied: None,
            rewritten: None,
            left_device: true,
            input_tokens: Some(1_024),
            output_tokens: Some(256),
            cache_read_tokens: Some(512),
            cache_write_tokens: Some(64),
            cost_usd: Some(0.0042),
        },
    );

    assert_eq!(state.activity.len(), 1);
    let item = &state.activity[0];
    assert_eq!(item.id, "req-merge");
    assert_eq!(item.at, 100);
    assert_eq!(item.receipt_id.as_deref(), Some("rcpt-merge"));
    assert_eq!(item.verified, Some(true));
    assert_eq!(item.local_policy_applied, Some(true));
    assert_eq!(item.rewritten, Some(true));
    assert_eq!(item.input_tokens, Some(1_024));
    assert_eq!(item.output_tokens, Some(256));
    assert_eq!(item.cache_read_tokens, Some(512));
    assert_eq!(item.cache_write_tokens, Some(64));
    assert_eq!(item.cost_usd, Some(0.0042));
    let mut timeout = item.clone();
    timeout.status = 504;
    timeout.detail = "Client delivery timed out".into();
    merge_activity(&mut state, timeout);
    merge_activity(&mut state, verdict.clone());
    assert_eq!(state.activity[0].status, 504);
    assert_eq!(state.activity[0].detail, "Client delivery timed out");
    assert_eq!(state.activity[0].input_tokens, Some(1_024));
    let mut pending = state.activity[0].clone();
    pending.id = "req-proof".into();
    pending.status = 502;
    pending.detail.clear();
    pending.verified = None;
    merge_activity(&mut state, pending);
    let withheld = receipt_activity(
        "req-proof",
        "session-merge",
        502,
        "Response withheld: receipt verification failed",
    );
    merge_activity(&mut state, withheld);
    let proof = state
        .activity
        .iter()
        .find(|item| item.id == "req-proof")
        .unwrap();
    assert_eq!(proof.status, 502);
    assert_eq!(
        proof.detail,
        "Response withheld: receipt verification failed"
    );
    assert_eq!(proof.verified, Some(false));
}
