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
    assert_eq!(error.to_string(), "Authorization was declined");
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
        assert_eq!(
            result.err().unwrap().to_string(),
            "Choose a workspace before saving"
        );
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

/// A Phala device authorization mock whose token endpoint answers `errors`
/// in order (`detail.error`, as Phala nests them), then issues a key.
async fn phala_mock(
    errors: &'static [&'static str],
) -> (
    String,
    Arc<std::sync::atomic::AtomicUsize>,
    AbortOnDropHandle<std::io::Result<()>>,
) {
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
            match errors.get(calls.fetch_add(1, Ordering::SeqCst)) {
                Some(error) => (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"detail":{"error":error}})),
                ),
                None => (
                    StatusCode::OK,
                    Json(json!({"access_token":"sk-test-credential"})),
                ),
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
    (base, calls, server)
}

#[tokio::test]
async fn phala_polling_uses_the_device_authorization_contract() {
    let (base, calls, _server) = phala_mock(&["authorization_pending"]).await;
    let credential = phala_at(client().unwrap(), "test-device".into(), 0, &base)
        .await
        .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(
        matches!(credential.auth, ProfileAuth::OAuth { ref account_id, .. } if account_id == "alice")
    );
}

// The paused clock skips the polling sleeps. Polling ends before the metadata
// request and the client has no timeouts, so no timer can fire during I/O.
#[tokio::test(start_paused = true)]
async fn phala_slow_down_adds_five_seconds_to_later_polls() {
    let (base, calls, _server) = phala_mock(&["slow_down", "access_denied"]).await;
    let started = Instant::now();
    let error = phala_at(Client::new(), "test-device".into(), 0, &base)
        .await
        .err()
        .unwrap();
    assert_eq!(
        error,
        crate::Error::account("Account: Authorization was declined.")
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    // RFC 8628 §3.5.
    assert!(started.elapsed() >= Duration::from_secs(5));
}

#[test]
fn structured_account_errors_survive_the_management_boundary_without_raw_details() {
    let error = account_error(
        StatusCode::FORBIDDEN,
        &json!({"detail":{"error":"device_disabled","internal":"secret-do-not-show"}}),
    );
    let public = desktop_core::protocol::Error::from(error);
    assert_eq!(public.code, desktop_core::protocol::ErrorCode::AccountError);
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

#[tokio::test]
async fn redpill_code_flow_binds_state_issuer_and_pkce_to_one_exchange() {
    use axum::{routing::post, Form, Json};
    let challenge = Arc::new(std::sync::Mutex::new(String::new()));
    let expected_challenge = challenge.clone();
    let token = post(
        move |Form(form): Form<std::collections::HashMap<String, String>>| {
            let challenge = expected_challenge.lock().unwrap().clone();
            async move {
                assert_eq!(form["grant_type"], "authorization_code");
                assert_eq!(form["client_id"], REDPILL_CLIENT_ID);
                assert_eq!(form["redirect_uri"], callback_url());
                assert_eq!(form["code"], "one-use");
                let verifier = Sha256::digest(form["code_verifier"].as_bytes());
                assert_eq!(
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier),
                    challenge
                );
                Json(
                    json!({"access_token": "clerk-access", "token_type": "Bearer", "expires_in": 60}),
                )
            }
        },
    );
    let app = Router::new()
        .route("/oauth/token", token)
        .route(
            "/oauth/userinfo",
            get(|headers: HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer clerk-access");
                Json(json!({"sub": "user_alice"}))
            }),
        )
        .route(
            "/api/oauth/account",
            get(|| async {
                Json(json!({
                    "user_id": "user_alice", "user_name": "Alice",
                    "organization_id": "org_test", "organization_slug": "research", "organization_name": "Research",
                    "workspaces": [{"id": 7, "name": "Default", "is_default": true}]
                }))
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let _server = AbortOnDropHandle::new(tokio::spawn(
        async move { axum::serve(listener, app).await },
    ));

    let oauth = redpill_client(
        Url::parse(&format!("{ISSUER}/oauth/authorize")).unwrap(),
        Url::parse(&format!("{base}/oauth/token")).unwrap(),
    )
    .unwrap();
    let (url, state, verifier) = authorization_request(&oauth);
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(url.origin().ascii_serialization(), ISSUER);
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["response_mode"], "query");
    assert_eq!(query["client_id"], REDPILL_CLIENT_ID);
    assert_eq!(query["redirect_uri"], callback_url());
    assert_eq!(query["scope"], "openid profile user:org:read");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["state"], *state.secret());
    *challenge.lock().unwrap() = query["code_challenge"].clone();

    // The browser redirect reaches the loopback callback (RFC 8252 §7.3).
    let callback_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let callback_address = callback_listener.local_addr().unwrap();
    let (sender, receiver) = oneshot::channel();
    let callback = Arc::new(CallbackState {
        expected: state.secret().clone(),
        sender: Mutex::new(Some(sender)),
    });
    let code = tokio::spawn(receive_code(callback_listener, callback, receiver));
    let redirect = client()
        .unwrap()
        .get(format!(
            "http://{callback_address}/oauth/callback?state={}&code=one-use&iss={}",
            state.secret(),
            url::form_urlencoded::byte_serialize(ISSUER.as_bytes()).collect::<String>()
        ))
        .header("host", CALLBACK_ADDRESS.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(redirect.status(), StatusCode::OK);
    let code = code.await.unwrap().unwrap();

    let authorization = redpill(
        &client().unwrap(),
        &oauth,
        code,
        verifier,
        &format!("{base}/oauth/userinfo"),
        &format!("{base}/api/oauth/account"),
    )
    .await
    .unwrap();
    let Authorization::Redpill {
        access_token,
        details,
    } = authorization
    else {
        panic!("Expected a RedPill authorization")
    };
    assert_eq!(access_token, "clerk-access");
    assert_eq!(details.workspaces[0].id, 7);
}

#[tokio::test]
async fn redpill_token_errors_never_echo_the_response() {
    let app = Router::new().route(
        "/oauth/token",
        axum::routing::post(|| async {
            (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({"error": "invalid_grant", "error_description": "secret-detail"})),
            )
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let _server = AbortOnDropHandle::new(tokio::spawn(
        async move { axum::serve(listener, app).await },
    ));
    let oauth = redpill_client(
        Url::parse(&format!("{ISSUER}/oauth/authorize")).unwrap(),
        Url::parse(&format!("{base}/oauth/token")).unwrap(),
    )
    .unwrap();
    let (_, _, verifier) = authorization_request(&oauth);
    let error = redpill(
        &client().unwrap(),
        &oauth,
        "used".into(),
        verifier,
        &base,
        &base,
    )
    .await
    .err()
    .unwrap();
    let error = error.to_string();
    assert!(
        error.starts_with("Account: Service rejected the request"),
        "{error}"
    );
    assert!(!error.contains("secret"));
}
