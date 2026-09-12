use super::*;

pub fn router(state: Arc<ProxyState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .route("/v1/responses", post(responses))
        .route("/v1/responses/compact", post(responses_compact))
        .fallback(not_found)
        .with_state(state)
}

async fn health(State(state): State<Arc<ProxyState>>) -> Response {
    let session = state.session();
    let models = session
        .catalog
        .as_ref()
        .map_or(0, |catalog| catalog.models.len());
    Json(json!({ "status": "ok", "verified": session.verified, "models": models })).into_response()
}

pub(super) async fn models(State(state): State<Arc<ProxyState>>, headers: HeaderMap) -> Response {
    let surface = Surface::ChatCompletions;
    let auth = match state.authorize(&headers, "/v1/models") {
        Ok(auth) => auth,
        Err(rejection) => {
            return error_response(
                surface,
                rejection.status,
                rejection.code,
                &rejection.message,
            )
        }
    };
    let session = state.session();
    match (session.verified, session.catalog) {
        (true, Some(catalog)) => {
            let catalog = Agent::from_id(&auth.agent)
                .map(|agent| catalog.for_surface(agent.surface()))
                .unwrap_or(catalog);
            Json(catalog.openai_list()).into_response()
        }
        _ => error_response(
            surface,
            StatusCode::SERVICE_UNAVAILABLE,
            "gateway_not_verified",
            "The verified model list is not available until verification succeeds",
        ),
    }
}

async fn chat_completions(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Body,
) -> Response {
    relay(
        state,
        headers,
        query,
        body,
        Surface::ChatCompletions,
        "/v1/chat/completions",
    )
    .await
}

async fn messages(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Body,
) -> Response {
    relay(
        state,
        headers,
        query,
        body,
        Surface::Messages,
        "/v1/messages",
    )
    .await
}

async fn responses(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Body,
) -> Response {
    relay(
        state,
        headers,
        query,
        body,
        Surface::Responses,
        "/v1/responses",
    )
    .await
}

/// Helper endpoints belong to their surface and are gated exactly like
/// inference: agent scope, verified session, and a model present in the
/// catalog (both protocols require `model`).
async fn count_tokens(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Body,
) -> Response {
    relay(
        state,
        headers,
        query,
        body,
        Surface::Messages,
        "/v1/messages/count_tokens",
    )
    .await
}

async fn responses_compact(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Body,
) -> Response {
    relay(
        state,
        headers,
        query,
        body,
        Surface::Responses,
        "/v1/responses/compact",
    )
    .await
}

async fn not_found() -> Response {
    error_response(
        Surface::ChatCompletions,
        StatusCode::NOT_FOUND,
        "not_found",
        &format!("{PRODUCT_NAME} serves /v1/models, /v1/chat/completions, /v1/messages, and /v1/responses"),
    )
}
