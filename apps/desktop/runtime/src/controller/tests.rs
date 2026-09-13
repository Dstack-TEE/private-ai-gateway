use super::*;
use crate::contracts::{GatewayState, UsageSummary};

struct NoSidecar;
impl SidecarLauncher for NoSidecar {
    fn spawn(
        &self,
        _: Vec<String>,
    ) -> Result<
        (
            tokio::sync::mpsc::Receiver<crate::gateway::SidecarEvent>,
            Box<dyn crate::gateway::SidecarChild>,
        ),
        String,
    > {
        Err("No sidecar in this listener test".to_string())
    }
}

#[test]
fn launch_requires_instance_ownership_before_initialization() {
    const CASE_ENV: &str = "PAP_TEST_INSTANCE_OWNERSHIP";
    if let Ok(case) = std::env::var(CASE_ENV) {
        let executor = tokio::runtime::Runtime::new().unwrap();
        let result = DesktopRuntime::launch(RuntimeOptions {
            launcher: Arc::new(NoSidecar),
            helper_path: app_data_dir().unwrap().join("helper"),
            task_runtime: executor.handle().clone(),
        });
        let error = result
            .err()
            .expect("Launch must refuse an unavailable lock");
        match case.as_str() {
            "held" => assert_eq!(
                error,
                "Another Private AI Proxy instance is already running"
            ),
            "invalid" => assert!(error.starts_with("Cannot take the instance lock:")),
            _ => panic!("Unknown instance ownership test case"),
        }
        return;
    }

    for case in ["held", "invalid"] {
        let home = tempfile::tempdir().unwrap();
        let data = home.path().join(".private-ai-proxy");
        std::fs::create_dir(&data).unwrap();
        let _owner = if case == "held" {
            Some(lock::instance(&data).unwrap().unwrap())
        } else {
            std::fs::create_dir(data.join("instance.lock")).unwrap();
            None
        };
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "controller::tests::launch_requires_instance_ownership_before_initialization",
                "--nocapture",
            ])
            .env(CASE_ENV, case)
            .env(desktop_gateway::agents::HOME_OVERRIDE_ENV, home.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let entries: Vec<_> = std::fs::read_dir(&data)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("instance.lock")]);
    }
}

fn test_runtime(
    executor: &tokio::runtime::Runtime,
    directory: &std::path::Path,
) -> Arc<DesktopRuntime> {
    let (events, _) = tokio::sync::mpsc::channel(8);
    let proxy = ProxyState::new(events).unwrap();
    let usage = Arc::new(UsageStore::memory().unwrap());
    let manager = Arc::new(GatewayManager::new(
        proxy.clone(),
        usage.clone(),
        Arc::new(NoSidecar),
        executor.handle().clone(),
        GatewayState::default(),
    ));
    Arc::new(DesktopRuntime {
        manager,
        proxy,
        usage,
        secrets: Arc::new(desktop_gateway::secrets::MemoryStore::default()),
        credentials: ClientCredentials::from_files(TokenFiles::new(directory)),
        account_login: tokio::sync::Mutex::new(None),
        account_save: Mutex::new(None),
        balances: crate::balance_cache::BalanceCache::default(),
        legacy_credential_pending: Mutex::new(false),
        endpoint: EndpointRuntime::new(executor.handle().clone()),
        codex_sync: CodexCatalogSync::default(),
        agent_policy: Mutex::new(()),
        lifecycle: tokio::sync::Mutex::new(()),
        exiting: AtomicBool::new(false),
        helper_path: directory.join("helper"),
        recovery: crate::recovery::Recovery::default(),
        instance: None,
    })
}

#[test]
fn completed_authorization_is_staged_until_explicit_save_and_bound_to_its_provider() {
    use crate::{
        account_login::{Authorization, Credential, LoginPresentation, PendingLogin},
        contracts::{ProfileAuth, ServiceProvider},
    };
    let executor = tokio::runtime::Runtime::new().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let runtime = test_runtime(&executor, directory.path());
    executor.block_on(async {
        let profile = ConfidentialProfileInput {
            id: "profile-test".into(),
            name: "Phala".into(),
            provider: ServiceProvider::Phala,
            remote_url: "https://inference.phala.com".into(),
        };
        let auth = ProfileAuth::OAuth {
            account_id: "account-test".into(),
            account_name: Some("Personal".into()),
            images: None,
            scope: None,
        };
        let expected = auth.clone();
        let worker = tokio::spawn(async move {
            Ok(Authorization::Inference(Credential {
                key: "secret-not-for-the-renderer".into(),
                auth,
            }))
        });
        *runtime.account_login.lock().await = Some(PendingLogin::new(
            LoginPresentation {
                id: "login-test".into(),
                url: "https://cloud.phala.com/cli/verify".into(),
                user_code: None,
            },
            profile.clone(),
            worker,
        ));
        let completed = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let Some(auth) = runtime
                    .poll_account_login("login-test".into())
                    .await
                    .unwrap()
                {
                    break auth;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(completed.auth, expected);
        assert!(!serde_json::to_string(&completed)
            .unwrap()
            .contains("secret-not-for-the-renderer"));
        assert_eq!(
            runtime
                .poll_account_login("login-test".into())
                .await
                .unwrap()
                .map(|details| details.auth),
            Some(expected)
        );
        assert!(runtime.state().unwrap().profiles.is_empty());
        let different_provider = ConfidentialProfileInput {
            provider: ServiceProvider::Redpill,
            remote_url: "https://tee.redpill.ai".into(),
            ..profile
        };
        assert_eq!(
            runtime
                .save_account_login("login-test".into(), different_provider, true, None)
                .await
                .unwrap_err(),
            "Sign in again for the selected provider"
        );
        // A different editor must not replace the active authorization.
        let other = ConfidentialProfileInput {
            id: "another-profile".into(),
            name: "Other".into(),
            provider: ServiceProvider::Custom,
            remote_url: "https://example.com".into(),
        };
        assert!(runtime
            .begin_account_login(other.clone())
            .await
            .err()
            .unwrap()
            .contains("other window"));
        // Simulate a saved authorization left behind by a closed window.
        runtime
            .account_login
            .lock()
            .await
            .as_mut()
            .unwrap()
            .mark_saved();
        let reopened = ConfidentialProfileInput {
            id: "profile-test".into(),
            ..other
        };
        assert_eq!(
            runtime.begin_account_login(reopened).await.err().unwrap(),
            "Account login is only available for Phala and RedPill"
        );
        assert!(runtime.state().unwrap().profiles.is_empty());
        assert!(runtime.account_login.lock().await.is_none());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    });
}

#[test]
fn offline_removal_queues_cleanup_and_uncommitted_retirement_preserves_active_key() {
    const CHILD: &str = "PAP_TEST_CREDENTIAL_CLEANUP";
    if std::env::var_os(CHILD).is_none() {
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "controller::tests::offline_removal_queues_cleanup_and_uncommitted_retirement_preserves_active_key", "--nocapture"])
            .env(CHILD, "1").env(desktop_gateway::agents::HOME_OVERRIDE_ENV, home.path()).output().unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let executor = tokio::runtime::Runtime::new().unwrap();
    let directory = app_data_dir().unwrap();
    std::fs::create_dir_all(&directory).unwrap();
    let runtime = test_runtime(&executor, &directory);
    std::fs::write(directory.join("account-cleanup.pending"), "invalid JSON").unwrap();
    assert!(runtime.cleanup_manifest().unwrap().is_empty());
    assert!(!directory.join("account-cleanup.pending").exists());
    assert!(runtime.cleanup_manifest().unwrap().is_empty());
    let mut profile = service_config::resolve_profile(
        ConfidentialProfileInput {
            id: "profile-test".into(),
            name: "Test".into(),
            provider: crate::contracts::ServiceProvider::Redpill,
            remote_url: "https://tee.redpill.ai".into(),
        },
        Some(1),
    )
    .unwrap();
    profile.credential_ref = Some("credential-old".into());
    profile.credential_saved = Some(true);
    profile.auth = crate::contracts::ProfileAuth::OAuth {
        account_id: "user_test".into(),
        account_name: None,
        images: None,
        scope: None,
    };
    let entry = service_config::profile_credential_entry(&profile).unwrap();
    runtime.secrets.set(&entry, "old-secret").unwrap();
    let config = StartGatewayConfig {
        remote_url: profile.remote_url.clone(),
        require_production_os: true,
    };
    runtime.manager.set_service_configuration(
        config,
        vec![profile.clone()],
        profile.id.clone(),
        true,
        false,
    );
    runtime
        .queue_retired(RetiredCredential {
            profile_id: profile.id.clone(),
            action: "revoke".into(),
            provider: profile.provider.clone(),
            key: "old-secret".into(),
            entry: entry.clone(),
            revoke: true,
        })
        .unwrap();
    // A new local ref can select the same stable provider secret.
    let mut reselected = profile.clone();
    reselected.credential_ref = Some("credential-new".into());
    let selected_entry = service_config::profile_credential_entry(&reselected).unwrap();
    runtime.secrets.set(&selected_entry, "old-secret").unwrap();
    let config = runtime.state().unwrap().config;
    runtime.manager.set_service_configuration(
        config,
        vec![reselected],
        profile.id.clone(),
        true,
        false,
    );
    executor.block_on(runtime.cleanup_retired()).unwrap();
    assert!(runtime.secrets.get(&entry).unwrap().is_none());
    assert_eq!(
        runtime.secrets.get(&selected_entry).unwrap().as_deref(),
        Some("old-secret")
    );
    executor
        .block_on(runtime.delete_profile(profile.id))
        .unwrap();
    assert!(runtime.state().unwrap().profiles.is_empty());
    assert!(runtime.secrets.get(&entry).unwrap().is_none());
    let entries = runtime.cleanup_manifest().unwrap();
    assert_eq!(entries.len(), 1);
    let pending: RetiredCredential =
        serde_json::from_str(&runtime.secrets.get(&entries[0]).unwrap().unwrap()).unwrap();
    assert_eq!(pending.action, "revoke");
    assert!(directory.join("account-cleanup.pending").exists());
}

#[test]
fn saving_an_offline_profile_does_not_launch_verification() {
    const CHILD: &str = "PAP_TEST_SAVE_WITHOUT_VERIFY";
    if std::env::var_os(CHILD).is_none() {
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "controller::tests::saving_an_offline_profile_does_not_launch_verification",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env(desktop_gateway::agents::HOME_OVERRIDE_ENV, home.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let executor = tokio::runtime::Runtime::new().unwrap();
    let directory = app_data_dir().unwrap();
    std::fs::create_dir_all(&directory).unwrap();
    let runtime = test_runtime(&executor, &directory);
    let profile = ConfidentialProfileInput {
        id: "offline".into(),
        name: "Offline".into(),
        provider: crate::contracts::ServiceProvider::Custom,
        remote_url: "https://offline.invalid".into(),
    };
    let saved = executor
        .block_on(runtime.save_configuration(profile, true, Some("test-key".into())))
        .unwrap();
    assert_eq!(saved.active_profile_id, "offline");
    assert_eq!(saved.status, "stopped");
    assert!(saved.profiles[0].verified_at.is_none());
    assert!(service_config::profile_has_credential(&saved.profiles[0]));
    assert!(
        runtime.start(saved.config).is_err(),
        "Start must invoke the missing verifier"
    );
}

#[test]
fn busy_save_is_a_definite_rejection_not_an_unknown_operation() {
    let executor = tokio::runtime::Runtime::new().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let runtime = test_runtime(&executor, directory.path());
    let (_sender, receiver) =
        tokio::sync::watch::channel(crate::contracts::AccountSaveResult::Running);
    *runtime.account_save.lock().unwrap() = Some((uuid::Uuid::new_v4().to_string(), receiver));
    let profile = ConfidentialProfileInput {
        id: "profile-test".into(),
        name: "Test".into(),
        provider: crate::contracts::ServiceProvider::Redpill,
        remote_url: "https://tee.redpill.ai".into(),
    };
    let result = runtime
        .begin_account_save(
            uuid::Uuid::new_v4().to_string(),
            "login-test".into(),
            profile,
            true,
            None,
        )
        .unwrap();
    assert!(
        matches!(result, crate::contracts::AccountSaveResult::Failed { error } if error.contains("Another save"))
    );
}

#[test]
fn shutdown_blocks_later_configuration_changes() {
    let executor = tokio::runtime::Runtime::new().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let runtime = test_runtime(&executor, directory.path());
    executor.block_on(runtime.shutdown()).unwrap();
    let state = runtime.state().unwrap();
    assert_eq!(state.status, "stopped");
    assert_eq!(
        runtime.start(state.config).unwrap_err(),
        "The app is closing"
    );
    assert_eq!(
        runtime
            .apply_agent(
                "codex".into(),
                true,
                "revision".into(),
                ConnectOptions::default()
            )
            .unwrap_err(),
        "The app is closing"
    );
}

#[test]
fn finished_local_listener_can_restart_at_the_same_address() {
    let executor = tokio::runtime::Runtime::new().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let runtime = test_runtime(&executor, directory.path());
    executor.block_on(async {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let config = LocalApiConfig {
            port: listener.local_addr().unwrap().port(),
            ..Default::default()
        };
        let resolved = local_api::resolve(config.clone()).unwrap();
        runtime
            .endpoint
            .start(
                runtime.manager.clone(),
                runtime.proxy.clone(),
                listener,
                config,
            )
            .unwrap();
        runtime
            .endpoint
            .task
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .abort();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !runtime
                .endpoint
                .task
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .is_finished()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        runtime.restore_endpoint(resolved.clone()).unwrap();
        assert!(std::net::TcpListener::bind(resolved.bind).is_err());
        assert!(runtime.restore_endpoint(resolved.clone()).is_err());
        runtime.endpoint.stop().await.unwrap();
        assert!(std::net::TcpListener::bind(resolved.bind).is_ok());
    });
}

#[test]
fn recovery_keeps_agent_routes_and_scans_cannot_reauthorize_them() {
    const CHILD: &str = "PAP_TEST_RECOVERY_ROUTES";
    if std::env::var_os(CHILD).is_none() {
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "controller::tests::recovery_keeps_agent_routes_and_scans_cannot_reauthorize_them",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env(desktop_gateway::agents::HOME_OVERRIDE_ENV, home.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let executor = tokio::runtime::Runtime::new().unwrap();
    let directory = app_data_dir().unwrap();
    std::fs::create_dir_all(&directory).unwrap();
    let mut runtime = test_runtime(&executor, &directory);
    Arc::get_mut(&mut runtime).unwrap().instance = lock::instance(&directory).unwrap();
    let home = std::path::PathBuf::from(
        std::env::var_os(desktop_gateway::agents::HOME_OVERRIDE_ENV).unwrap(),
    );
    let cli = home.join(".local/bin").join(if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    });
    std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
    for path in [&cli, &runtime.helper_path] {
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let agent = Agent::ClaudeCode;
    let path = home.join(".claude/settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let original = r#"{"env":{"ANTHROPIC_BASE_URL":"https://api.anthropic.com"}}"#;
    std::fs::write(&path, original).unwrap();
    let catalog =
        Catalog::from_remote(&serde_json::json!({"data":[{"id":"test/model"}]}), 1).unwrap();
    let projector = runtime.current_projector().unwrap();
    let options = ConnectOptions {
        default_model: Some("test/model".into()),
    };
    let preview = projector
        .preview(agent, true, Some(&catalog), &options)
        .unwrap();
    projector
        .apply(agent, true, &preview.revision, Some(&catalog), &options)
        .unwrap();
    let projected = std::fs::read(&path).unwrap();
    let files = TokenFiles::new(&directory);
    let token = files.read(agent.id()).unwrap().unwrap();
    let verified = GatewayState {
        status: "verified".into(),
        api_key_saved: true,
        session_active: true,
        config: StartGatewayConfig {
            remote_url: "https://inference.phala.com".into(),
            require_production_os: true,
        },
        ..Default::default()
    };
    runtime.manager.restore_snapshot(verified.clone());
    runtime.proxy.publish(proxy::Session {
        verified: true,
        catalog: Some(catalog.clone()),
        ..Default::default()
    });
    runtime
        .publish_agent_tokens(projector.scan(Some(&catalog)).unwrap().1)
        .unwrap();
    assert_eq!(runtime.proxy.tokens().agent_for(&token), Some(agent.id()));
    runtime.recovery.available.store(false, Ordering::Release);
    runtime.recovery.request();
    runtime.recover_network().unwrap();
    let statuses = runtime.list_agents().unwrap();
    assert!(statuses.iter().all(|status| !status.authorized));
    assert!(runtime.proxy.tokens().agent_for(&token).is_none());
    assert_eq!(std::fs::read(&path).unwrap(), projected);
    for status in ["error", "stopped"] {
        runtime.manager.restore_snapshot(GatewayState {
            status: status.into(),
            ..verified.clone()
        });
        runtime.reload_agent_tokens().unwrap();
        assert!(runtime
            .list_agents()
            .unwrap()
            .iter()
            .all(|status| !status.authorized));
        assert!(runtime.proxy.tokens().agent_for(&token).is_none());
        assert_eq!(std::fs::read(&path).unwrap(), projected);
    }
    runtime.manager.restore_snapshot(verified);
    runtime.proxy.publish(proxy::Session {
        verified: true,
        catalog: Some(catalog),
        ..Default::default()
    });
    assert!(runtime
        .list_agents()
        .unwrap()
        .iter()
        .any(|status| status.id == agent.id() && status.authorized));
    assert_eq!(
        files.read(agent.id()).unwrap().as_deref(),
        Some(token.as_str())
    );
    assert_eq!(std::fs::read(&path).unwrap(), projected);
    runtime.stop().unwrap();
    assert!(files.read(agent.id()).unwrap().is_none());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(path).unwrap()).unwrap(),
        serde_json::from_str::<serde_json::Value>(original).unwrap()
    );
}

#[test]
fn network_loss_revokes_session_and_manual_stop_cancels_recovery() {
    let executor = tokio::runtime::Runtime::new().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let runtime = test_runtime(&executor, directory.path());
    runtime.manager.restore_snapshot(GatewayState {
        status: "verified".into(),
        session_id: Some("network-session".into()),
        session_active: true,
        protected_since: Some(123),
        session_usage: UsageSummary {
            requests: 7,
            ..Default::default()
        },
        config: StartGatewayConfig {
            remote_url: "https://inference.phala.com".into(),
            require_production_os: true,
        },
        ..Default::default()
    });
    runtime.proxy.publish(proxy::Session {
        verified: true,
        ..Default::default()
    });
    runtime.recovery.available.store(false, Ordering::Release);
    runtime.system_resumed();
    let operation = runtime.lifecycle.try_lock().unwrap();
    runtime.recover_network().unwrap();
    assert!(runtime.recovery.needs_check());
    drop(operation);
    let mut verifying = runtime.state().unwrap();
    verifying.status = "verifying".into();
    runtime.manager.restore_snapshot(verifying.clone());
    runtime.recovery.available.store(true, Ordering::Release);
    runtime.recover_network().unwrap();
    assert!(runtime.recovery.needs_check());
    runtime.recovery.available.store(false, Ordering::Release);
    runtime.manager.restore_snapshot(verifying);
    runtime.recover_network().unwrap();
    assert!(!runtime.recovery.needs_check());
    assert!(!runtime.proxy.session().verified);
    assert_eq!(runtime.state().unwrap().status, "stopped");
    assert!(runtime.state().unwrap().reconnecting);
    assert_eq!(
        runtime.state().unwrap().session_id.as_deref(),
        Some("network-session")
    );
    assert_eq!(runtime.state().unwrap().protected_since, Some(123));
    assert_eq!(runtime.state().unwrap().session_usage.requests, 7);
    assert!(runtime.recovery.pending());
    runtime.stop().unwrap();
    assert!(!runtime.state().unwrap().reconnecting);
    assert!(runtime.state().unwrap().protected_since.is_none());
    assert!(!runtime.recovery.pending());
    runtime.recovery.available.store(true, Ordering::Release);
    runtime.recover_network().unwrap();
    assert_eq!(runtime.state().unwrap().status, "stopped");
}

#[test]
fn failed_and_noop_imports_preserve_recovery_and_monitor_state() {
    let executor = tokio::runtime::Runtime::new().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let runtime = test_runtime(&executor, directory.path());
    runtime.recovery.wait();
    runtime.system_resumed();
    assert!(runtime
        .import_profiles(crate::maintenance::ProfileBackup {
            version: 99,
            profiles: vec![]
        })
        .is_err());
    assert!(runtime.recovery.pending());
    assert!(runtime.recovery.needs_check());
    assert_eq!(
        runtime
            .import_profiles(crate::maintenance::ProfileBackup {
                version: 1,
                profiles: vec![]
            })
            .unwrap()
            .imported,
        0
    );
    assert!(runtime.recovery.pending());
    assert!(runtime.recovery.needs_check());
    let snapshot = runtime.state().unwrap();
    assert!(runtime.set_wake_monitor_available(false));
    assert!(!runtime.set_wake_monitor_available(false));
    runtime.manager.restore_snapshot(snapshot);
    assert_eq!(runtime.state().unwrap().wake_monitor_available, Some(false));
    runtime.stop().unwrap();
    assert!(!runtime.recovery.needs_check());
    assert_eq!(runtime.state().unwrap().wake_monitor_available, Some(false));
    assert!(runtime.set_wake_monitor_available(true));
    assert_eq!(runtime.state().unwrap().wake_monitor_available, Some(true));
}

#[test]
fn client_key_rotation_preserves_agent_tokens_and_fails_closed() {
    let executor = tokio::runtime::Runtime::new().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let runtime = test_runtime(&executor, directory.path());
    let original = runtime.client_key().unwrap();
    let mut tokens = TokenSet::default();
    tokens.insert(original.clone(), LOCAL_TOOLS_AGENT.to_string());
    tokens.insert("agent-token".to_string(), "codex".to_string());
    runtime.proxy.set_tokens(tokens);

    let rotated = runtime.rotate_client_key().unwrap();
    // Subscribers arriving after rotation must see the new non-secret revision.
    assert_eq!(runtime.subscribe().borrow().client_key_revision, 1);
    assert_eq!(
        runtime.subscribe().borrow().client_key_available,
        Some(true)
    );
    assert_ne!(rotated, original);
    assert_eq!(runtime.client_key().unwrap(), rotated);
    assert_eq!(runtime.proxy.tokens().agent_for(&original), None);
    assert_eq!(
        runtime.proxy.tokens().agent_for(&rotated),
        Some(LOCAL_TOOLS_AGENT)
    );
    assert_eq!(
        runtime.proxy.tokens().agent_for("agent-token"),
        Some("codex")
    );

    let token_path = TokenFiles::new(directory.path()).path(LOCAL_TOOLS_AGENT);
    std::fs::remove_file(&token_path).unwrap();
    std::fs::create_dir(&token_path).unwrap();
    assert!(runtime.rotate_client_key().is_err());
    assert_eq!(runtime.subscribe().borrow().client_key_revision, 2);
    assert_eq!(
        runtime.subscribe().borrow().client_key_available,
        Some(false)
    );
    assert_eq!(runtime.proxy.tokens().agent_for(&rotated), None);
    assert_eq!(
        runtime.proxy.tokens().agent_for("agent-token"),
        Some("codex")
    );
    // A readable leftover must not be admitted by a later scan or key read.
    std::fs::remove_dir(&token_path).unwrap();
    std::fs::write(&token_path, &rotated).unwrap();
    assert!(runtime.client_key().is_err());
    let active = with_client_token(runtime.proxy.tokens(), &runtime.credentials).unwrap();
    assert_eq!(active.agent_for(&rotated), None);
    assert_eq!(active.agent_for("agent-token"), Some("codex"));
    let replacement = runtime.rotate_client_key().unwrap();
    assert_ne!(replacement, rotated);
    assert_eq!(runtime.client_key().unwrap(), replacement);
}

#[test]
fn occupied_listener_preserves_previous_endpoint_and_serializes_mutations() {
    let executor = tokio::runtime::Runtime::new().unwrap();
    executor.block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let old_listener = proxy::bind_std("127.0.0.1:0".parse().unwrap()).unwrap();
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let config = LocalApiConfig {
            port: old_listener.local_addr().unwrap().port(),
            ..LocalApiConfig::default()
        };
        let original = local_api::resolve(config.clone()).unwrap();
        let runtime = test_runtime(&executor, temp.path());
        runtime
            .manager
            .set_endpoint(config.clone(), Ok(original.endpoint.clone()));
        runtime
            .endpoint
            .start(
                runtime.manager.clone(),
                runtime.proxy.clone(),
                old_listener,
                config.clone(),
            )
            .unwrap();
        let gate = runtime.lifecycle.lock().await;
        assert!(runtime.stop().unwrap_err().contains("in progress"));
        assert!(runtime
            .save_local_api_config(config.clone())
            .await
            .unwrap_err()
            .contains("in progress"));
        drop(gate);
        let candidate = LocalApiConfig {
            port: occupied.local_addr().unwrap().port(),
            ..config.clone()
        };
        // A secondary instance cannot touch the listener or persisted config.
        assert!(runtime
            .save_local_api_config(candidate.clone())
            .await
            .unwrap_err()
            .contains("primary"));
        assert!(std::net::TcpStream::connect(original.bind).is_ok());
        // A failed reservation keeps the existing listener alive.
        assert!(runtime
            .rebind_local_api(
                candidate.clone(),
                original.clone(),
                local_api::resolve(candidate).unwrap()
            )
            .await
            .is_err());
        let state = runtime.state().unwrap();
        assert_eq!(state.local_api, config);
        assert_eq!(state.proxy_url.as_deref(), Some(original.endpoint.as_str()));
        assert!(state.endpoint_error.is_none());
        assert!(std::net::TcpStream::connect(original.bind).is_ok());
        runtime.endpoint.stop().await.unwrap();
    });
}

#[test]
fn only_live_protection_allows_agent_projection() {
    let mut state = GatewayState {
        api_key_saved: true,
        ..GatewayState::default()
    };
    for status in ["stopped", "verifying", "blocked", "error"] {
        state.status = status.to_string();
        assert!(!protection_active(&state));
    }
    state.status = "verified".to_string();
    assert!(protection_active(&state));
    state.configuration_verification = true;
    assert!(!protection_active(&state));
    state.configuration_verification = false;
    state.api_key_saved = false;
    assert!(!protection_active(&state));
    state.api_key_saved = true;
    state.endpoint_error = Some("Listener unavailable".to_string());
    assert!(!protection_active(&state));
}
