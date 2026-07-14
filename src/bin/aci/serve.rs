//! `aci serve`: a local OpenAI-compatible proxy that fails closed on the
//! attested service. On startup it fully verifies `<base-url>` (§10.1) and
//! refuses to listen unless the verdict is VERIFIED. Every upstream hop
//! enforces the attested TLS SPKI pin, streaming responses pass through
//! byte-exact, and after each inference response completes the proxy fetches
//! and verifies its receipt (§10.2–10.3) out of band — a one-line verdict per
//! request, loud on failure, always keep serving. No bodies are logged and no
//! artifacts are stored.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use futures_util::StreamExt;
use private_ai_gateway::aci::tee::{CompletionHook, TeeStream};
use private_ai_gateway::aci::types::AttestationReport;
use serde_json::Value;

use crate::args::ServeArgs;
use crate::checks::{
    established_identity, fetch_live_session, parse_receipt_envelope, run_receipt_checks,
    run_upstream_checks, ReceiptContext,
};
use crate::client::AciClient;
use crate::transcript::{Status, Transcript};
use crate::verify::{verify_service, ServiceVerification};

/// Request headers copied verbatim onto the upstream request. `authorization`
/// is forwarded unchanged (the proxy never inspects or stores the credential).
const FORWARD_REQUEST_HEADERS: &[&str] = &["authorization", "content-type", "accept"];

/// Upstream response headers surfaced back to the local client (§6.2).
const COPY_RESPONSE_HEADERS: &[&str] = &[
    "content-type",
    "x-receipt-id",
    "x-aci-version",
    "x-aci-keyset-digest",
    "x-e2ee-applied",
    "cache-control",
    "x-accel-buffering",
];

/// What happened to one forwarded request; the reporter turns it into a line.
pub struct RequestOutcome {
    pub method: &'static str,
    pub path: String,
    pub status: u16,
    pub streamed: bool,
    /// `Some` for inference requests (receipt verified/failed); `None` for
    /// passthrough GETs and for responses that carried no receipt to check.
    pub verified: Option<bool>,
    /// The one-line detail printed after the request line, e.g.
    /// `receipt rcpt-1: signature ok, wire hash ok, upstream tee_attested asserted (hardware_proven)`.
    pub detail: String,
}

type Reporter = Arc<dyn Fn(RequestOutcome) + Send + Sync>;

/// The attested identity the proxy currently trusts. Replaced wholesale when a
/// keyset rotation forces a fresh verify.
#[derive(Clone)]
struct TrustedIdentity {
    report: Arc<AttestationReport>,
    keyset_digest: String,
}

pub struct ProxyState {
    client: AciClient,
    base_url: String,
    host: String,
    trusted: Mutex<TrustedIdentity>,
    /// Set when an upstream response advertised a keyset digest other than the
    /// trusted one; blocks inference forwards until a fresh verify passes.
    blocked: AtomicBool,
    /// Serializes the re-verify so a burst of blocked requests reverifies once.
    reverify: tokio::sync::Mutex<()>,
    reporter: Reporter,
}

impl ProxyState {
    fn new(
        client: AciClient,
        base_url: String,
        host: String,
        report: AttestationReport,
        reporter: Reporter,
    ) -> Self {
        let keyset_digest = report.workload_keyset_digest.clone();
        Self {
            client,
            base_url,
            host,
            trusted: Mutex::new(TrustedIdentity {
                report: Arc::new(report),
                keyset_digest,
            }),
            blocked: AtomicBool::new(false),
            reverify: tokio::sync::Mutex::new(()),
            reporter,
        }
    }

    fn snapshot(&self) -> TrustedIdentity {
        self.trusted
            .lock()
            .expect("trusted identity poisoned")
            .clone()
    }

    /// When blocked by a keyset change, re-verify the service once and, on
    /// success, re-pin the TLS key and adopt the new identity. Returns `Ok`
    /// when forwarding may proceed.
    async fn ensure_unblocked(self: &Arc<Self>) -> Result<(), String> {
        if !self.blocked.load(Ordering::SeqCst) {
            return Ok(());
        }
        let _guard = self.reverify.lock().await;
        if !self.blocked.load(Ordering::SeqCst) {
            return Ok(());
        }
        let verification = verify_service(&self.base_url, None, false).await?;
        if !verification.transcript.verified() {
            return Err("service re-verification did not reach VERIFIED".to_string());
        }
        if let Some(spki) = &verification.observed_spki {
            self.client.pin(&self.host, spki);
        }
        let keyset_digest = verification.report.workload_keyset_digest.clone();
        *self.trusted.lock().expect("trusted identity poisoned") = TrustedIdentity {
            report: Arc::new(verification.report),
            keyset_digest,
        };
        self.blocked.store(false, Ordering::SeqCst);
        eprintln!("aci serve: re-verified after keyset change; resuming forwards");
        Ok(())
    }
}

pub async fn run(args: ServeArgs) -> Result<i32, String> {
    let verification = verify_service(&args.base_url, None, false).await?;
    println!("== service verification: {} ==", verification.base_url);
    print!("{}", verification.transcript.render_human(false));
    if !verification.transcript.verified() {
        return Err(
            "service verification failed; refusing to start the proxy (fail closed)".to_string(),
        );
    }

    let ServiceVerification {
        report,
        client,
        base_url,
        host,
        observed_spki,
        ..
    } = verification;
    // Pin the just-verified TLS key on every future hop to this host.
    if let Some(spki) = &observed_spki {
        client.pin(&host, spki);
    }
    let state = Arc::new(ProxyState::new(
        client,
        base_url.clone(),
        host,
        report,
        Arc::new(default_reporter),
    ));

    let listen = args.listen.as_deref().unwrap_or("127.0.0.1:4180");
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| format!("cannot bind {listen}: {e}"))?;
    let local = listener
        .local_addr()
        .map_err(|e| format!("cannot read listen address: {e}"))?;

    println!();
    println!("aci serve: proxying {base_url} on http://{local} (plain HTTP, localhost)");
    println!(
        "forwarding /v1/chat/completions, /v1/completions, /v1/embeddings, GET /v1/models and \
         /v1/aci/*; Authorization passed through unchanged; every upstream hop pinned to the \
         attested TLS key; receipts verified after each response."
    );
    println!();

    axum::serve(listener, build_proxy_router(state))
        .await
        .map_err(|e| format!("proxy server error: {e}"))?;
    Ok(0)
}

fn build_proxy_router(state: Arc<ProxyState>) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(proxy_inference))
        .route("/v1/completions", post(proxy_inference))
        .route("/v1/embeddings", post(proxy_inference))
        .route("/v1/models", get(proxy_passthrough))
        .route("/v1/aci/*rest", get(proxy_passthrough))
        .with_state(state)
}

/// Read-only passthrough (GET /v1/models, GET /v1/aci/*). Buffered; no receipt.
async fn proxy_passthrough(
    axum::extract::State(state): axum::extract::State<Arc<ProxyState>>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let path = uri.path().to_string();
    let url = join_url(&state.base_url, &uri);
    let req = forward_headers(state.client.request(reqwest::Method::GET, &url), &headers);
    let resp = match req.send().await {
        Ok(resp) => resp,
        Err(e) => return send_error(&state, "GET", path, e),
    };
    let status = resp.status().as_u16();
    let resp_headers = resp.headers().clone();
    let body = match resp.bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return send_error(&state, "GET", path, e),
    };
    (state.reporter)(RequestOutcome {
        method: "GET",
        path,
        status,
        streamed: false,
        verified: None,
        detail: String::new(),
    });
    let mut builder = Response::builder().status(status);
    for name in COPY_RESPONSE_HEADERS {
        if let Some(value) = resp_headers.get(*name) {
            builder = builder.header(*name, value);
        }
    }
    builder
        .body(Body::from(body))
        .unwrap_or_else(|_| internal_error())
}

/// Inference forward (chat/completions/embeddings): streamed byte-exact, wire
/// bytes teed for the after-the-fact receipt check.
async fn proxy_inference(
    axum::extract::State(state): axum::extract::State<Arc<ProxyState>>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();

    if let Err(reason) = state.ensure_unblocked().await {
        eprintln!("!! POST {path} -> 503 blocked: {reason}");
        return text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "upstream keyset changed and re-verification failed; refusing to forward\n",
        );
    }

    let trusted = state.snapshot();
    let url = join_url(&state.base_url, &uri);
    let bearer = bearer_token(&headers);
    let request_body = body.to_vec();
    let req = forward_headers(state.client.request(reqwest::Method::POST, &url), &headers)
        .body(request_body.clone());
    let resp = match req.send().await {
        Ok(resp) => resp,
        Err(e) => return send_error(&state, "POST", path, e),
    };
    let status = resp.status().as_u16();
    let resp_headers = resp.headers().clone();

    // Keyset-rotation gate: a response advertising a different digest blocks
    // further inference forwards until a fresh verify re-establishes trust.
    if let Some(observed) = header_str(&resp_headers, "x-aci-keyset-digest") {
        if observed != trusted.keyset_digest {
            state.blocked.store(true, Ordering::SeqCst);
            eprintln!(
                "!! upstream X-ACI-Keyset-Digest changed ({observed} != {}); blocking further \
                 inference forwards until re-verify",
                trusted.keyset_digest
            );
        }
    }

    let receipt_id = header_str(&resp_headers, "x-receipt-id").map(str::to_string);
    let streamed = header_str(&resp_headers, "content-type")
        .is_some_and(|ct| ct.contains("text/event-stream"));

    let mut builder = Response::builder().status(status);
    for name in COPY_RESPONSE_HEADERS {
        if let Some(value) = resp_headers.get(*name) {
            builder = builder.header(*name, value);
        }
    }

    // On clean completion, verify the receipt out of band against the exact
    // wire bytes just streamed.
    let hook_state = state.clone();
    let hook_path = path.clone();
    let hook: CompletionHook = Box::new(move |wire| {
        tokio::spawn(async move {
            let outcome = verify_inference(
                &hook_state,
                trusted.report,
                hook_path,
                status,
                streamed,
                receipt_id,
                bearer,
                request_body,
                wire,
            )
            .await;
            (hook_state.reporter)(outcome);
        });
    });

    let stream = TeeStream::new(resp.bytes_stream().boxed(), hook);
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| internal_error())
}

/// Fetch and check the receipt (§10.2) plus the upstream shallow/deep audit
/// (§10.3) for one completed inference response.
#[allow(clippy::too_many_arguments)]
async fn verify_inference(
    state: &ProxyState,
    report: Arc<AttestationReport>,
    path: String,
    status: u16,
    streamed: bool,
    receipt_id: Option<String>,
    bearer: Option<String>,
    request_body: Vec<u8>,
    wire: Vec<u8>,
) -> RequestOutcome {
    let outcome = |verified: Option<bool>, detail: String| RequestOutcome {
        method: "POST",
        path: path.clone(),
        status,
        streamed,
        verified,
        detail,
    };

    let Some(receipt_id) = receipt_id else {
        // A 2xx completion with no receipt header cannot be audited: treat it as
        // a failure so it prints loudly. Non-2xx upstream responses (auth
        // rejections, errors) legitimately carry no receipt — stay quiet there.
        if (200..300).contains(&status) {
            return outcome(
                Some(false),
                "no X-Receipt-Id on a 2xx inference response; receipt audit cannot run".to_string(),
            );
        }
        return outcome(
            None,
            "no X-Receipt-Id returned; nothing to verify".to_string(),
        );
    };
    let receipt_resp = match state
        .client
        .fetch_receipt(&state.base_url, &receipt_id, bearer.as_deref())
        .await
    {
        Ok(resp) if (200..300).contains(&resp.status) => resp,
        Ok(resp) => {
            return outcome(
                Some(false),
                format!("receipt {receipt_id}: fetch returned HTTP {}", resp.status),
            )
        }
        Err(e) => {
            return outcome(
                Some(false),
                format!("receipt {receipt_id}: fetch failed: {e}"),
            )
        }
    };
    let receipt = match receipt_resp.json().and_then(parse_receipt_envelope) {
        Ok(receipt) => receipt,
        Err(e) => return outcome(Some(false), format!("receipt {receipt_id}: {e}")),
    };
    let identity = match established_identity(&report) {
        Ok(identity) => identity,
        Err(e) => return outcome(Some(false), format!("receipt {receipt_id}: {e}")),
    };

    let mut transcript = Transcript::default();
    run_receipt_checks(
        &mut transcript,
        ReceiptContext::new(&receipt, &identity, Some(&request_body), Some(&wire)),
    );

    let (session_resp, no_session_reason) =
        fetch_live_session(&state.client, &state.base_url, &receipt.payload).await;
    let session_bytes = session_resp.map(|resp| resp.body);
    run_upstream_checks(
        &mut transcript,
        &receipt.payload,
        session_bytes.as_deref(),
        &no_session_reason,
        false,
    );

    // The claims live in the session document (§9.3), not the receipt.
    let session = session_bytes.and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let detail = format!(
        "receipt {receipt_id}: {}",
        summarize(&transcript, session.as_ref())
    );
    outcome(Some(transcript.verified()), detail)
}

/// One-line receipt summary: signature, wire hash, and the asserted upstream
/// claims (e.g. `signature ok, wire hash ok, upstream tee_attested asserted (hardware_proven)`).
fn summarize(transcript: &Transcript, session: Option<&Value>) -> String {
    let mut parts = vec![
        check_clause(transcript, "R.1", "signature"),
        check_clause(transcript, "R.4", "wire hash"),
        upstream_clause(transcript, session),
    ];
    parts.retain(|part| !part.is_empty());
    parts.join(", ")
}

fn check_clause(transcript: &Transcript, id: &str, label: &str) -> String {
    match status_of(transcript, id) {
        Some(Status::Pass) => format!("{label} ok"),
        Some(Status::Fail) => format!("{label} FAILED"),
        Some(Status::Skip) => format!("{label} skipped"),
        _ => String::new(),
    }
}

/// `upstream <name status (source)>...` over the asserted claims of the cited
/// session (§9.3), or a loud clause if the shallow audit (U.1) did not pass.
fn upstream_clause(transcript: &Transcript, session: Option<&Value>) -> String {
    if status_of(transcript, "U.1") != Some(Status::Pass) {
        return "upstream UNVERIFIED".to_string();
    }
    let claims = session
        .and_then(|record| record.get("claims"))
        .and_then(Value::as_object);
    let asserted: Vec<String> = claims
        .into_iter()
        .flatten()
        .filter(|(name, _)| name.as_str() != "extra")
        .filter_map(|(name, claim)| {
            let status = claim.get("status").and_then(Value::as_str)?;
            if status == "unknown" {
                return None;
            }
            match claim.get("source").and_then(Value::as_str) {
                Some(source) => Some(format!("{name} {status} ({source})")),
                None => Some(format!("{name} {status}")),
            }
        })
        .collect();
    if asserted.is_empty() {
        "upstream verified".to_string()
    } else {
        format!("upstream {}", asserted.join(", "))
    }
}

fn status_of(transcript: &Transcript, id: &str) -> Option<Status> {
    transcript
        .checks
        .iter()
        .find(|c| c.def.id == id)
        .map(|c| c.status)
}

/// Default console reporter: one line per request; loud on verification
/// failure; keep serving either way.
fn default_reporter(outcome: RequestOutcome) {
    let tag = if outcome.streamed { " (streamed)" } else { "" };
    let mut line = format!(
        "{} {} -> {}{tag}",
        outcome.method, outcome.path, outcome.status
    );
    if !outcome.detail.is_empty() {
        line.push_str(" — ");
        line.push_str(&outcome.detail);
    }
    if outcome.verified == Some(false) {
        eprintln!("!! {line}");
    } else {
        println!("{line}");
    }
}

fn join_url(base_url: &str, uri: &Uri) -> String {
    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or_else(|| uri.path());
    format!("{base_url}{path_and_query}")
}

fn forward_headers(
    mut req: reqwest::RequestBuilder,
    headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    for name in FORWARD_REQUEST_HEADERS {
        if let Some(value) = headers.get(*name) {
            if let Ok(value) = value.to_str() {
                req = req.header(*name, value);
            }
        }
    }
    req
}

/// The bearer token (credential only, `Bearer ` prefix stripped) for the
/// out-of-band receipt fetch; the forwarded request keeps the header verbatim.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let raw = header_str(headers, "authorization")?;
    let token = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .unwrap_or(raw);
    Some(token.to_string())
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// A send failure (including a fail-closed TLS pin mismatch) is a loud event.
fn send_error(
    state: &ProxyState,
    method: &'static str,
    path: String,
    err: reqwest::Error,
) -> Response {
    (state.reporter)(RequestOutcome {
        method,
        path,
        status: 502,
        streamed: false,
        verified: Some(false),
        detail: format!("upstream connection failed (possible TLS pin mismatch): {err}"),
    });
    text_response(StatusCode::BAD_GATEWAY, "upstream connection failed\n")
}

fn text_response(status: StatusCode, body: &'static str) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(Body::from(body))
        .unwrap_or_else(|_| internal_error())
}

fn internal_error() -> Response {
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(Body::empty())
        .expect("static internal-error response is valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::host_of;
    use crate::spec_fixtures::{
        vector_receipt_envelope, vector_report, vector_session_bytes, REQUEST_BODY, RESPONSE_BODY,
    };
    use axum::routing::{get, post};
    use axum::Json;
    use serde_json::json;
    use tokio::sync::mpsc;

    /// The one-line summary over the self-consistent fixtures, without any
    /// network: signature, wire hash, and the asserted upstream claim.
    #[test]
    fn summary_over_fixtures_reads_all_ok() {
        let report = vector_report();
        let identity = established_identity(&report).unwrap();
        let receipt = parse_receipt_envelope(vector_receipt_envelope()).unwrap();
        let session_bytes = vector_session_bytes();
        let mut transcript = Transcript::default();
        run_receipt_checks(
            &mut transcript,
            ReceiptContext::new(&receipt, &identity, Some(REQUEST_BODY), Some(RESPONSE_BODY)),
        );
        run_upstream_checks(
            &mut transcript,
            &receipt.payload,
            Some(&session_bytes),
            "unused",
            false,
        );

        assert!(transcript.verified());
        let session: Value = serde_json::from_slice(&session_bytes).unwrap();
        let summary = summarize(&transcript, Some(&session));
        assert_eq!(
            summary,
            "signature ok, wire hash ok, upstream tee_attested asserted (hardware_proven)"
        );
    }

    fn state_over(base_url: String, tx: mpsc::UnboundedSender<RequestOutcome>) -> Arc<ProxyState> {
        let host = host_of(&base_url).unwrap();
        Arc::new(ProxyState::new(
            AciClient::new().unwrap(),
            base_url,
            host,
            vector_report(),
            Arc::new(move |outcome| {
                let _ = tx.send(outcome);
            }),
        ))
    }

    async fn spawn_server(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// Hermetic end-to-end: a mock upstream serving fixture artifacts, the
    /// proxy in front of it. Asserts byte-exact passthrough, GET routing, and
    /// that the after-the-fact receipt check runs and passes.
    #[tokio::test]
    async fn proxy_forwards_and_verifies_receipt() {
        let keyset_digest = vector_report().workload_keyset_digest;
        let upstream = Router::new()
            .route(
                "/v1/chat/completions",
                post(move || async move {
                    (
                        [
                            ("content-type", "application/json".to_string()),
                            ("x-receipt-id", "rcpt-0001".to_string()),
                            ("x-aci-keyset-digest", keyset_digest),
                        ],
                        RESPONSE_BODY,
                    )
                }),
            )
            .route(
                "/v1/aci/receipts/:id",
                get(|| async { Json(vector_receipt_envelope()) }),
            )
            .route(
                // Sessions are served as their exact sealed bytes (§9).
                "/v1/aci/sessions/:id",
                get(|| async {
                    (
                        [("content-type", "application/json")],
                        vector_session_bytes(),
                    )
                }),
            )
            .route(
                "/v1/models",
                get(|| async { Json(json!({ "data": [{ "id": "demo-model" }] })) }),
            );
        let base = spawn_server(upstream).await;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let state = state_over(base.clone(), tx);
        let proxy = spawn_server(build_proxy_router(state)).await;

        let http = reqwest::Client::new();

        // Inference forward: byte-exact passthrough + receipt header surfaced.
        let resp = http
            .post(format!("{proxy}/v1/chat/completions"))
            .header("content-type", "application/json")
            .body(REQUEST_BODY.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(
            resp.headers()
                .get("x-receipt-id")
                .unwrap()
                .to_str()
                .unwrap(),
            "rcpt-0001"
        );
        assert_eq!(resp.bytes().await.unwrap().as_ref(), RESPONSE_BODY);

        let outcome = rx.recv().await.expect("inference outcome reported");
        assert_eq!(outcome.method, "POST");
        assert_eq!(outcome.path, "/v1/chat/completions");
        assert_eq!(outcome.verified, Some(true));
        assert!(
            outcome.detail.contains("signature ok"),
            "{}",
            outcome.detail
        );
        assert!(
            outcome.detail.contains("wire hash ok"),
            "{}",
            outcome.detail
        );
        assert!(
            outcome
                .detail
                .contains("tee_attested asserted (hardware_proven)"),
            "{}",
            outcome.detail
        );

        // GET passthrough routes and reports without a receipt check.
        let models = http.get(format!("{proxy}/v1/models")).send().await.unwrap();
        assert_eq!(models.status().as_u16(), 200);
        let models_outcome = rx.recv().await.expect("models outcome reported");
        assert_eq!(models_outcome.method, "GET");
        assert_eq!(models_outcome.verified, None);
    }
}
