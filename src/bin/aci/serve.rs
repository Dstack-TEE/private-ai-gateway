//! `aci serve`: a local verifying proxy that fails closed on the attested
//! service.
//!
//! Startup verifies `<base-url>` (spec 9.1) and refuses to listen unless
//! the verdict is VERIFIED. The proxy exposes a plaintext local API, rejects
//! E2EE request headers, and forwards accepted traffic over the SPKI-pinned
//! channel. Each POST response streams through byte-exact while its receipt id
//! and body digests are recorded — bodies are never stored — and a local control
//! endpoint fetches and verifies any recorded receipt on demand (spec 9.3, 9.2).
//! No bodies are logged.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::capture::{tee, CompletionHook, StreamEnd};
use axum::body::{Body, Bytes};
use axum::extract::Path;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use axum::{Json, Router};
use private_ai_gateway::aci::types::{
    AttestationReport, PROVIDER_ACI_SESSION_IDS, PROVIDER_ACI_VERIFIED,
};
use serde_json::{json, Value};

use crate::args::ServeArgs;
use crate::checks::{
    fetch_live_session, parse_receipt_document, run_response_checks, BodyDigest,
    EstablishedIdentity, RequiredClaim, UpstreamContext,
};
use crate::client::AciClient;
use crate::sessions::audit_current_sessions;
use crate::transcript::{Status, Transcript};
use crate::verify::{verify_service, ServiceVerification};

/// Connection-scoped headers a proxy re-derives per hop and never relays
/// (RFC 9110 §7.6.1). Everything else passes through in both directions after
/// E2EE input is rejected. Dropping a service header would hide protocol
/// members this proxy does not know about (Appendix B).
const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
];

/// Headers that select either E2EE v2 or the legacy encrypted transport.
/// `aci serve` exposes a plaintext local API, so even a partial encrypted
/// request is rejected instead of being forwarded or silently downgraded.
const E2EE_REQUEST_HEADERS: &[&str] = &[
    "x-signing-algo",
    "x-client-pub-key",
    "x-model-pub-key",
    "x-e2ee-version",
    "x-e2ee-nonce",
    "x-e2ee-timestamp",
];

fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP_HEADERS.contains(&name.to_ascii_lowercase().as_str())
}

fn has_e2ee_request_headers(headers: &HeaderMap) -> bool {
    E2EE_REQUEST_HEADERS
        .iter()
        .any(|name| headers.contains_key(*name))
}

/// What happened to one forwarded request; the reporter turns it into a line.
pub struct RequestOutcome {
    pub method: Method,
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
    /// The identity id-2 established on the verify that pinned this report.
    identity: Arc<EstablishedIdentity>,
    /// §3.4: forwarding on an expired keyset must stop, so expiry blocks
    /// like a rotation and a fresh verify re-establishes trust.
    not_after: u64,
}

/// One forwarded POST exchange, recorded as digests so the control endpoint
/// can verify its receipt on demand. Bodies are never stored.
#[derive(Clone)]
pub struct RecordedExchange {
    pub receipt_id: String,
    pub path: String,
    pub status: u16,
    pub streamed: bool,
    /// Digest of the plaintext request bytes this proxy forwarded.
    pub request: BodyDigest,
    /// Digest of the response wire bytes as forwarded — the full body, or
    /// everything before the truncation recorded alongside.
    pub response: BodyDigest,
    /// The upstream stream error, when the response was truncated. The
    /// recorded partial digest then honestly fails receipt-4 (§9.3(4)).
    pub truncation: Option<String>,
    /// The client's §5.3 pinned session ids from the request body.
    pub pinned_sessions: Vec<String>,
    pub at: u64,
    /// Verdict of the last on-demand verification, when one ran.
    pub verified: Option<bool>,
}

/// Recorded exchanges kept for on-demand verification (a bounded ring;
/// oldest evicted first).
const RECORDED_CAP: usize = 256;

pub struct ProxyState {
    client: AciClient,
    base_url: String,
    host: String,
    /// Demand verified attested-session serving (`provider.aci_verified`,
    /// §5.3) on every inference forward. On by default.
    enforce_verified: bool,
    /// Compose hashes this operator accepts (§1.3), applied on the startup
    /// verify and on every keyset-change re-verify.
    accepted_composes: Vec<String>,
    /// Apply the production dstack OS-image policy on startup and re-verification.
    require_production_os: bool,
    trusted: Mutex<TrustedIdentity>,
    /// Set when an upstream response advertised a keyset digest other than the
    /// trusted one; blocks inference forwards until a fresh verify passes.
    blocked: AtomicBool,
    /// Serializes the re-verify so a burst of blocked requests reverifies once.
    reverify: tokio::sync::Mutex<()>,
    recorded: Mutex<VecDeque<RecordedExchange>>,
    /// `--session`: a fixed §5.3 accepted set composed with every request's
    /// own pins. Never refreshed, so a 412 refusal surfaces as-is.
    fixed_pins: Vec<String>,
    /// `--require-claim`: the §9.2(3) policy that derives the pin set from
    /// the service's current attested sessions.
    required_claims: Vec<RequiredClaim>,
    /// The policy-derived pin set (empty when no policy). Refreshed when the
    /// service refuses a pinned forward with 412 `session_not_accepted`.
    policy_pins: Mutex<Vec<String>>,
    reporter: Reporter,
}

impl ProxyState {
    #[allow(clippy::too_many_arguments)]
    fn new(
        client: AciClient,
        base_url: String,
        host: String,
        enforce_verified: bool,
        accepted_composes: Vec<String>,
        require_production_os: bool,
        fixed_pins: Vec<String>,
        required_claims: Vec<RequiredClaim>,
        report: AttestationReport,
        identity: EstablishedIdentity,
        reporter: Reporter,
    ) -> Self {
        let keyset_digest = report.workload_keyset_digest.clone();
        Self {
            client,
            base_url,
            host,
            enforce_verified,
            accepted_composes,
            require_production_os,
            trusted: Mutex::new(TrustedIdentity {
                report: Arc::new(report),
                keyset_digest,
                not_after: identity.keyset.not_after,
                identity: Arc::new(identity),
            }),
            blocked: AtomicBool::new(false),
            reverify: tokio::sync::Mutex::new(()),
            recorded: Mutex::new(VecDeque::new()),
            fixed_pins,
            required_claims,
            policy_pins: Mutex::new(Vec::new()),
            reporter,
        }
    }

    /// The active accepted set: the user's fixed list, or the current
    /// policy-derived one.
    fn active_pins(&self) -> Vec<String> {
        if !self.fixed_pins.is_empty() {
            return self.fixed_pins.clone();
        }
        self.policy_pins
            .lock()
            .expect("policy pins poisoned")
            .clone()
    }

    fn record(&self, exchange: RecordedExchange) {
        let mut recorded = self.recorded.lock().expect("recorded ring poisoned");
        if recorded.len() == RECORDED_CAP {
            recorded.pop_front();
        }
        recorded.push_back(exchange);
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
        let verification = verify_service(
            &self.base_url,
            None,
            &self.accepted_composes,
            self.require_production_os,
            false,
        )
        .await?;
        if !verification.transcript.verified() {
            return Err("service re-verification did not reach VERIFIED".to_string());
        }
        if let Some(spki) = &verification.observed_spki {
            self.client.pin(&self.host, spki);
        }
        let keyset_digest = verification.report.workload_keyset_digest.clone();
        let identity = verification
            .identity
            .ok_or("verified run carried no established identity")?;
        *self.trusted.lock().expect("trusted identity poisoned") = TrustedIdentity {
            report: Arc::new(verification.report),
            keyset_digest,
            not_after: identity.keyset.not_after,
            identity: Arc::new(identity),
        };
        self.blocked.store(false, Ordering::SeqCst);
        eprintln!("aci serve: re-verified after keyset change; resuming forwards");
        Ok(())
    }
}

pub async fn run(args: ServeArgs, require_production_os: bool) -> Result<i32, String> {
    let verification = verify_service(
        &args.base_url,
        None,
        &args.accepted_composes,
        require_production_os,
        false,
    )
    .await?;
    println!("== service verification: {} ==", verification.base_url);
    print!("{}", verification.transcript.render_human(false));
    if !verification.transcript.verified() {
        return Err(
            "service verification failed; refusing to start the proxy (fail closed)".to_string(),
        );
    }

    let ServiceVerification {
        report,
        identity,
        client,
        base_url,
        host,
        observed_spki,
        ..
    } = verification;
    let identity = identity.ok_or("verified run carried no established identity")?;
    // Pin the just-verified TLS key on every future hop to this host.
    if let Some(spki) = &observed_spki {
        client.pin(&host, spki);
    }
    let state = Arc::new(ProxyState::new(
        client,
        base_url.clone(),
        host,
        !args.allow_unverified,
        args.accepted_composes.clone(),
        require_production_os,
        args.sessions.clone(),
        args.require_claims.clone(),
        report,
        identity,
        Arc::new(default_reporter),
    ));
    // §5.3 prevention: a claims policy derives the pin set from the current
    // attested sessions before any traffic — and nothing acceptable to pin
    // means refusing to start, not serving unpinned.
    if !state.required_claims.is_empty() {
        let pins = derive_policy_pins(&state).await?;
        if pins.is_empty() {
            return Err(
                "no current attested session satisfies the --require-claim policy; \
                 refusing to start (fail closed)"
                    .to_string(),
            );
        }
        println!("policy-accepted sessions pinned ({}):", pins.len());
        for pin in &pins {
            println!("  {pin}");
        }
        *state.policy_pins.lock().expect("policy pins poisoned") = pins;
    }

    let listen = args.listen.as_deref().unwrap_or("127.0.0.1:4180");
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| format!("cannot bind {listen}: {e}"))?;
    let local = listener
        .local_addr()
        .map_err(|e| format!("cannot read listen address: {e}"))?;
    let control = args.control.as_deref().unwrap_or("127.0.0.1:4181");
    let control_listener = tokio::net::TcpListener::bind(control)
        .await
        .map_err(|e| format!("cannot bind control address {control}: {e}"))?;
    let control_local = control_listener
        .local_addr()
        .map_err(|e| format!("cannot read control address: {e}"))?;

    println!();
    println!("aci serve: proxying {base_url} on http://{local} (plain HTTP, localhost)");
    println!(
        "forwarding every method and path; Authorization passed through unchanged; every \
         upstream hop pinned to the attested TLS key; each POST response's receipt id and \
         body digests recorded.\n\
         verify on demand: GET http://{control_local}/receipts lists recent exchanges, \
         POST http://{control_local}/receipts/<id>/verify checks one (send Authorization \
         if the receipt fetch needs it).\n{}",
        if args.allow_unverified {
            "verified serving NOT demanded (--allow-unverified)."
        } else {
            "every inference demands verified serving \
             (provider.aci_verified, spec 5.3)."
        }
    );
    println!();

    let control_server = axum::serve(control_listener, build_control_router(state.clone()));
    tokio::spawn(async move {
        if let Err(e) = control_server.await {
            eprintln!("aci serve: control server error: {e}");
        }
    });
    axum::serve(listener, build_proxy_router(state))
        .await
        .map_err(|e| format!("proxy server error: {e}"))?;
    Ok(0)
}

fn build_control_router(state: Arc<ProxyState>) -> Router {
    Router::new()
        .route("/receipts", axum::routing::get(control_list))
        .route("/receipts/:id/verify", axum::routing::post(control_verify))
        .with_state(state)
}

fn build_proxy_router(state: Arc<ProxyState>) -> Router {
    // No route list: every method and path forwards to the same path on the
    // service, so protocol surfaces this proxy does not know about
    // (Appendix B) keep working. POST is the inference surface (§5.1) and
    // gets the receipt check; everything else is read-only passthrough.
    Router::new().fallback(proxy).with_state(state)
}

async fn proxy(
    axum::extract::State(state): axum::extract::State<Arc<ProxyState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if method == Method::POST {
        proxy_inference(state, uri, headers, body).await
    } else {
        proxy_passthrough(state, method, uri, headers, body).await
    }
}

/// Non-POST passthrough: streamed byte-exact, no receipt to check.
async fn proxy_passthrough(
    state: Arc<ProxyState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let url = join_url(&state.base_url, &uri);
    let mut req = forward_headers(state.client.request(method.clone(), &url), &headers);
    if !body.is_empty() {
        req = req.body(body.to_vec());
    }
    let resp = match req.send().await {
        Ok(resp) => resp,
        Err(e) => return send_error(&state, method, path, e),
    };
    let status = resp.status().as_u16();
    let resp_headers = resp.headers().clone();
    rotation_gate(&state, &state.snapshot().keyset_digest, &resp_headers);
    (state.reporter)(RequestOutcome {
        method,
        path,
        status,
        streamed: false,
        verified: None,
        detail: String::new(),
    });
    let mut builder = Response::builder().status(status);
    for (name, value) in resp_headers.iter() {
        if !is_hop_by_hop(name.as_str()) {
            builder = builder.header(name, value);
        }
    }
    builder
        .body(Body::from_stream(resp.bytes_stream()))
        .unwrap_or_else(|_| internal_error())
}

/// Inference forward (any POST): streamed byte-exact, wire bytes teed for the
/// after-the-fact receipt check.
async fn proxy_inference(
    state: Arc<ProxyState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();

    if has_e2ee_request_headers(&headers) {
        eprintln!("!! POST {path} -> 400 E2EE request rejected by plaintext local API");
        return text_response(
            StatusCode::BAD_REQUEST,
            "aci serve accepts plaintext requests only; remove E2EE request headers\n",
        );
    }

    // §3.4: a long-running proxy must not keep forwarding on an expired
    // keyset — expiry blocks exactly like a rotation.
    if crate::checks::now_secs() >= state.snapshot().not_after {
        state.blocked.store(true, Ordering::SeqCst);
    }
    if let Err(reason) = state.ensure_unblocked().await {
        eprintln!("!! POST {path} -> 503 blocked: {reason}");
        return text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "upstream keyset changed or expired and re-verification failed; refusing to forward\n",
        );
    }

    let trusted = state.snapshot();
    let url = join_url(&state.base_url, &uri);
    let active_pins = state.active_pins();
    // A policy-derived set is refreshed only when it actually constrained this
    // request. Requests without a local policy keep their own pins unchanged.
    let injected_policy_pins = !state.required_claims.is_empty()
        && !active_pins.is_empty()
        && pinned_session_ids(&body).is_empty();
    let mut request_body =
        match apply_constraints(body.to_vec(), state.enforce_verified, &active_pins) {
            Ok(body) => body,
            Err(reason) => {
                eprintln!("!! POST {path} -> 400: {reason}");
                return text_response(
                    StatusCode::BAD_REQUEST,
                    "request session ids are not accepted by the local ACI policy\n",
                );
            }
        };
    let send = |body: Vec<u8>| {
        forward_headers(state.client.request(Method::POST, &url), &headers)
            .body(body)
            .send()
    };
    let mut resp = match send(request_body.clone()).await {
        Ok(resp) => resp,
        Err(e) => return send_error(&state, Method::POST, path, e),
    };
    // A 412 refusal against a policy-derived pin set means the sessions
    // rotated under us (§8 supersession): refresh the set from the service's
    // current sessions and retry once. §5.3 refuses before serving, so
    // nothing ran on the refused attempt. A user-fixed --session list is
    // never refreshed; its 412 surfaces as-is.
    if resp.status().as_u16() == 412 && injected_policy_pins {
        match derive_policy_pins(&state).await {
            Ok(pins) if !pins.is_empty() && pins != active_pins => {
                eprintln!(
                    "aci serve: pinned sessions refused (412); policy re-accepted {} current \
                     session(s), retrying",
                    pins.len()
                );
                *state.policy_pins.lock().expect("policy pins poisoned") = pins.clone();
                request_body = match apply_constraints(body.to_vec(), state.enforce_verified, &pins)
                {
                    Ok(body) => body,
                    Err(reason) => {
                        eprintln!("aci serve: refreshed session policy rejected request: {reason}");
                        return text_response(
                            StatusCode::BAD_REQUEST,
                            "request session ids are not accepted by the refreshed ACI policy\n",
                        );
                    }
                };
                match send(request_body.clone()).await {
                    Ok(retried) => resp = retried,
                    Err(e) => return send_error(&state, Method::POST, path, e),
                }
            }
            Ok(_) => {}
            Err(e) => eprintln!("aci serve: policy pin refresh after 412 failed: {e}"),
        }
    }
    let status = resp.status().as_u16();
    let resp_headers = resp.headers().clone();
    rotation_gate(&state, &trusted.keyset_digest, &resp_headers);

    let receipt_id = header_str(&resp_headers, "x-receipt-id").map(str::to_string);
    let streamed = header_str(&resp_headers, "content-type")
        .is_some_and(|ct| ct.contains("text/event-stream"));

    let mut builder = Response::builder().status(status);
    for (name, value) in resp_headers.iter() {
        if !is_hop_by_hop(name.as_str()) {
            builder = builder.header(name, value);
        }
    }

    // On stream end, record the exchange as digests for on-demand
    // verification via the control endpoint. Nothing is fetched per request.
    let hook_state = state.clone();
    let hook_path = path.clone();
    let request_digest = BodyDigest::of(&request_body);
    // §9.3(6): the pinned ids ride along so the on-demand check enforces the
    // same membership rule as `aci send --session`.
    let pinned_sessions = pinned_session_ids(&request_body);
    let hook: CompletionHook = Box::new(move |end| {
        let (response, truncation) = match end {
            StreamEnd::Complete(digest) => (digest, None),
            // ACI §9.3(4) uses the wire hash to catch truncation, so it must
            // reach the report rather than vanish.
            StreamEnd::Errored { partial, error } => (partial, Some(error)),
        };
        let outcome = |verified: Option<bool>, detail: String| RequestOutcome {
            method: Method::POST,
            path: hook_path.clone(),
            status,
            streamed,
            verified,
            detail,
        };
        let outcome = match (&receipt_id, &truncation) {
            (Some(id), None) => outcome(None, format!("receipt {id} recorded; verify on demand")),
            (Some(id), Some(error)) => outcome(
                Some(false),
                format!(
                    "upstream stream errored after {} bytes: {error}; receipt {id} recorded — \
                     verification will surface the truncation",
                    response.len
                ),
            ),
            // A 2xx POST completion with no receipt header can never be
            // audited: fail loudly (spec 5.2 puts a receipt on every
            // inference response). Non-2xx responses legitimately carry none.
            (None, _) if (200..300).contains(&status) => outcome(
                Some(false),
                "no X-Receipt-Id on a 2xx POST response (spec 5.2); nothing recorded".to_string(),
            ),
            (None, _) => outcome(
                None,
                "no X-Receipt-Id returned; nothing to verify".to_string(),
            ),
        };
        if let Some(receipt_id) = receipt_id {
            hook_state.record(RecordedExchange {
                receipt_id,
                path: hook_path,
                status,
                streamed,
                request: request_digest,
                response,
                truncation,
                pinned_sessions,
                at: crate::checks::now_secs(),
                verified: None,
            });
        }
        (hook_state.reporter)(outcome);
    });

    let stream = tee(resp.bytes_stream(), hook);
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| internal_error())
}

/// GET /receipts on the control endpoint: recorded exchanges, newest first.
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
                    "truncated": exchange.truncation.is_some(),
                    "at": exchange.at,
                    "verified": exchange.verified,
                })
            })
            .collect(),
    ))
}

/// POST /receipts/{id}/verify on the control endpoint: fetch the receipt and
/// its cited session from the service and run the §9.3/§9.2 checks against
/// the recorded digests. The request's own Authorization header (if any) is
/// used for the receipt fetch.
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
    let report = |verified: Option<bool>, detail: String| {
        (state.reporter)(RequestOutcome {
            method: Method::POST,
            path: exchange.path.clone(),
            status: exchange.status,
            streamed: exchange.streamed,
            verified,
            detail,
        });
    };
    match verify_exchange(&state, &trusted, &exchange, bearer.as_deref()).await {
        Ok((transcript, detail)) => {
            let verified = transcript.verified();
            {
                let mut recorded = state.recorded.lock().expect("recorded ring poisoned");
                if let Some(entry) = recorded
                    .iter_mut()
                    .rev()
                    .find(|entry| entry.receipt_id == id)
                {
                    entry.verified = Some(verified);
                }
            }
            report(Some(verified), detail);
            let mut body = transcript.to_json(false);
            body["receipt_id"] = json!(id);
            json_response(StatusCode::OK, body)
        }
        Err(e) => {
            let detail = format!("receipt {id}: {e}");
            report(Some(false), detail.clone());
            json_response(StatusCode::BAD_GATEWAY, json!({ "error": detail }))
        }
    }
}

/// Fetch and check the receipt (§9.3) plus the cited-session audit (§9.2)
/// for one recorded exchange, against its recorded digests.
async fn verify_exchange(
    state: &ProxyState,
    trusted: &TrustedIdentity,
    exchange: &RecordedExchange,
    bearer: Option<&str>,
) -> Result<(Transcript, String), String> {
    let receipt_resp = state
        .client
        .fetch_receipt(&state.base_url, &exchange.receipt_id, bearer)
        .await
        .map_err(|e| format!("fetch failed: {e}"))?;
    if !(200..300).contains(&receipt_resp.status) {
        return Err(format!("fetch returned HTTP {}", receipt_resp.status));
    }
    let receipt = receipt_resp.json().and_then(parse_receipt_document)?;

    let mut transcript = Transcript::default();
    let (session_resp, no_session_reason) =
        fetch_live_session(&state.client, &state.base_url, &receipt).await;
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
    if let Some(error) = &exchange.truncation {
        detail.push_str(&format!(
            " (response truncated at {} bytes: {error})",
            exchange.response.len
        ));
    }
    Ok((transcript, detail))
}

fn json_response(status: StatusCode, body: Value) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| internal_error())
}

/// One-line receipt summary: signature, wire hash, and the asserted upstream
/// claims (e.g. `signature ok, wire hash ok, upstream tee_attested asserted (hardware_proven)`).
fn summarize(transcript: &Transcript, session: Option<&Value>, serving: &str) -> String {
    let mut parts = vec![
        check_clause(transcript, "receipt-1", "signature"),
        check_clause(transcript, "receipt-4", "wire hash"),
        upstream_clause(transcript, session, serving),
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
/// session (§8.3), or a loud clause if the shallow audit (upstream-1) did not pass.
fn upstream_clause(transcript: &Transcript, session: Option<&Value>, serving: &str) -> String {
    // §4.1/§5.3: a direct service has no upstream hop — "UNVERIFIED" would
    // misread the workload the client itself verified.
    if serving == "direct" {
        return "direct service, no upstream hop".to_string();
    }
    if status_of(transcript, "upstream-1") != Some(Status::Pass) {
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
            // Appendix B: an unrecognized status or source is treated as
            // `unknown`, so it is never presented as a claim of record.
            let status = match claim.get("status").and_then(Value::as_str)? {
                status @ ("asserted" | "refuted") => status,
                _ => return None,
            };
            match claim.get("source").and_then(Value::as_str) {
                Some(
                    source @ ("hardware_proven" | "verifier_derived" | "provider_asserted"
                    | "operator_asserted"),
                ) => Some(format!("{name} {status} ({source})")),
                Some(_) => None,
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

/// Keyset-rotation gate (§3.4): a response advertising a digest other than
/// the trusted one blocks further inference forwards until a fresh verify
/// re-establishes trust.
fn rotation_gate(state: &ProxyState, trusted_digest: &str, headers: &HeaderMap) {
    if let Some(observed) = header_str(headers, "x-aci-keyset-digest") {
        if observed != trusted_digest {
            state.blocked.store(true, Ordering::SeqCst);
            eprintln!(
                "!! upstream X-ACI-Keyset-Digest changed ({observed} != {trusted_digest}); \
                 blocking further inference forwards until re-verify"
            );
        }
    }
}

fn join_url(base_url: &str, uri: &Uri) -> String {
    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or_else(|| uri.path());
    format!("{base_url}{path_and_query}")
}

/// The `provider.aci_session_ids` a client pinned in its request body (§5.3),
/// for the client-side §9.3(6) membership check. Malformed bodies pin nothing;
/// the service rejects them itself.
fn pinned_session_ids(body: &[u8]) -> Vec<String> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|parsed| {
            let list = parsed
                .get("provider")?
                .get(PROVIDER_ACI_SESSION_IDS)?
                .as_array()?
                .clone();
            Some(
                list.iter()
                    .filter_map(|id| id.as_str().map(str::to_string))
                    .collect(),
            )
        })
        .unwrap_or_default()
}

/// Tighten an inference body (§5.3): demand verified serving and compose the
/// client's own pins with the local accepted set. A narrower client set is
/// preserved; disjoint policies fail locally instead of bypassing either one.
fn apply_constraints(
    body: Vec<u8>,
    enforce_verified: bool,
    pins: &[String],
) -> Result<Vec<u8>, String> {
    if !enforce_verified && pins.is_empty() {
        return Ok(body);
    }
    let Ok(mut parsed) = serde_json::from_slice::<Value>(&body) else {
        return Ok(body);
    };
    let Some(provider) = parsed
        .as_object_mut()
        .map(|members| members.entry("provider").or_insert_with(|| json!({})))
        .and_then(Value::as_object_mut)
    else {
        return Ok(body);
    };
    // Pinning implies verified serving (§5.3).
    provider.insert(PROVIDER_ACI_VERIFIED.to_string(), Value::Bool(true));
    if !pins.is_empty() {
        let supplied = provider
            .get(PROVIDER_ACI_SESSION_IDS)
            .and_then(Value::as_array)
            .filter(|ids| !ids.is_empty() && ids.iter().all(Value::is_string));
        let accepted: Vec<&String> = match supplied {
            Some(ids) => ids
                .iter()
                .filter_map(Value::as_str)
                .filter_map(|id| pins.iter().find(|pin| pin.as_str() == id))
                .collect(),
            None => pins.iter().collect(),
        };
        if supplied.is_some() && accepted.is_empty() {
            return Err("request pins and local accepted session set are disjoint".to_string());
        }
        provider.insert(PROVIDER_ACI_SESSION_IDS.to_string(), json!(accepted));
    }
    Ok(serde_json::to_vec(&parsed).unwrap_or(body))
}

/// Derive the §5.3 pin set from the service's current attested sessions:
/// list, run the spec 9.2 audit on each record, and keep the ids satisfying
/// the claims policy. Rejections are printed, never silently dropped.
async fn derive_policy_pins(state: &ProxyState) -> Result<Vec<String>, String> {
    let audited =
        audit_current_sessions(&state.client, &state.base_url, None, &state.required_claims)
            .await?;
    for rejected in audited.iter().filter(|session| !session.accepted()) {
        eprintln!(
            "aci serve: session {} rejected ({})",
            rejected.session_id,
            match &rejected.audit {
                Err(e) => e.clone(),
                Ok(_) if !rejected.integrity_ok() => "spec 9.2 integrity audit failed".to_string(),
                Ok(_) => format!("unmet claims: {}", rejected.unmet.join(", ")),
            }
        );
    }
    Ok(audited
        .iter()
        .filter(|session| session.accepted())
        .map(|session| session.session_id.clone())
        .collect())
}

fn forward_headers(
    mut req: reqwest::RequestBuilder,
    headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    for (name, value) in headers.iter() {
        if !is_hop_by_hop(name.as_str()) {
            req = req.header(name, value);
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
fn send_error(state: &ProxyState, method: Method, path: String, err: reqwest::Error) -> Response {
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
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::mpsc;

    /// The one-line summary over the self-consistent fixtures, without any
    /// network: signature, wire hash, and the asserted upstream claim.
    #[test]
    fn summary_over_fixtures_reads_all_ok() {
        let report = vector_report();
        let identity = crate::checks::established_identity(&report).unwrap();
        let receipt = parse_receipt_document(vector_receipt_envelope()).unwrap();
        let session_bytes = vector_session_bytes();
        let mut transcript = Transcript::default();
        run_response_checks(
            &mut transcript,
            &receipt,
            &identity,
            Some(&BodyDigest::of(REQUEST_BODY)),
            Some(&BodyDigest::of(RESPONSE_BODY)),
            UpstreamContext {
                session_bytes: Some(&session_bytes),
                no_session_reason: "unused",
                pinned: None,
                requires_verified: true,
                serving: "aggregator",
                required_claims: &[],
            },
        );

        assert!(transcript.verified());
        let session: Value = serde_json::from_slice(&session_bytes).unwrap();
        let summary = summarize(&transcript, Some(&session), "upstream");
        assert_eq!(
            summary,
            "signature ok, wire hash ok, upstream tee_attested asserted (hardware_proven)"
        );
    }

    fn state_over(base_url: String, tx: mpsc::UnboundedSender<RequestOutcome>) -> Arc<ProxyState> {
        let host = host_of(&base_url).unwrap();
        // Byte-exact passthrough harness: enforcement off so fixture-pinned
        // request hashes hold; `apply_constraints` has its own unit test.
        Arc::new(ProxyState::new(
            AciClient::new().unwrap(),
            base_url,
            host,
            false,
            Vec::new(),
            false,
            Vec::new(),
            Vec::new(),
            vector_report(),
            crate::checks::established_identity(&vector_report()).unwrap(),
            Arc::new(move |outcome| {
                let _ = tx.send(outcome);
            }),
        ))
    }

    #[test]
    fn apply_constraints_tightens_plaintext_body() {
        // Plain body: the member is added.
        let out = apply_constraints(br#"{"model":"m","messages":[]}"#.to_vec(), true, &[]).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["provider"]["aci_verified"], true);
        assert_eq!(v["provider"].get("aci_session_ids"), None);

        // Existing routing members survive; an explicit `false` is tightened.
        let out = apply_constraints(
            br#"{"model":"m","provider":{"order":["x"],"aci_verified":false}}"#.to_vec(),
            true,
            &[],
        )
        .unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["provider"]["aci_verified"], true);
        assert_eq!(v["provider"]["order"][0], "x");

        // A pin set is injected — and implies verified serving.
        let pins = vec!["a".repeat(64)];
        let out = apply_constraints(br#"{"model":"m"}"#.to_vec(), false, &pins).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["provider"]["aci_session_ids"][0], pins[0]);
        assert_eq!(v["provider"]["aci_verified"], true);

        // The client's own narrower set survives when local policy accepts it.
        let out = apply_constraints(
            format!(r#"{{"provider":{{"aci_session_ids":["{}"]}}}}"#, pins[0]).into_bytes(),
            true,
            &pins,
        )
        .unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["provider"]["aci_session_ids"], json!(pins));

        // Disjoint client and local policies fail before network access.
        let disjoint = json!({
            "provider": { "aci_session_ids": ["b".repeat(64)] }
        });
        assert!(apply_constraints(serde_json::to_vec(&disjoint).unwrap(), true, &pins,).is_err());

        // Non-JSON bodies pass through untouched.
        assert_eq!(
            apply_constraints(b"not json".to_vec(), true, &[]).unwrap(),
            b"not json"
        );
    }

    #[tokio::test]
    async fn proxy_rejects_e2ee_request_headers_without_contacting_upstream() {
        let upstream_calls = Arc::new(AtomicUsize::new(0));
        let counted_calls = upstream_calls.clone();
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let counted_calls = counted_calls.clone();
                async move {
                    counted_calls.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NO_CONTENT
                }
            }),
        );
        let base = spawn_server(upstream).await;

        let (tx, _rx) = mpsc::unbounded_channel();
        let proxy = spawn_server(build_proxy_router(state_over(base, tx))).await;
        let http = reqwest::Client::new();

        for header in E2EE_REQUEST_HEADERS {
            let resp = http
                .post(format!("{proxy}/v1/chat/completions"))
                .header(*header, "2")
                .header("content-type", "application/json")
                .body(REQUEST_BODY.to_vec())
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status().as_u16(), 400, "header {header}");
            assert!(
                resp.text()
                    .await
                    .unwrap()
                    .contains("accepts plaintext requests only"),
                "header {header}"
            );
        }

        assert_eq!(upstream_calls.load(Ordering::SeqCst), 0);
    }

    /// Policy-pinned proxy against rotated sessions: the stale pin is
    /// refused 412, the proxy re-derives the accepted set from the current
    /// sessions (spec 9.2 audit + claims policy) and retries once.
    #[tokio::test]
    async fn a_412_refusal_refreshes_policy_pins_and_retries() {
        use axum::response::IntoResponse;

        // A currently-valid session: the fixture record with its validity
        // window moved to now (the id is content-addressed, so it changes).
        let mut record: Value = serde_json::from_slice(&vector_session_bytes()).unwrap();
        let now = crate::checks::now_secs();
        record["established_at"] = json!(now - 10);
        record["expires_at"] = json!(now + 3600);
        let session_bytes = private_ai_gateway::aci::digest::jcs_bytes(&record).unwrap();
        let current_id = private_ai_gateway::aci::digest::sha256_bare_hex(&session_bytes);
        let keyset_digest = vector_report().workload_keyset_digest;

        let sid = current_id.clone();
        let upstream = Router::new()
            .route(
                "/v1/chat/completions",
                post(move |body: Bytes| {
                    let sid = sid.clone();
                    let keyset_digest = keyset_digest.clone();
                    async move {
                        let v: Value = serde_json::from_slice(&body).unwrap();
                        let pinned_current = v["provider"]["aci_session_ids"]
                            .as_array()
                            .is_some_and(|pins| pins.iter().any(|pin| pin == &json!(sid)));
                        if pinned_current {
                            (
                                StatusCode::OK,
                                [
                                    ("x-receipt-id", "rcpt-0002".to_string()),
                                    ("x-aci-keyset-digest", keyset_digest),
                                ],
                                RESPONSE_BODY,
                            )
                                .into_response()
                        } else {
                            (StatusCode::PRECONDITION_FAILED, "session_not_accepted")
                                .into_response()
                        }
                    }
                }),
            )
            .route(
                "/v1/aci/sessions",
                get({
                    let sid = current_id.clone();
                    move || {
                        let sid = sid.clone();
                        async move {
                            Json(json!({
                                "api_version": "aci/1",
                                "sessions": [{ "session_id": sid }],
                            }))
                        }
                    }
                }),
            )
            .route(
                "/v1/aci/sessions/:id",
                get({
                    let session_bytes = session_bytes.clone();
                    move || {
                        let session_bytes = session_bytes.clone();
                        async move { ([("content-type", "application/json")], session_bytes) }
                    }
                }),
            );
        let base = spawn_server(upstream).await;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let host = host_of(&base).unwrap();
        let state = Arc::new(ProxyState::new(
            AciClient::new().unwrap(),
            base.clone(),
            host,
            true,
            Vec::new(),
            false,
            Vec::new(),
            vec![crate::checks::RequiredClaim::parse("tee_attested").unwrap()],
            vector_report(),
            crate::checks::established_identity(&vector_report()).unwrap(),
            Arc::new(move |outcome| {
                let _ = tx.send(outcome);
            }),
        ));
        // A stale pin, as if the pinned session was superseded after startup.
        *state.policy_pins.lock().unwrap() = vec!["f".repeat(64)];
        let proxy = spawn_server(build_proxy_router(state.clone())).await;

        let resp = reqwest::Client::new()
            .post(format!("{proxy}/v1/chat/completions"))
            .header("content-type", "application/json")
            .body(br#"{"model":"m","messages":[]}"#.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(resp.bytes().await.unwrap().as_ref(), RESPONSE_BODY);

        let outcome = rx.recv().await.expect("retried outcome reported");
        assert_eq!(outcome.status, 200);
        assert!(outcome.detail.contains("recorded"), "{}", outcome.detail);
        // The refreshed set replaced the stale pin.
        assert_eq!(*state.policy_pins.lock().unwrap(), vec![current_id]);
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
    /// proxy in front of it. Asserts byte-exact passthrough, that any POST
    /// path gets its exchange recorded (an Anthropic-style `/v1/messages`
    /// included), that the control endpoint verifies a recorded receipt on
    /// demand, and that a receiptless 2xx POST fails loudly.
    #[tokio::test]
    async fn proxy_forwards_and_verifies_receipt() {
        let inference = || {
            let keyset_digest = vector_report().workload_keyset_digest;
            post(move || async move {
                (
                    [
                        ("content-type", "application/json".to_string()),
                        ("x-receipt-id", "rcpt-0001".to_string()),
                        ("x-aci-keyset-digest", keyset_digest),
                    ],
                    RESPONSE_BODY,
                )
            })
        };
        let upstream = Router::new()
            .route("/v1/chat/completions", inference())
            .route("/v1/messages", inference())
            .route(
                "/v1/responses",
                post(|| async { Json(json!({ "ok": true })) }),
            )
            .route(
                "/v1/aci/receipts/:id",
                get(|| async { Json(vector_receipt_envelope()) }),
            )
            .route(
                // Sessions are served as their exact sealed bytes (§8).
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
        let proxy = spawn_server(build_proxy_router(state.clone())).await;
        let control = spawn_server(build_control_router(state)).await;

        let http = reqwest::Client::new();

        // Inference forward: byte-exact passthrough + receipt header surfaced;
        // the exchange is recorded, nothing fetched per request.
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
        assert_eq!(outcome.verified, None);
        assert!(outcome.detail.contains("recorded"), "{}", outcome.detail);

        // Any POST path is inference-capable: an Anthropic-style /v1/messages
        // forward is recorded the same way without being enumerated.
        let resp = http
            .post(format!("{proxy}/v1/messages"))
            .header("content-type", "application/json")
            .body(REQUEST_BODY.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(resp.bytes().await.unwrap().as_ref(), RESPONSE_BODY);
        let outcome = rx.recv().await.expect("messages outcome reported");
        assert_eq!(outcome.path, "/v1/messages");
        assert_eq!(outcome.verified, None);

        // The control endpoint lists both recorded exchanges, newest first.
        let listed: Value = http
            .get(format!("{control}/receipts"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 2);
        assert_eq!(listed[0]["path"], "/v1/messages");
        assert_eq!(listed[0]["verified"], Value::Null);

        // On-demand verification runs the full receipt + session audit
        // against the recorded digests.
        let resp = http
            .post(format!("{control}/receipts/rcpt-0001/verify"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let verdict: Value = resp.json().await.unwrap();
        assert_eq!(verdict["verdict"]["verified"], Value::Bool(true));
        let outcome = rx.recv().await.expect("verification outcome reported");
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

        // The verdict is remembered on the recorded exchange.
        let listed: Value = http
            .get(format!("{control}/receipts"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed[0]["verified"], Value::Bool(true));

        // Verifying an unknown receipt id is a 404, not a quiet pass.
        let resp = http
            .post(format!("{control}/receipts/rcpt-unknown/verify"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 404);

        // A 2xx POST response with no receipt header fails loudly (spec 5.2).
        let resp = http
            .post(format!("{proxy}/v1/responses"))
            .header("content-type", "application/json")
            .body(REQUEST_BODY.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let _ = resp.bytes().await.unwrap();
        let outcome = rx.recv().await.expect("responses outcome reported");
        assert_eq!(outcome.verified, Some(false));
        assert!(outcome.detail.contains("spec 5.2"), "{}", outcome.detail);

        // GET passthrough routes and reports without a receipt check.
        let models = http.get(format!("{proxy}/v1/models")).send().await.unwrap();
        assert_eq!(models.status().as_u16(), 200);
        let models_outcome = rx.recv().await.expect("models outcome reported");
        assert_eq!(models_outcome.method, Method::GET);
        assert_eq!(models_outcome.verified, None);
    }
}
