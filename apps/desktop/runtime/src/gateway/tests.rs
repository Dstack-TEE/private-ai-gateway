use super::*;

struct WaitingSidecar;
struct WaitingChild(Option<tokio::sync::mpsc::Sender<SidecarEvent>>);
impl SidecarChild for WaitingChild {
    fn kill(&mut self) -> Result<(), String> {
        self.0.take();
        Ok(())
    }
}
impl SidecarLauncher for WaitingSidecar {
    fn spawn(
        &self,
        _: Vec<String>,
    ) -> Result<(Receiver<SidecarEvent>, Box<dyn SidecarChild>), String> {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        Ok((receiver, Box::new(WaitingChild(Some(sender)))))
    }
}

#[tokio::test(start_paused = true)]
async fn silent_verifier_times_out_without_stopping_a_completed_verification() {
    let (events, _) = tokio::sync::mpsc::channel(8);
    let proxy = ProxyState::new(events).unwrap();
    let manager = Arc::new(GatewayManager::new(
        proxy.clone(),
        Arc::new(UsageStore::memory().unwrap()),
        Arc::new(WaitingSidecar),
        Handle::current(),
        GatewayState::default(),
    ));
    let config = StartGatewayConfig {
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
    let manager = Arc::new(GatewayManager::new(
        proxy.clone(),
        usage.clone(),
        Arc::new(WaitingSidecar),
        executor.handle().clone(),
        GatewayState::default(),
    ));
    manager
        .start(StartGatewayConfig {
            remote_url: "https://inference.phala.com".into(),
            require_production_os: true,
        })
        .unwrap();
    manager
        .handle_line(
            proxy.session().generation,
            r#"{"schema_version":1,"type":"blocked","code":"keyset_changed","reason":"rotation"}"#,
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
        let manager = Arc::new(GatewayManager::new(
            proxy.clone(),
            usage.clone(),
            Arc::new(WaitingSidecar),
            executor.handle().clone(),
            GatewayState::default(),
        ));
        let config = StartGatewayConfig {
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
            .handle_line(
                generation,
                r#"{"schema_version":1,"type":"blocked","reason":"identity rejected"}"#,
            )
            .unwrap();
        assert_eq!(usage.active_session().unwrap().is_some(), verification_only);
        let running = manager.is_running().unwrap();
        manager.handle_line(generation, r#"{"schema_version":1,"type":"blocked","code":"keyset_changed","reason":"late rotation"}"#).unwrap();
        assert_eq!(manager.snapshot().unwrap().status, "blocked");
        assert_eq!(usage.active_session().unwrap().is_some(), verification_only);
        assert_eq!(manager.is_running().unwrap(), running);
        manager
            .handle_line(
                generation,
                r#"{"schema_version":1,"type":"fatal","message":"process failed"}"#,
            )
            .unwrap();
        manager.terminated(generation).unwrap();
        manager.fail(generation, "reader failed".into()).unwrap();
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
    let manager = Arc::new(GatewayManager::new(
        proxy.clone(),
        Arc::new(UsageStore::memory().unwrap()),
        Arc::new(WaitingSidecar),
        executor.handle().clone(),
        GatewayState::default(),
    ));
    let config = StartGatewayConfig {
        remote_url: "https://inference.phala.com".into(),
        require_production_os: true,
    };
    manager.restore_snapshot(GatewayState {
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
    manager.terminated(retired_generation).unwrap();
    assert_eq!(manager.snapshot().unwrap().status, "verifying");
    manager
        .fail(proxy.session().generation, "Transport interrupted".into())
        .unwrap();
    let recovered = GatewayManager::new(
        proxy.clone(),
        manager.usage.clone(),
        Arc::new(WaitingSidecar),
        executor.handle().clone(),
        GatewayState::default(),
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
    let candidate = StartGatewayConfig {
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
    let mut state = GatewayState {
        protected_since: Some(123),
        ..GatewayState::default()
    };
    state.session_usage.requests = 7;
    let stopped = GatewayManager::carried(&state);
    assert_eq!(stopped.protected_since, None);
    assert_eq!(stopped.session_usage.requests, 7);
}
use serde_json::json;

#[test]
fn identity_alone_does_not_verify_and_requests_are_attributed() {
    let identity = json!({
        "type": "ready",
        "schema_version": 1,
        "remote_url": "https://tee.redpill.ai",
        "proxy_url": "http://127.0.0.1:53211",
        "tee_type": "tdx",
        "trust_level": "hardware_verified",
        "keyset_digest": "sha256:keyset",
        "keyset_not_after": 2_000_000_000,
        "source_provenance": { "repo_commit": "abc123" },
        "verification": { "checks": [{
            "id": "id-1", "section": "9.1(1)", "title": "Hardware quote",
            "status": "pass", "detail": "TDX quote verified"
        }]}
    });
    let mut state = GatewayState::default();
    apply_identity_event(&mut state, identity.as_object().unwrap()).unwrap();
    assert_eq!(
        state.status, "stopped",
        "status is decided once the catalog is in"
    );
    assert!(state.identity.is_some());

    let request = json!({
        "method": "POST", "path": "/v1/messages", "status": 200, "streamed": true,
        "receipt_id": "rcpt-1", "verified": null,
        "detail": "receipt rcpt-1 recorded", "tag": "pap:req-1:session-1:claude-code"
    });
    apply_request_event(&mut state, request.as_object().unwrap()).unwrap();
    let verdict = json!({
        "method": "POST", "path": "/v1/messages", "status": 200, "streamed": true,
        "receipt_id": "rcpt-1", "verified": true, "rewritten": true,
        "locally_constrained": true,
        "detail": "receipt verified", "tag": "pap:req-1:session-1:claude-code"
    });
    apply_request_event(&mut state, verdict.as_object().unwrap()).unwrap();

    assert_eq!(state.activity.len(), 1);
    let item = &state.activity[0];
    assert_eq!(item.id, "req-1");
    assert_eq!(item.session_id, "session-1");
    assert_eq!(item.agent.as_deref(), Some("claude-code"));
    assert_eq!(item.verified, Some(true));
    assert_eq!(item.rewritten, Some(true));
    assert_eq!(item.locally_constrained, Some(true));
}

#[test]
fn proxy_receipt_and_usage_events_merge_into_one_complete_activity() {
    let mut state = GatewayState::default();
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
            locally_constrained: None,
            rewritten: None,
            left_device: true,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost_usd: None,
        },
    );

    let verdict = json!({
        "method": "POST", "path": "/v1/messages", "status": 200,
        "streamed": true, "receipt_id": "rcpt-merge", "verified": true,
        "locally_constrained": true, "rewritten": true,
        "detail": "receipt verified",
        "tag": "pap:req-merge:session-merge:claude-code"
    });
    apply_request_event(&mut state, verdict.as_object().unwrap()).unwrap();

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
            locally_constrained: None,
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
    assert_eq!(item.locally_constrained, Some(true));
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
    apply_request_event(&mut state, verdict.as_object().unwrap()).unwrap();
    assert_eq!(state.activity[0].status, 504);
    assert_eq!(state.activity[0].detail, "Client delivery timed out");
    assert_eq!(state.activity[0].input_tokens, Some(1_024));
    let mut pending = state.activity[0].clone();
    pending.id = "req-proof".into();
    pending.status = 502;
    pending.detail.clear();
    pending.verified = None;
    merge_activity(&mut state, pending);
    let mut withheld = verdict.clone();
    withheld["tag"] = json!("pap:req-proof:session-merge:claude-code");
    withheld["status"] = json!(502);
    withheld["detail"] = json!("Response withheld: receipt verification failed");
    withheld["verified"] = json!(false);
    apply_request_event(&mut state, withheld.as_object().unwrap()).unwrap();
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
