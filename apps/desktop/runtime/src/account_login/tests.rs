use super::*;

#[test]
fn account_presentation_uses_clerk_identity_and_restricts_avatar_sources() {
    let data = json!({
        "user_id": "user_alice", "user_name": "Alice Example",
        "user_image_url": "https://img.clerk.com/alice",
        "organization_id": "org_test", "organization_slug": "research-team", "organization_name": "Research", "organization_image_url": "https://images.clerk.dev/research",
        "workspaces": [{"id": 1, "name": "Default", "is_default": true}]
    });
    let details = redpill_details(&data).unwrap();
    let ProfileAuth::OAuth {
        account_id,
        account_name,
        images,
        scope,
    } = details.auth
    else {
        panic!("Expected OAuth identity")
    };
    assert_eq!(account_id, "user_alice");
    assert_eq!(account_name.as_deref(), Some("Alice Example"));
    assert_eq!(
        images.unwrap().organization.as_deref(),
        Some("https://images.clerk.dev/research")
    );
    assert_eq!(scope.unwrap().organization.as_deref(), Some("Research"));
    for url in [
        "http://img.clerk.com/a",
        "https://example.com/a",
        "https://img.clerk.com.evil.test/a",
        "https://user:secret@img.clerk.com/a",
    ] {
        assert!(avatar_url(&json!({"image":url}), "image").is_none());
    }
}

#[test]
fn billing_permissions_hide_denied_balances_and_bind_links_to_the_organization() {
    let denied = json!({"detail":{"error":"billing_permission_required"}});
    assert!(parse_account_balance(
        &ServiceProvider::Redpill,
        StatusCode::UNAUTHORIZED,
        &json!({})
    )
    .unwrap()
    .is_none());
    assert!(
        parse_account_balance(&ServiceProvider::Redpill, StatusCode::FORBIDDEN, &denied)
            .unwrap()
            .is_none()
    );
    assert!(parse_account_balance(
        &ServiceProvider::Redpill,
        StatusCode::SERVICE_UNAVAILABLE,
        &json!({})
    )
    .is_err());
    let data = json!({"balance_usd":"0", "organization_id":"org_test", "organization_slug":"research-team", "organization_name":"Research", "can_top_up":false});
    let balance = parse_account_balance(&ServiceProvider::Redpill, StatusCode::OK, &data)
        .unwrap()
        .unwrap();
    assert_eq!(balance.balance_usd, "0");
    let phala = parse_account_balance(&ServiceProvider::Phala, StatusCode::OK,
        &json!({"workspace":{"name":"Research","slug":"phala-research"},"credits":{"balance":"2","granted_balance":"0"}})
    ).unwrap().unwrap();
    assert_eq!(
        top_up_url(
            &ServiceProvider::Phala,
            phala.scope.workspace_slug.as_deref()
        )
        .unwrap(),
        "https://cloud.phala.com/phala-research/billing"
    );
    assert!(!balance.can_top_up);
    assert_eq!(
        organization_url(Some("research-team")).unwrap(),
        "https://redpill.ai/research-team"
    );
    for field in ["organization_id", "organization_slug", "can_top_up"] {
        let mut incomplete = data.clone();
        incomplete.as_object_mut().unwrap().remove(field);
        assert!(
            parse_account_balance(&ServiceProvider::Redpill, StatusCode::OK, &incomplete).is_err()
        );
    }
    assert_eq!(
        top_up_url(
            &ServiceProvider::Redpill,
            balance.scope.organization_slug.as_deref()
        )
        .unwrap(),
        "https://redpill.ai/research-team/credits"
    );
    for id in [
        None,
        Some(""),
        Some("org_"),
        Some("org_../other"),
        Some("org_test?other"),
    ] {
        assert!(top_up_url(&ServiceProvider::Redpill, id).is_err());
        assert!(top_up_url(&ServiceProvider::Phala, id).is_err());
        assert!(organization_url(id).is_err());
    }
}

#[tokio::test]
async fn failed_authorization_is_terminal_and_repeatable() {
    let profile = ConfidentialProfileInput {
        id: "profile-test".into(),
        name: "Phala".into(),
        provider: ServiceProvider::Phala,
        remote_url: "https://inference.phala.com".into(),
    };
    let presentation = LoginPresentation {
        id: "login-test".into(),
        url: "https://cloud.phala.com/cli/verify".into(),
        user_code: None,
    };
    let mut pending = PendingLogin::new(
        presentation,
        profile,
        tokio::spawn(async { Err("Authorization was declined".into()) }),
    );
    let error = timeout(Duration::from_secs(5), async {
        loop {
            match pending.poll("login-test").await {
                Err(error) => break error,
                Ok(_) => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(error, "Authorization was declined");
    assert_eq!(pending.poll("login-test").await.unwrap_err(), error);
    assert!(!pending.is_active());
    pending.cancel().await.unwrap();
}

#[tokio::test]
async fn redpill_requires_an_explicit_accessible_workspace() {
    let profile = ConfidentialProfileInput {
        id: "profile-test".into(),
        name: "RedPill".into(),
        provider: ServiceProvider::Redpill,
        remote_url: "https://tee.redpill.ai".into(),
    };
    let authorization = Authorization::Redpill {
        access_token: "must-not-be-sent".into(),
        details: AccountLoginDetails {
            auth: ProfileAuth::OAuth {
                account_id: "user_test".into(),
                account_name: None,
                images: None,
                scope: None,
            },
            workspaces: vec![AccountWorkspace {
                id: 7,
                name: "Research".into(),
                is_default: false,
            }],
        },
    };
    for selection in [None, Some(999)] {
        let result = authorization.issue(&profile, selection).await;
        assert_eq!(result.err().unwrap(), "Choose a workspace before saving");
    }
}

#[test]
fn workspace_and_balance_responses_are_validated() {
    let valid = AccountWorkspace {
        id: 7,
        name: "Research".into(),
        is_default: false,
    };
    assert!(validate_workspaces(std::slice::from_ref(&valid)).is_ok());
    assert!(validate_workspaces(&[valid.clone(), valid]).is_err());
    assert!(validate_workspaces(&[]).is_err());
    for value in ["NaN", "inf", "not-a-number"] {
        assert!(amount(&json!({"balance": value}), "balance").is_err());
    }
    assert_eq!(
        amount(&json!({"balance": "-1.25"}), "balance").unwrap(),
        "-1.25"
    );
}

#[tokio::test]
async fn phala_polling_uses_the_device_authorization_contract() {
    use axum::{routing::post, Json};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let token_calls = calls.clone();
    let tokens = post(move |Json(body): Json<Value>| {
        let calls = token_calls.clone();
        async move {
            assert_eq!(body["device_code"], "test-device");
            assert_eq!(
                body["grant_type"],
                "urn:ietf:params:oauth:grant-type:device_code"
            );
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"detail":{"error":"authorization_pending"}})),
                )
            } else {
                (
                    StatusCode::OK,
                    Json(json!({"access_token":"sk-test-credential"})),
                )
            }
        }
    });
    let app = Router::new()
        .route("/api/v1/auth/device/token", tokens)
        .route(
            "/api/v1/private_ai/self",
            get(|headers: HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer sk-test-credential");
                Json(json!({"user":{"username":"alice"},"workspace":{"name":"Research","slug":"research-team"}}))
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = AbortOnDropHandle::new(tokio::spawn(
        async move { axum::serve(listener, app).await },
    ));
    let credential = phala_at(client().unwrap(), "test-device".into(), 0, &base)
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(
        matches!(credential.auth, ProfileAuth::OAuth { ref account_id, .. } if account_id == "alice")
    );
    server.abort();
}

#[test]
fn structured_account_errors_survive_the_management_boundary_without_raw_details() {
    let error = account_error(
        StatusCode::FORBIDDEN,
        &json!({"detail":{"error":"device_disabled","internal":"secret-do-not-show"}}),
    );
    let public = crate::protocol::RpcError::operation(&error);
    assert!(public.message.contains("disabled"));
    assert!(!public.message.contains("secret"));
}

#[tokio::test]
async fn pasted_callback_is_bound_to_session_and_consumed_once() {
    let profile = ConfidentialProfileInput {
        id: "test".into(),
        name: "Test".into(),
        provider: ServiceProvider::Redpill,
        remote_url: "https://tee.redpill.ai".into(),
    };
    let mut pending = PendingLogin::new(
        LoginPresentation {
            id: "login".into(),
            url: "https://clerk.redpill.ai".into(),
            user_code: None,
        },
        profile,
        tokio::spawn(std::future::pending()),
    );
    let (sender, receiver) = oneshot::channel();
    pending.callback = Some(Arc::new(CallbackState {
        expected: "expected".into(),
        sender: Mutex::new(Some(sender)),
    }));
    for url in [
        "https://attacker.test/oauth/callback?state=expected&code=secret",
        "http://127.0.0.1:4181/oauth/callback?state=wrong&code=secret",
        "http://127.0.0.1:4181/other?state=expected&code=secret",
        "http://127.0.0.1:4181/oauth/callback?state=expected&state=expected&code=secret",
    ] {
        assert!(pending.complete_callback("login", url).await.is_err());
    }
    let url = "http://127.0.0.1:4181/oauth/callback?state=expected&code=secret";
    assert!(pending.complete_callback("other-login", url).await.is_err());
    pending.complete_callback("login", url).await.unwrap();
    assert_eq!(receiver.await.unwrap().unwrap(), "secret");
    assert!(pending.complete_callback("login", url).await.is_err());
    assert!(!callback_page(true).contains("secret"));
    assert!(callback_page(true).contains(&account_return_url()));
    assert!(!callback_page(false).contains(&account_return_url()));
    let return_url = Url::parse(&account_return_url()).unwrap();
    assert!(return_url.query().is_none() && return_url.fragment().is_none());
}

#[test]
fn callback_binds_host_state_and_issuer_and_rejects_duplicates() {
    let mut headers = HeaderMap::new();
    headers.insert("host", "127.0.0.1:4181".parse().unwrap());
    let valid: Uri =
        "/oauth/callback?state=expected&code=one-use&iss=https%3A%2F%2Fclerk.redpill.ai"
            .parse()
            .unwrap();
    assert_eq!(
        callback_code(&valid, &headers, "expected").unwrap(),
        "one-use"
    );
    for query in [
        "state=wrong&code=x",
        "state=expected&state=other&code=x",
        "state=expected&code=x&code=y",
        "state=expected&code=x&iss=https://attacker.invalid",
        "state=expected&code=x&iss=https://clerk.redpill.ai&iss=https://attacker.invalid",
    ] {
        let uri = format!("/oauth/callback?{query}").parse().unwrap();
        assert!(callback_code(&uri, &headers, "expected").is_err());
    }
    headers.insert("host", "attacker.invalid:4181".parse().unwrap());
    assert!(callback_code(&valid, &headers, "expected").is_err());
}

#[test]
fn authorization_urls_cannot_redirect_credentials_to_another_origin() {
    for url in [
        "http://clerk.redpill.ai/oauth/token",
        "https://clerk.redpill.ai.attacker.invalid/oauth/token",
        "https://user@clerk.redpill.ai/oauth/token",
        "https://clerk.redpill.ai/oauth/token#fragment",
    ] {
        assert!(trusted_url(url, ISSUER).is_err());
    }
    assert!(trusted_url("https://clerk.redpill.ai/oauth/token", ISSUER).is_ok());
}

#[test]
fn unsupported_pkce_discovery_fails_closed() {
    let mut discovery = json!({"issuer":ISSUER,"grant_types_supported":["authorization_code"],"code_challenge_methods_supported":["S256"],"token_endpoint_auth_methods_supported":["none"]});
    assert!(validate_discovery(&discovery).is_ok());
    discovery["code_challenge_methods_supported"] = json!(["plain"]);
    assert!(validate_discovery(&discovery).is_err());
}
