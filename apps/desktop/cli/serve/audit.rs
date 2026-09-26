//! Receipt audits: after delivery for every recorded exchange, and on demand
//! from the standalone control endpoint.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::http::Method;
use backon::{ExponentialBuilder, Retryable};
use serde_json::Value;

use super::report::{rewrite_noted, summarize};
use super::{ProxyState, RecordedExchange, RequestOutcome, ResponseDelivery, TrustedIdentity};
use crate::checks::{
    parse_receipt_document, run_response_checks, session_id_from_receipt, UpstreamContext,
};
use crate::client::HttpResult;
use crate::transcript::Transcript;

pub(super) fn audit_exchange(
    state: Arc<ProxyState>,
    trusted: TrustedIdentity,
    exchange: RecordedExchange,
    bearer: Option<String>,
) {
    let report_state = state.clone();
    let report_exchange = exchange.clone();
    let report = move |verified, detail, rewritten, receipt| {
        let state = &report_state;
        let exchange = &report_exchange;
        if let Some(entry) = state
            .recorded
            .lock()
            .expect("recorded ring poisoned")
            .iter_mut()
            .rev()
            .find(|entry| entry.receipt_id == exchange.receipt_id && entry.at == exchange.at)
        {
            entry.verified = verified;
        }
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
    match &exchange.delivery {
        ResponseDelivery::Complete => {}
        ResponseDelivery::Failed(_) => {
            report(
                Some(false),
                "Response delivery failed before the complete response was received; the receipt does not match the partial response."
                    .into(),
                None,
                None,
            );
            return;
        }
        ResponseDelivery::Cancelled => {
            report(
                None,
                "Response stream was canceled or protection stopped; no complete response proof was recorded."
                    .into(),
                None,
                None,
            );
            return;
        }
    }
    let Ok(permit) = state.audits.clone().try_acquire_owned() else {
        report(
            None,
            "Response delivered; receipt audit deferred because the audit limit was reached".into(),
            None,
            None,
        );
        return;
    };
    tokio::spawn(async move {
        let _permit = permit;
        let result = tokio::select! {
            _ = state.shutdown.cancelled() => return,
            result = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                verify_exchange(&state, &trusted, &exchange, bearer.as_deref()),
            ) => result,
        };
        match result {
            Ok(Ok((transcript, detail, receipt))) => report(
                Some(transcript.verified()),
                format!("Post-delivery receipt audit: {detail}"),
                Some(rewrite_noted(&transcript)),
                receipt,
            ),
            Ok(Err(_)) => report(
                None,
                format!(
                    "Response delivered; receipt audit could not complete. Standalone serve can retry with POST /receipts/{}/verify.",
                    exchange.receipt_id
                ),
                None,
                None,
            ),
            Err(_) => report(
                None,
                format!(
                    "Response delivered; receipt audit timed out. Standalone serve can retry with POST /receipts/{}/verify.",
                    exchange.receipt_id
                ),
                None,
                None,
            ),
        }
    });
}

pub(super) async fn verify_exchange(
    state: &ProxyState,
    trusted: &TrustedIdentity,
    exchange: &RecordedExchange,
    bearer: Option<&str>,
) -> Result<(Transcript, String, Option<String>), String> {
    if matches!(&exchange.delivery, ResponseDelivery::Cancelled) {
        return Err(
            "the client canceled the response stream before its complete bytes were observed"
                .to_string(),
        );
    }
    let receipt_resp = fetch_receipt_for_audit(state, &exchange.receipt_id, bearer).await?;
    let receipt = receipt_resp.json().and_then(parse_receipt_document)?;
    // JSON is UTF-8 (RFC 8259 §8.1), so the checked document keeps its bytes.
    let document = String::from_utf8(receipt_resp.body).ok();

    let mut transcript = Transcript::default();
    let (session_resp, no_session_reason) = fetch_session_for_audit(state, &receipt).await;
    let session_bytes = session_resp.map(|resp| resp.body);
    run_response_checks(
        &mut transcript,
        &receipt,
        &trusted.identity,
        Some(&exchange.request),
        Some(&exchange.response),
        UpstreamContext {
            session_bytes: session_bytes.as_deref(),
            no_session_reason: &no_session_reason,
            pinned: (!exchange.pinned_sessions.is_empty())
                .then_some(exchange.pinned_sessions.as_slice()),
            requires_verified: state.enforce_verified || !exchange.pinned_sessions.is_empty(),
            serving: &trusted.report.service_capabilities.serving,
            required_claims: &state.required_claims,
        },
    );

    // The claims live in the session document (§8.3), not the receipt.
    let session = session_bytes.and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let mut detail = format!(
        "receipt {}: {}",
        exchange.receipt_id,
        summarize(
            &transcript,
            session.as_ref(),
            &trusted.report.service_capabilities.serving,
        )
    );
    if let ResponseDelivery::Failed(error) = &exchange.delivery {
        detail.push_str(&format!(
            " (response truncated at {} bytes: {error})",
            exchange.response.len
        ));
    }
    Ok((transcript, detail, document))
}

fn transient_audit_status(status: u16) -> bool {
    status == 404 || status == 429 || (500..600).contains(&status)
}

enum AuditFetchError {
    Status(u16),
    Transport(String),
}

/// Fetches a receipt or session, retrying transport failures and statuses
/// that may clear (not yet published, rate limited, server errors) four
/// times, 100 ms apart and doubling up to 1 s.
async fn fetch_audit_artifact<F, Fut>(mut fetch: F) -> Result<HttpResult, AuditFetchError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<HttpResult, String>>,
{
    let attempt = || {
        let response = fetch();
        async {
            match response.await {
                Ok(response) if (200..300).contains(&response.status) => Ok(response),
                Ok(response) => Err(AuditFetchError::Status(response.status)),
                Err(error) => Err(AuditFetchError::Transport(error)),
            }
        }
    };
    attempt
        .retry(
            ExponentialBuilder::default()
                .with_min_delay(Duration::from_millis(100))
                .with_max_delay(Duration::from_secs(1))
                .with_max_times(4),
        )
        .when(|error| match error {
            AuditFetchError::Status(status) => transient_audit_status(*status),
            AuditFetchError::Transport(_) => true,
        })
        .await
}

async fn fetch_receipt_for_audit(
    state: &ProxyState,
    receipt_id: &str,
    bearer: Option<&str>,
) -> Result<HttpResult, String> {
    match fetch_audit_artifact(|| {
        state
            .client
            .fetch_receipt(&state.base_url, receipt_id, bearer)
    })
    .await
    {
        Ok(response) => Ok(response),
        Err(AuditFetchError::Status(status)) => Err(format!("fetch returned HTTP {status}")),
        Err(AuditFetchError::Transport(error)) => Err(format!("fetch failed: {error}")),
    }
}

async fn fetch_session_for_audit(
    state: &ProxyState,
    receipt: &Value,
) -> (Option<HttpResult>, String) {
    let Some(session_id) = session_id_from_receipt(receipt) else {
        return (
            None,
            "receipt's upstream.verified carries no session_id".to_string(),
        );
    };
    match fetch_audit_artifact(|| state.client.fetch_session(&state.base_url, &session_id)).await {
        Ok(response) => (Some(response), String::new()),
        Err(AuditFetchError::Status(status)) => (
            None,
            format!("session {session_id} fetch returned HTTP {status}"),
        ),
        Err(AuditFetchError::Transport(error)) => {
            (None, format!("session {session_id} fetch failed: {error}"))
        }
    }
}
