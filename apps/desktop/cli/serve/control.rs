//! The standalone `serve` control endpoint: list recorded exchanges and
//! re-verify one on demand.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::Path;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use axum::{Json, Router};
use serde_json::{json, Value};

use super::audit::verify_exchange;
use super::report::rewrite_noted;
use super::{bearer_token, internal_error, ProxyState, RequestOutcome, ResponseDelivery};

pub(super) fn build_control_router(state: Arc<ProxyState>) -> Router {
    Router::new()
        .route("/receipts", axum::routing::get(control_list))
        .route("/receipts/{id}/verify", axum::routing::post(control_verify))
        .with_state(state)
}

/// GET /receipts on the standalone control endpoint: newest first.
async fn control_list(
    axum::extract::State(state): axum::extract::State<Arc<ProxyState>>,
) -> Json<Value> {
    let recorded = state.recorded.lock().expect("recorded ring poisoned");
    Json(Value::Array(
        recorded
            .iter()
            .rev()
            .map(|exchange| {
                json!({
                    "receipt_id": exchange.receipt_id,
                    "path": exchange.path,
                    "status": exchange.status,
                    "streamed": exchange.streamed,
                    "truncated": matches!(&exchange.delivery, ResponseDelivery::Failed(_)),
                    "cancelled": matches!(&exchange.delivery, ResponseDelivery::Cancelled),
                    "at": exchange.at,
                    "verified": exchange.verified,
                })
            })
            .collect(),
    ))
}

/// Re-run receipt verification against the stored digests. Authorization is
/// forwarded only to the out-of-band receipt fetch when the upstream needs it.
async fn control_verify(
    axum::extract::State(state): axum::extract::State<Arc<ProxyState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let exchange = {
        let recorded = state.recorded.lock().expect("recorded ring poisoned");
        recorded
            .iter()
            .rev()
            .find(|exchange| exchange.receipt_id == id)
            .cloned()
    };
    let Some(exchange) = exchange else {
        return json_response(
            StatusCode::NOT_FOUND,
            json!({ "error": format!("no recorded exchange cites receipt {id}") }),
        );
    };
    let bearer = bearer_token(&headers);
    let trusted = state.snapshot();
    let report = |verified: Option<bool>,
                  rewritten: Option<bool>,
                  detail: String,
                  receipt: Option<String>| {
        (state.reporter)(RequestOutcome {
            method: Method::POST,
            path: exchange.path.clone(),
            status: exchange.status,
            streamed: exchange.streamed,
            receipt_id: Some(exchange.receipt_id.clone()),
            receipt,
            verified,
            detail,
            context: exchange.context.clone(),
            rewritten,
            local_policy_applied: exchange.local_policy_applied,
        });
    };
    match verify_exchange(&state, &trusted, &exchange, bearer.as_deref()).await {
        Ok((transcript, detail, receipt)) => {
            let verified = transcript.verified();
            let rewritten = Some(rewrite_noted(&transcript));
            if let Some(entry) = state
                .recorded
                .lock()
                .expect("recorded ring poisoned")
                .iter_mut()
                .rev()
                .find(|entry| entry.receipt_id == id)
            {
                entry.verified = Some(verified);
            }
            report(Some(verified), rewritten, detail, receipt);
            let mut body = transcript.to_json(false);
            body["receipt_id"] = json!(id);
            json_response(StatusCode::OK, body)
        }
        Err(error) => {
            let detail = format!("receipt {id}: {error}");
            if let Some(entry) = state
                .recorded
                .lock()
                .expect("recorded ring poisoned")
                .iter_mut()
                .rev()
                .find(|entry| entry.receipt_id == id)
            {
                entry.verified = None;
            }
            report(None, None, detail.clone(), None);
            json_response(StatusCode::BAD_GATEWAY, json!({ "error": detail }))
        }
    }
}

pub(super) fn json_response(status: StatusCode, body: Value) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| internal_error())
}
