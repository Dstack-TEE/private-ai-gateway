//! `private-ai-proxy serve`: a local verifying proxy that fails closed on the attested
//! service.
//!
//! Startup verifies `<base-url>` (spec 9.1) and refuses to listen unless
//! the verdict is VERIFIED. The proxy exposes a plaintext local API, rejects
//! E2EE request headers, and forwards accepted traffic over the SPKI-pinned
//! channel. Receipt auditing never delays response delivery.
//! Only digests and verdicts are retained after the request; bodies never go to disk.
//! No bodies are logged.

use std::collections::VecDeque;
use std::future::Future;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::capture::{tee, CompletionHook, StreamEnd};
use axum::body::{to_bytes, Body, Bytes};
use axum::extract::Path;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use axum::{Json, Router};
use desktop_gateway::proxy::{
    hop_by_hop_names, ForwardContext, ProxyEvent, VerifiedResponse, VerifiedService, MAX_BODY_BYTES,
};
use desktop_runtime::gateway::{
    IdentityEvent, IdentitySourceProvenance, ServiceCapabilities, VerifierConfig, VerifierEvent,
    VerifierEventSink, VerifierLauncher, VerifierTask,
};
use futures_util::StreamExt;
use private_ai_proxy::aci::types::{
    AttestationReport, PROVIDER_ACI_SESSION_IDS, PROVIDER_ACI_VERIFIED,
};
use serde_json::{json, Value};

use crate::args::ServeArgs;
use crate::checks::{
    parse_receipt_document, run_response_checks, session_id_from_receipt, BodyDigest,
    EstablishedIdentity, RequiredClaim, UpstreamContext,
};
use crate::client::{AciClient, HttpResult};
use crate::sessions::audit_current_sessions;
use crate::transcript::{Status, Transcript};
use crate::verify::{verify_service, ServiceVerification};

macro_rules! diagnostic {
    ($($arg:tt)*) => {
        desktop_runtime::diagnostic(format_args!($($arg)*))
    };
}

/// Headers that select either E2EE v2 or the legacy encrypted transport.
/// `private-ai-proxy serve` exposes a plaintext local API, so even a partial encrypted
/// request is rejected instead of being forwarded or silently downgraded.
const E2EE_REQUEST_HEADERS: &[&str] = &[
    "x-signing-algo",
    "x-client-pub-key",
    "x-model-pub-key",
    "x-e2ee-version",
    "x-e2ee-nonce",
    "x-e2ee-timestamp",
];

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
    /// Receipt associated with this inference response, when one was returned.
    pub receipt_id: Option<String>,
    /// `Some` when receipt checks reached a verdict; `None` when no receipt
    /// applies or its verification could not be completed.
    pub verified: Option<bool>,
    /// The one-line detail printed after the request line, e.g.
    /// `receipt rcpt-1: signature ok, wire hash ok, upstream tee_attested asserted (hardware_proven)`.
    pub detail: String,
    /// Direct request attribution for the managed backend. Standalone serves
    /// leave this empty.
    #[allow(dead_code)]
    pub context: Option<ForwardContext>,
    /// Whether the receipt records a service-side rewrite of the request
    /// (§9.3 note); known only once the receipt was checked.
    pub rewritten: Option<bool>,
    /// Whether this proxy changed the request body before forwarding (ACI
    /// policy: `provider.aci_verified` / pinned sessions, re-serialized).
    /// True when local verification policy was applied to the outgoing request.
    pub local_policy_applied: bool,
}

type Reporter = Arc<dyn Fn(RequestOutcome) + Send + Sync>;
type EventSink = VerifierEventSink;

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

/// One forwarded POST exchange, recorded as digests for its asynchronous
/// receipt audit. Bodies are never stored.
#[derive(Clone)]
pub struct RecordedExchange {
    pub receipt_id: String,
    pub path: String,
    pub status: u16,
    pub streamed: bool,
    /// Digest of the plaintext request bytes this proxy forwarded.
    pub request: BodyDigest,
    /// Digest of the response wire bytes observed by the proxy: the full body,
    /// or the partial body when delivery failed or the client stopped reading.
    pub response: BodyDigest,
    delivery: ResponseDelivery,
    /// The client's §5.3 pinned session ids from the request body.
    pub pinned_sessions: Vec<String>,
    pub at: u64,
    /// Verdict of the last verification, when one reached a verdict.
    pub verified: Option<bool>,
    pub context: Option<ForwardContext>,
    /// Whether the forwarded body differs from what the caller sent.
    pub local_policy_applied: bool,
}

#[derive(Clone)]
enum ResponseDelivery {
    Complete,
    Failed(String),
    Cancelled,
}

/// Recorded exchanges kept for standalone on-demand verification.
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
    audits: Arc<tokio::sync::Semaphore>,
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
    /// Cancels every managed verifier operation when its owning task stops.
    /// Standalone `serve` owns a token that remains live for the listener's
    /// lifetime.
    shutdown: tokio_util::sync::CancellationToken,
    /// Delivery gate (same pattern as the desktop proxy): every identity loss
    /// or replacement cancels this token, so a request that passed the entry
    /// checks but has not started sending upstream is refused, never sent
    /// under a decision made for the old identity.
    delivery: Mutex<tokio_util::sync::CancellationToken>,
    #[cfg(test)]
    pause: Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
    event_sink: EventSink,
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
        event_sink: EventSink,
        shutdown: tokio_util::sync::CancellationToken,
    ) -> Self {
        let keyset_digest = report.workload_keyset_digest.clone();
        Self {
            client,
            base_url,
            host,
            enforce_verified,
            accepted_composes,
            require_production_os,
            audits: Arc::new(tokio::sync::Semaphore::new(16)),
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
            event_sink,
            delivery: Mutex::new(shutdown.child_token()),
            shutdown,
            #[cfg(test)]
            pause: Mutex::new(None),
        }
    }

    fn delivery_token(&self) -> tokio_util::sync::CancellationToken {
        self.delivery
            .lock()
            .expect("delivery gate poisoned")
            .clone()
    }

    fn record(&self, exchange: RecordedExchange) {
        let mut recorded = self.recorded.lock().expect("recorded ring poisoned");
        if recorded.len() == RECORDED_CAP {
            recorded.pop_front();
        }
        recorded.push_back(exchange);
    }

    /// Cancel every delivery admitted under the previous identity.
    fn revoke_deliveries(&self) {
        let previous = std::mem::replace(
            &mut *self.delivery.lock().expect("delivery gate poisoned"),
            self.shutdown.child_token(),
        );
        previous.cancel();
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
        let verification = tokio::select! {
            _ = self.shutdown.cancelled() => {
                return Err("verifier stopped during re-verification".to_string());
            }
            result = verify_service(
                &self.base_url,
                None,
                &self.accepted_composes,
                self.require_production_os,
                false,
            ) => result?,
        };
        if !verification.transcript.verified() {
            return Err("service re-verification did not reach VERIFIED".to_string());
        }
        if let Some(spki) = &verification.observed_spki {
            self.client.pin(&self.host, spki);
        }
        let verification_summary = verification.transcript.to_json(false);
        let keyset_digest = verification.report.workload_keyset_digest.clone();
        let identity = verification
            .identity
            .ok_or("verified run carried no established identity")?;
        let identity_event = VerifierEvent::IdentityUpdated {
            identity: identity_event(
                &verification.report,
                &identity,
                verification.observed_spki.as_deref(),
                verification_summary,
            ),
        };
        // The identity is being replaced: nothing admitted under the old one
        // may still be delivered.
        self.revoke_deliveries();
        *self.trusted.lock().expect("trusted identity poisoned") = TrustedIdentity {
            report: Arc::new(verification.report),
            keyset_digest,
            not_after: identity.keyset.not_after,
            identity: Arc::new(identity),
        };
        self.blocked.store(false, Ordering::SeqCst);
        (self.event_sink)(identity_event);
        diagnostic!("private-ai-proxy serve: re-verified after keyset change; resuming forwards");
        Ok(())
    }
}

impl VerifiedService for ProxyState {
    fn call(
        self: Arc<Self>,
        request: axum::http::Request<Body>,
        context: Option<ForwardContext>,
    ) -> VerifiedResponse {
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let body = match tokio::select! {
                _ = self.shutdown.cancelled() => {
                    return text_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "verifier stopped before the request could be read\n",
                    );
                }
                result = to_bytes(body, MAX_BODY_BYTES) => result,
            } {
                Ok(body) => body,
                Err(_) => {
                    return text_response(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "request body exceeds the verifier limit\n",
                    )
                }
            };
            proxy_request(self, parts.method, parts.uri, parts.headers, body, context).await
        })
    }
}

#[allow(dead_code)]
pub struct InProcessVerifierLauncher {
    runtime: tokio::runtime::Handle,
}

#[allow(dead_code)]
impl InProcessVerifierLauncher {
    pub fn new(runtime: tokio::runtime::Handle) -> Self {
        Self { runtime }
    }

    fn spawn_task<F, Fut>(&self, events: VerifierEventSink, worker: F) -> Box<dyn VerifierTask>
    where
        F: FnOnce(tokio_util::sync::CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        let cancellation = tokio_util::sync::CancellationToken::new();
        let worker = self.runtime.spawn(worker(cancellation.clone()));
        self.runtime.spawn(async move {
            let error = match worker.await {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(error) if error.is_cancelled() => None,
                Err(error) => Some(format!("Verifier task failed: {error}")),
            };
            events(VerifierEvent::Terminated { error });
        });
        Box::new(InProcessVerifierTask { cancellation })
    }
}

#[allow(dead_code)]
struct InProcessVerifierTask {
    cancellation: tokio_util::sync::CancellationToken,
}

impl VerifierTask for InProcessVerifierTask {
    fn stop(&mut self) -> Result<(), String> {
        self.cancellation.cancel();
        Ok(())
    }
}

impl Drop for InProcessVerifierTask {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl VerifierLauncher for InProcessVerifierLauncher {
    fn spawn(
        &self,
        config: VerifierConfig,
        events: VerifierEventSink,
        requests: tokio::sync::mpsc::Sender<ProxyEvent>,
    ) -> Result<Box<dyn VerifierTask>, String> {
        let worker_events = events.clone();
        let task = self.spawn_task(events, move |cancelled| async move {
            let args = ServeArgs {
                base_url: config.remote_url.clone(),
                accepted_composes: Vec::new(),
                listen: None,
                control: None,
                json_events: false,
                allow_unverified: false,
                sessions: Vec::new(),
                require_claims: Vec::new(),
            };
            let reporter = managed_reporter(requests);
            let initialized = tokio::select! {
                _ = cancelled.cancelled() => return Ok::<(), String>(()),
                result = initialize(
                    &args,
                    config.require_production_os,
                    reporter,
                    worker_events.clone(),
                    cancelled.clone(),
                    false,
                ) => result,
            };
            match initialized {
                Ok((state, identity, remote_url)) => {
                    worker_events(VerifierEvent::Ready {
                        identity,
                        remote_url,
                        service: state,
                    });
                    cancelled.cancelled().await;
                    Ok::<(), String>(())
                }
                Err(error) => {
                    worker_events(VerifierEvent::Fatal {
                        message: error.clone(),
                    });
                    Err(error)
                }
            }
        });
        Ok(task)
    }
}

pub async fn run(args: ServeArgs, require_production_os: bool) -> Result<i32, String> {
    let json_events = args.json_events;
    match run_inner(args, require_production_os).await {
        Ok(code) => Ok(code),
        Err(error) if json_events => {
            write_json_event(&json!({"type": "fatal", "message": error}))?;
            Ok(1)
        }
        Err(error) => Err(error),
    }
}

async fn run_inner(args: ServeArgs, require_production_os: bool) -> Result<i32, String> {
    let reporter: Reporter = if args.json_events {
        Arc::new(json_reporter)
    } else {
        Arc::new(default_reporter)
    };
    let event_sink: EventSink = if args.json_events {
        Arc::new(json_event_sink)
    } else {
        Arc::new(|_| {})
    };
    let (state, ready_identity, base_url) = initialize(
        &args,
        require_production_os,
        reporter,
        event_sink,
        tokio_util::sync::CancellationToken::new(),
        true,
    )
    .await?;

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
    if args.json_events {
        let event = lifecycle_json(
            "ready",
            ready_identity,
            Some(json!({
                "proxy_url": format!("http://{local}"),
                "control_url": format!("http://{control_local}"),
                "remote_url": base_url,
                "policy": {
                    "enforce_verified": !args.allow_unverified,
                    "require_production_os": require_production_os,
                    "accepted_composes": args.accepted_composes,
                    "pinned_sessions": state.active_pins(),
                },
            })),
        );
        write_json_event(&event)?;
    } else {
        println!();
        println!(
            "private-ai-proxy serve: proxying {base_url} on http://{local} (plain HTTP, localhost)"
        );
        println!(
            "forwarding every method and path; Authorization passed through unchanged; every \
             upstream hop pinned to the attested TLS key; each POST response's receipt id and \
             body digests recorded; responses stream immediately and receipts are audited after delivery.\n\
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
    }

    let control_server = axum::serve(control_listener, build_control_router(state.clone()));
    let proxy_server = axum::serve(listener, build_proxy_router(state));
    tokio::select! {
        result = control_server => {
            result.map_err(|e| format!("control server error: {e}"))?;
        }
        result = proxy_server => {
            result.map_err(|e| format!("proxy server error: {e}"))?;
        }
    }
    Ok(0)
}

async fn initialize(
    args: &ServeArgs,
    require_production_os: bool,
    reporter: Reporter,
    event_sink: EventSink,
    shutdown: tokio_util::sync::CancellationToken,
    interactive: bool,
) -> Result<(Arc<ProxyState>, IdentityEvent, String), String> {
    let verification = verify_service(
        &args.base_url,
        None,
        &args.accepted_composes,
        require_production_os,
        false,
    )
    .await?;
    if interactive && !args.json_events {
        println!("== service verification: {} ==", verification.base_url);
        print!("{}", verification.transcript.render_human(false));
    }
    if !verification.transcript.verified() {
        return Err(
            "service verification failed; refusing to start the proxy (fail closed)".to_string(),
        );
    }

    let verification_summary = verification.transcript.to_json(false);
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
    let ready_identity = identity_event(
        &report,
        &identity,
        observed_spki.as_deref(),
        verification_summary,
    );
    // Pin the just-verified TLS key on every future hop to this host.
    if let Some(spki) = &observed_spki {
        client.pin(&host, spki);
    }
    let state = ProxyState::new(
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
        reporter,
        event_sink,
        shutdown,
    );
    let state = Arc::new(state);
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
        if interactive && !args.json_events {
            println!("policy-accepted sessions pinned ({}):", pins.len());
            for pin in &pins {
                println!("  {pin}");
            }
        }
        *state.policy_pins.lock().expect("policy pins poisoned") = pins;
    }

    Ok((state, ready_identity, base_url))
}

fn build_control_router(state: Arc<ProxyState>) -> Router {
    Router::new()
        .route("/receipts", axum::routing::get(control_list))
        .route("/receipts/{id}/verify", axum::routing::post(control_verify))
        .with_state(state)
}

fn build_proxy_router(state: Arc<ProxyState>) -> Router {
    // No route list: every method and path forwards to the same path on the
    // service, so protocol surfaces this proxy does not know about
    // (Appendix B) keep working. POST is the inference surface (§5.1) and
    // gets the receipt check; everything else is read-only passthrough.
    Router::new()
        .fallback(proxy)
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

async fn proxy(
    axum::extract::State(state): axum::extract::State<Arc<ProxyState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    proxy_request(state, method, uri, headers, body, None).await
}

async fn proxy_request(
    state: Arc<ProxyState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    context: Option<ForwardContext>,
) -> Response {
    let path = uri.path().to_string();
    // Every method re-checks verification (§3.4): an expired or rotated keyset
    // blocks GET passthrough exactly like inference. A re-verification that
    // changed the service identity must not carry this request either: it
    // was admitted upstream against the old identity, so it is refused with a
    // retryable status until the desktop publishes the new identity.
    if crate::checks::now_secs() >= state.snapshot().not_after {
        state.blocked.store(true, Ordering::SeqCst);
        state.revoke_deliveries();
    }
    let identity_before = state.snapshot().keyset_digest;
    if let Err(reason) = state.ensure_unblocked().await {
        (state.event_sink)(VerifierEvent::Blocked {
            code: None,
            reason: reason.clone(),
        });
        diagnostic!("!! {method} {path} -> 503 blocked: {reason}");
        return text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "upstream keyset changed or expired and re-verification failed; refusing to forward\n",
        );
    }
    if state.snapshot().keyset_digest != identity_before {
        diagnostic!("!! {method} {path} -> 503 identity changed during re-verification; retry");
        return text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "service identity changed during re-verification; retry once the gateway is verified again\n",
        );
    }
    if method == Method::POST {
        proxy_inference(state, uri, headers, body, context).await
    } else {
        proxy_passthrough(state, method, uri, headers, body, context).await
    }
}

/// Non-POST passthrough: streamed byte-exact, no receipt to check.
async fn proxy_passthrough(
    state: Arc<ProxyState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    context: Option<ForwardContext>,
) -> Response {
    let path = uri.path().to_string();
    let delivery = state.delivery_token();
    let url = join_url(&state.base_url, &uri);
    let mut req = forward_headers(state.client.request(method.clone(), &url), &headers);
    if !body.is_empty() {
        req = req.body(body.to_vec());
    }
    let mut resp = match race_delivery(&delivery, req.send()).await {
        None => return delivery_revoked_response(&path),
        Some(Ok(resp)) => resp,
        Some(Err(e)) => return send_error(&state, method, path, e, context),
    };
    let status = resp.status().as_u16();
    let resp_headers = resp.headers().clone();
    rotation_gate(&state, &state.snapshot().keyset_digest, &resp_headers);
    (state.reporter)(RequestOutcome {
        method,
        path,
        status,
        streamed: false,
        receipt_id: None,
        verified: None,
        detail: String::new(),
        context,
        rewritten: None,
        local_policy_applied: false,
    });
    let mut builder = Response::builder().status(status);
    let dropped = dropped_headers(&resp_headers);
    for (name, value) in resp_headers.iter() {
        if !dropped.contains(name.as_str()) {
            builder = builder.header(name, value);
        }
    }
    let upstream = async_stream::stream! {
        loop {
            match race_delivery(&delivery, resp.chunk()).await {
                Some(Ok(Some(chunk))) => yield Ok::<Bytes, std::io::Error>(chunk),
                Some(Ok(None)) => break,
                Some(Err(_)) => {
                    yield Err(std::io::Error::other("Upstream response interrupted"));
                    break;
                }
                None => {
                    yield Err(std::io::Error::other("Protection revoked during response delivery"));
                    break;
                }
            }
        }
    };
    builder
        .body(Body::from_stream(upstream))
        .unwrap_or_else(|_| internal_error())
}

/// Inference forward (any POST): streamed byte-exact, wire bytes teed for the
/// after-the-fact receipt check.
async fn proxy_inference(
    state: Arc<ProxyState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    context: Option<ForwardContext>,
) -> Response {
    let path = uri.path().to_string();

    if has_e2ee_request_headers(&headers) {
        diagnostic!("!! POST {path} -> 400 E2EE request rejected by plaintext local API");
        return text_response(
            StatusCode::BAD_REQUEST,
            "private-ai-proxy serve accepts plaintext requests only; remove E2EE request headers\n",
        );
    }

    let trusted = state.snapshot();
    let delivery = state.delivery_token();
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
                diagnostic!("!! POST {path} -> 400: {reason}");
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
    #[cfg(test)]
    {
        let pause = state.pause.lock().expect("pause poisoned").clone();
        if let Some((reached, resume)) = pause {
            reached.notify_one();
            resume.notified().await;
        }
    }
    let mut resp = match race_delivery(&delivery, send(request_body.clone())).await {
        None => return delivery_revoked_response(&path),
        Some(Ok(resp)) => resp,
        Some(Err(e)) => return send_error(&state, Method::POST, path, e, context.clone()),
    };
    // A 412 refusal against a policy-derived pin set means the sessions
    // rotated under us (§8 supersession): refresh the set from the service's
    // current sessions and retry once. §5.3 refuses before serving, so
    // nothing ran on the refused attempt. A user-fixed --session list is
    // never refreshed; its 412 surfaces as-is.
    if resp.status().as_u16() == 412 && injected_policy_pins {
        match derive_policy_pins(&state).await {
            Ok(pins) if !pins.is_empty() && pins != active_pins => {
                diagnostic!(
                    "private-ai-proxy serve: pinned sessions refused (412); policy re-accepted {} current \
                     session(s), retrying",
                    pins.len()
                );
                *state.policy_pins.lock().expect("policy pins poisoned") = pins.clone();
                request_body = match apply_constraints(body.to_vec(), state.enforce_verified, &pins)
                {
                    Ok(body) => body,
                    Err(reason) => {
                        diagnostic!("private-ai-proxy serve: refreshed session policy rejected request: {reason}");
                        return text_response(
                            StatusCode::BAD_REQUEST,
                            "request session ids are not accepted by the refreshed ACI policy\n",
                        );
                    }
                };
                match race_delivery(&delivery, send(request_body.clone())).await {
                    None => return delivery_revoked_response(&path),
                    Some(Ok(retried)) => resp = retried,
                    Some(Err(e)) => {
                        return send_error(&state, Method::POST, path, e, context.clone())
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                diagnostic!("private-ai-proxy serve: policy pin refresh after 412 failed: {e}")
            }
        }
    }
    let status = resp.status().as_u16();
    let resp_headers = resp.headers().clone();
    rotation_gate(&state, &trusted.keyset_digest, &resp_headers);

    let receipt_id = header_str(&resp_headers, "x-receipt-id").map(str::to_string);
    let streamed = header_str(&resp_headers, "content-type")
        .is_some_and(|ct| ct.contains("text/event-stream"));

    let mut builder = Response::builder().status(status);
    let dropped = dropped_headers(&resp_headers);
    for (name, value) in resp_headers.iter() {
        if !dropped.contains(name.as_str()) {
            builder = builder.header(name, value);
        }
    }

    // Streaming modes record digests for background audits.
    // Non-success responses without receipts also use this passthrough path.
    let hook_state = state.clone();
    let hook_path = path.clone();
    let local_policy_applied = request_body.as_slice() != body.as_ref();
    let request_digest = BodyDigest::of(&request_body);
    // §9.3(6): the pinned ids ride along so the asynchronous audit enforces
    // the same membership rule as `private-ai-proxy send --session`.
    let pinned_sessions = pinned_session_ids(&request_body);
    let audit_bearer = bearer_token(&headers);
    let hook_delivery = delivery.clone();
    let hook: CompletionHook = Box::new(move |end| {
        let (response, delivery) = match end {
            StreamEnd::Complete(digest) => (digest, ResponseDelivery::Complete),
            // ACI §9.3(4) uses the wire hash to catch truncation, so it must
            // reach the report rather than vanish.
            StreamEnd::Errored { partial, error: _ } if hook_delivery.is_cancelled() => {
                (partial, ResponseDelivery::Cancelled)
            }
            StreamEnd::Errored { partial, error } => (partial, ResponseDelivery::Failed(error)),
            StreamEnd::Cancelled(partial) => (partial, ResponseDelivery::Cancelled),
        };
        let outcome =
            |receipt_id: Option<String>, verified: Option<bool>, detail: &str| RequestOutcome {
                method: Method::POST,
                path: hook_path.clone(),
                status,
                streamed,
                receipt_id,
                verified,
                detail: detail.to_string(),
                context: context.clone(),
                rewritten: None,
                local_policy_applied,
            };
        if let Some(receipt_id) = receipt_id {
            let exchange = RecordedExchange {
                receipt_id: receipt_id.clone(),
                path: hook_path.clone(),
                status,
                streamed,
                request: request_digest,
                response,
                delivery: delivery.clone(),
                pinned_sessions,
                at: crate::checks::now_secs(),
                verified: None,
                context: context.clone(),
                local_policy_applied,
            };
            hook_state.record(exchange.clone());
            if matches!(delivery, ResponseDelivery::Complete) {
                (hook_state.reporter)(outcome(
                    Some(receipt_id),
                    None,
                    "Response delivered; receipt audit pending",
                ));
            }
            audit_exchange(hook_state, trusted, exchange, audit_bearer);
            return;
        }
        // A 2xx POST completion with no receipt header can never be audited:
        // fail loudly (spec 5.2 puts a receipt on every inference response).
        // Non-2xx responses legitimately carry none.
        let (verified, detail) = if (200..300).contains(&status) {
            (
                Some(false),
                "no X-Receipt-Id on a 2xx POST response (spec 5.2); nothing recorded",
            )
        } else {
            (None, "no X-Receipt-Id returned; nothing to verify")
        };
        (hook_state.reporter)(outcome(None, verified, detail));
    });

    let upstream = async_stream::stream! {
        loop {
            match race_delivery(&delivery, resp.chunk()).await {
                Some(Ok(Some(chunk))) => yield Ok(chunk),
                Some(Ok(None)) => break,
                Some(Err(_)) => {
                    yield Err(std::io::Error::other("Upstream response interrupted"));
                    break;
                }
                None => {
                    yield Err(std::io::Error::other("Protection revoked during response delivery"));
                    break;
                }
            }
        }
    };
    // Own the upstream body in a bounded producer task. If an Agent stops
    // reading after its protocol-level terminal event, the receiver closes;
    // the producer then drains without buffering so the full wire digest and
    // receipt audit still complete. While the Agent is connected, the bounded
    // channel preserves normal backpressure.
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    tokio::spawn(async move {
        let mut stream = Box::pin(tee(upstream, hook));
        let mut downstream_open = true;
        while let Some(item) = stream.next().await {
            if downstream_open && sender.send(item).await.is_err() {
                downstream_open = false;
            }
        }
    });
    let stream = async_stream::stream! {
        while let Some(item) = receiver.recv().await {
            yield item;
        }
    };
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| internal_error())
}

fn audit_exchange(
    state: Arc<ProxyState>,
    trusted: TrustedIdentity,
    exchange: RecordedExchange,
    bearer: Option<String>,
) {
    let report_state = state.clone();
    let report_exchange = exchange.clone();
    let report = move |verified, detail, rewritten| {
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
            );
            return;
        }
        ResponseDelivery::Cancelled => {
            report(
                None,
                "Response stream was canceled or protection stopped; no complete response proof was recorded."
                    .into(),
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
            Ok(Ok((transcript, detail))) => report(
                Some(transcript.verified()),
                format!("Post-delivery receipt audit: {detail}"),
                Some(rewrite_noted(&transcript)),
            ),
            Ok(Err(_)) => report(
                None,
                format!(
                    "Response delivered; receipt audit could not complete. Standalone serve can retry with POST /receipts/{}/verify.",
                    exchange.receipt_id
                ),
                None,
            ),
            Err(_) => report(
                None,
                format!(
                    "Response delivered; receipt audit timed out. Standalone serve can retry with POST /receipts/{}/verify.",
                    exchange.receipt_id
                ),
                None,
            ),
        }
    });
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
    let report = |verified: Option<bool>, rewritten: Option<bool>, detail: String| {
        (state.reporter)(RequestOutcome {
            method: Method::POST,
            path: exchange.path.clone(),
            status: exchange.status,
            streamed: exchange.streamed,
            receipt_id: Some(exchange.receipt_id.clone()),
            verified,
            detail,
            context: exchange.context.clone(),
            rewritten,
            local_policy_applied: exchange.local_policy_applied,
        });
    };
    match verify_exchange(&state, &trusted, &exchange, bearer.as_deref()).await {
        Ok((transcript, detail)) => {
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
            report(Some(verified), rewritten, detail);
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
            report(None, None, detail.clone());
            json_response(StatusCode::BAD_GATEWAY, json!({ "error": detail }))
        }
    }
}

async fn verify_exchange(
    state: &ProxyState,
    trusted: &TrustedIdentity,
    exchange: &RecordedExchange,
    bearer: Option<&str>,
) -> Result<(Transcript, String), String> {
    if matches!(&exchange.delivery, ResponseDelivery::Cancelled) {
        return Err(
            "the client canceled the response stream before its complete bytes were observed"
                .to_string(),
        );
    }
    let receipt_resp = fetch_receipt_for_audit(state, &exchange.receipt_id, bearer).await?;
    let receipt = receipt_resp.json().and_then(parse_receipt_document)?;

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
    Ok((transcript, detail))
}

const AUDIT_RETRY_DELAYS_MS: [u64; 4] = [100, 250, 500, 1_000];

fn transient_audit_status(status: u16) -> bool {
    status == 404 || status == 429 || (500..600).contains(&status)
}

enum AuditFetchError {
    Status(u16),
    Transport(String),
}

async fn fetch_audit_artifact<F, Fut>(mut fetch: F) -> Result<HttpResult, AuditFetchError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<HttpResult, String>>,
{
    let mut delays = AUDIT_RETRY_DELAYS_MS.into_iter();
    loop {
        match fetch().await {
            Ok(response) if (200..300).contains(&response.status) => return Ok(response),
            Ok(response) if transient_audit_status(response.status) => {
                let Some(delay_ms) = delays.next() else {
                    return Err(AuditFetchError::Status(response.status));
                };
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            Ok(response) => return Err(AuditFetchError::Status(response.status)),
            Err(error) => {
                let Some(delay_ms) = delays.next() else {
                    return Err(AuditFetchError::Transport(error));
                };
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
        }
    }
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

/// Whether the receipt notes a service-side rewrite (§9.3 informational note).
fn rewrite_noted(transcript: &Transcript) -> bool {
    status_of(transcript, "receipt-note") == Some(Status::Info)
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
        diagnostic!("!! {line}");
    } else {
        println!("{line}");
    }
}

fn json_reporter(outcome: RequestOutcome) {
    let event = request_outcome_event(outcome);
    if let Err(error) = write_json_event(&event) {
        diagnostic!("private-ai-proxy serve: cannot write JSON event: {error}");
    }
}

fn json_event_sink(event: VerifierEvent) {
    let value = match event {
        VerifierEvent::IdentityUpdated { identity } => {
            lifecycle_json("identity_updated", identity, None)
        }
        VerifierEvent::Blocked {
            code: Some(code),
            reason,
        } => json!({"type": "blocked", "code": code, "reason": reason}),
        VerifierEvent::Blocked { code: None, reason } => {
            json!({"type": "blocked", "reason": reason})
        }
        VerifierEvent::Fatal { message } => json!({"type": "fatal", "message": message}),
        VerifierEvent::Terminated { error } => {
            json!({"type": "terminated", "error": error})
        }
        VerifierEvent::Ready { .. } => return,
    };
    if let Err(error) = write_json_event(&value) {
        diagnostic!("private-ai-proxy serve: cannot write JSON event: {error}");
    }
}

fn identity_event(
    report: &AttestationReport,
    identity: &EstablishedIdentity,
    observed_spki: Option<&str>,
    verification: Value,
) -> IdentityEvent {
    IdentityEvent {
        trust_level: "hardware_verified".to_string(),
        tee_type: report.attestation.tee_type.clone(),
        keyset_digest: report.workload_keyset_digest.clone(),
        keyset_not_after: identity.keyset.not_after,
        tls_spki: observed_spki.map(str::to_string),
        source_provenance: IdentitySourceProvenance {
            repo_url: report.attestation.source_provenance.repo_url.clone(),
            repo_commit: report.attestation.source_provenance.repo_commit.clone(),
            image_digest: report.attestation.source_provenance.image_digest.clone(),
        },
        service_capabilities: ServiceCapabilities {
            serving: report.service_capabilities.serving.clone(),
            supported_e2ee_versions: report.service_capabilities.supported_e2ee_versions.clone(),
        },
        verification,
    }
}

fn request_outcome_event(outcome: RequestOutcome) -> Value {
    json!({
        "type": "request_complete",
        "method": outcome.method.as_str(),
        "path": outcome.path,
        "status": outcome.status,
        "streamed": outcome.streamed,
        "receipt_id": outcome.receipt_id,
        "verified": outcome.verified,
        "detail": outcome.detail,
        "rewritten": outcome.rewritten,
        "local_policy_applied": outcome.local_policy_applied,
    })
}

fn write_json_event(event: &impl serde::Serialize) -> Result<(), String> {
    let line = serde_json::to_string(event)
        .map_err(|error| format!("failed to serialize serve event: {error}"))?;
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "{line}").map_err(|error| format!("failed to write serve event: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("failed to flush serve event: {error}"))
}

fn lifecycle_json(kind: &str, identity: IdentityEvent, extra: Option<Value>) -> Value {
    let mut object = serde_json::to_value(identity)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    object.insert("type".to_string(), Value::String(kind.to_string()));
    if let Some(Value::Object(extra)) = extra {
        object.extend(extra);
    }
    Value::Object(object)
}

#[allow(dead_code)]
fn managed_reporter(events: tokio::sync::mpsc::Sender<ProxyEvent>) -> Reporter {
    // Receipt verdicts are correctness-sensitive. Serialize them through a
    // lossless handoff and await the runtime's bounded queue off the response
    // hot path. The gateway's own usage-progress emitter remains intentionally
    // lossy when its bounded queue is saturated.
    let (verdicts, mut pending) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(event) = pending.recv().await {
            if events.send(event).await.is_err() {
                break;
            }
        }
    });
    Arc::new(move |outcome| {
        let Some(context) = outcome.context else {
            return;
        };
        let _ = verdicts.send(ProxyEvent {
            generation: context.generation,
            request_id: context.request_id,
            session_id: context.session_id,
            agent: Some(context.agent),
            method: outcome.method.as_str().to_string(),
            path: outcome.path,
            model: context.model,
            status: outcome.status,
            streamed: outcome.streamed,
            receipt_id: outcome.receipt_id,
            verified: outcome.verified,
            detail: outcome.detail,
            at: context.at,
            local_policy_applied: Some(outcome.local_policy_applied),
            rewritten: outcome.rewritten,
            left_device: true,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost_usd: None,
        });
    })
}

/// Race an upstream send against the delivery gate; `None` means revoked
/// before (or while) sending, and nothing may be treated as delivered.
async fn race_delivery<T>(
    token: &tokio_util::sync::CancellationToken,
    send: impl std::future::Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        _ = token.cancelled() => None,
        result = send => Some(result),
    }
}

fn delivery_revoked_response(path: &str) -> Response {
    diagnostic!("!! {path} -> 503 verification changed before the request was sent");
    text_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "verification changed before the request was sent; retry once the gateway is verified again\n",
    )
}

/// Keyset-rotation gate (§3.4): a response advertising a digest other than
/// the trusted one blocks further inference forwards until a fresh verify
/// re-establishes trust.
fn rotation_gate(state: &ProxyState, trusted_digest: &str, headers: &HeaderMap) {
    if let Some(observed) = header_str(headers, "x-aci-keyset-digest") {
        if observed != trusted_digest {
            state.blocked.store(true, Ordering::SeqCst);
            state.revoke_deliveries();
            let reason = format!(
                "upstream keyset digest changed ({observed} != {trusted_digest}); re-verification required"
            );
            (state.event_sink)(VerifierEvent::Blocked {
                code: Some("keyset_changed".to_string()),
                reason,
            });
            diagnostic!(
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
        diagnostic!(
            "private-ai-proxy serve: session {} rejected ({})",
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
    let dropped = dropped_headers(headers);
    for (name, value) in headers.iter() {
        let name_str = name.as_str();
        if !dropped.contains(name_str) && name_str != "x-aci-tag" {
            req = req.header(name, value);
        }
    }
    req
}

fn dropped_headers(headers: &HeaderMap) -> std::collections::HashSet<String> {
    let mut names = hop_by_hop_names(headers);
    names.insert("host".to_string());
    names.insert("content-length".to_string());
    names
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
    method: Method,
    path: String,
    err: reqwest::Error,
    context: Option<ForwardContext>,
) -> Response {
    (state.reporter)(RequestOutcome {
        method,
        path,
        status: 502,
        streamed: false,
        receipt_id: None,
        verified: Some(false),
        detail: format!("upstream connection failed (possible TLS pin mismatch): {err}"),
        context,
        rewritten: None,
        local_policy_applied: false,
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

    /// A request that passed the entry checks but has not started sending is
    /// refused with zero delivery when the identity is lost in between (as a
    /// concurrent response's rotation gate would do).
    #[tokio::test]
    async fn blocked_after_entry_checks_delivers_nothing() {
        let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = delivered.clone();
        let upstream = Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Json(json!({ "ok": true })) }
            }),
        );
        let base = spawn_server(upstream).await;
        let (tx, _rx) = mpsc::unbounded_channel();
        let state = state_over(base, tx);
        let reached = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        *state.pause.lock().unwrap() = Some((reached.clone(), resume.clone()));
        let proxy = spawn_server(build_proxy_router(state.clone())).await;
        let request = tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{proxy}/v1/chat/completions"))
                .header("content-type", "application/json")
                .body(r#"{"model":"m","messages":[]}"#)
                .send()
                .await
                .unwrap()
        });
        reached.notified().await;
        state.blocked.store(true, Ordering::SeqCst);
        state.revoke_deliveries();
        resume.notify_one();
        let response = request.await.unwrap();
        assert_eq!(response.status().as_u16(), 503);
        assert_eq!(delivered.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// Stopping an in-process verifier cancels the same delivery gate used for
    /// identity loss, so admitted requests cannot outlive their owning task.
    #[tokio::test]
    async fn managed_shutdown_revokes_request_before_send() {
        let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = delivered.clone();
        let upstream = Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Json(json!({ "ok": true })) }
            }),
        );
        let base = spawn_server(upstream).await;
        let (tx, _rx) = mpsc::unbounded_channel();
        let state = state_over(base, tx);
        let reached = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        *state.pause.lock().unwrap() = Some((reached.clone(), resume.clone()));
        let proxy = spawn_server(build_proxy_router(state.clone())).await;
        let request = tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{proxy}/v1/chat/completions"))
                .header("content-type", "application/json")
                .body(r#"{"model":"m","messages":[]}"#)
                .send()
                .await
                .unwrap()
        });
        reached.notified().await;
        state.shutdown.cancel();
        resume.notify_one();
        let response = request.await.unwrap();
        assert_eq!(response.status().as_u16(), 503);
        assert_eq!(delivered.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn managed_shutdown_interrupts_passthrough_response() {
        let resume = Arc::new(tokio::sync::Notify::new());
        let upstream_resume = resume.clone();
        let upstream = Router::new().route(
            "/v1/models",
            get(move || {
                let resume = upstream_resume.clone();
                async move {
                    let body = async_stream::stream! {
                        yield Ok::<Bytes, std::io::Error>(Bytes::from_static(b"first"));
                        resume.notified().await;
                        yield Ok::<Bytes, std::io::Error>(Bytes::from_static(b"second"));
                    };
                    Response::builder()
                        .body(Body::from_stream(body))
                        .expect("streaming response is valid")
                }
            }),
        );
        let base = spawn_server(upstream).await;
        let (tx, _rx) = mpsc::unbounded_channel();
        let state = state_over(base, tx);
        let proxy = spawn_server(build_proxy_router(state.clone())).await;
        let response = reqwest::Client::new()
            .get(format!("{proxy}/v1/models"))
            .send()
            .await
            .unwrap();
        let mut body = response.bytes_stream();
        assert_eq!(body.next().await.unwrap().unwrap(), "first");

        state.shutdown.cancel();
        resume.notify_waiters();
        let interrupted = tokio::time::timeout(std::time::Duration::from_secs(1), body.next())
            .await
            .expect("managed shutdown should interrupt the response")
            .expect("the interrupted response should report an error");
        assert!(interrupted.is_err());
    }

    /// While blocked, every method is refused until re-verification succeeds;
    /// here the upstream offers no attestation, so it cannot.
    #[tokio::test]
    async fn non_post_requests_are_refused_while_blocked() {
        let upstream = Router::new().route(
            "/v1/models",
            get(|| async { Json(json!({ "data": [{ "id": "demo-model" }] })) }),
        );
        let base = spawn_server(upstream).await;
        let (tx, _rx) = mpsc::unbounded_channel();
        let state = state_over(base, tx);
        state.blocked.store(true, Ordering::SeqCst);
        let proxy = spawn_server(build_proxy_router(state)).await;
        let resp = reqwest::Client::new()
            .get(format!("{proxy}/v1/models"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 503);
    }

    #[test]
    fn connection_named_headers_are_stripped_in_both_directions() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", "close, X-Private".parse().unwrap());
        headers.insert("x-private", "1".parse().unwrap());
        headers.insert("proxy-connection", "keep-alive".parse().unwrap());
        headers.insert("content-length", "1".parse().unwrap());
        let dropped = dropped_headers(&headers);
        assert!(dropped.contains("x-private"));
        assert!(dropped.contains("proxy-connection"));
        assert!(dropped.contains("content-length"));
        assert!(!dropped.contains("x-receipt-id"));
    }

    #[test]
    fn request_outcome_event_is_stable_json() {
        let outcome = RequestOutcome {
            method: Method::POST,
            path: "/v1/messages".to_string(),
            status: 200,
            streamed: true,
            receipt_id: Some("rcpt-1".to_string()),
            verified: Some(true),
            detail: "receipt verified".to_string(),
            context: None,
            rewritten: Some(false),
            local_policy_applied: true,
        };
        let event = serde_json::to_value(request_outcome_event(outcome)).unwrap();
        assert_eq!(event["local_policy_applied"], true);

        assert_eq!(event["type"], "request_complete");
        assert_eq!(event["method"], "POST");
        assert_eq!(event["receipt_id"], "rcpt-1");
        assert_eq!(event["verified"], true);
    }

    #[tokio::test]
    async fn keyset_change_and_verification_failure_have_distinct_events() {
        let upstream =
            spawn_server(Router::new().fallback(|| async { StatusCode::SERVICE_UNAVAILABLE }))
                .await;
        let (outcomes, _) = mpsc::unbounded_channel();
        let mut state = state_over(upstream, outcomes);
        let (events, mut received) = mpsc::unbounded_channel();
        Arc::get_mut(&mut state).unwrap().event_sink = Arc::new(move |event| {
            let _ = events.send(event);
        });
        let previous = state.delivery.lock().unwrap().clone();
        let mut headers = HeaderMap::new();
        headers.insert("x-aci-keyset-digest", "new-keyset".parse().unwrap());
        rotation_gate(&state, "old-keyset", &headers);
        assert!(previous.is_cancelled());
        assert!(matches!(
            received.recv().await.unwrap(),
            VerifierEvent::Blocked { code: Some(code), .. } if code == "keyset_changed"
        ));
        let proxy = spawn_server(build_proxy_router(state)).await;
        let response = reqwest::Client::new()
            .get(format!("{proxy}/v1/models"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(matches!(
            received.recv().await.unwrap(),
            VerifierEvent::Blocked { code: None, .. }
        ));
    }

    #[test]
    fn identity_event_carries_the_verified_workload_summary() {
        let report = vector_report();
        let identity = crate::checks::established_identity(&report).unwrap();
        let event = lifecycle_json(
            "ready",
            identity_event(
                &report,
                &identity,
                Some("sha256:observed"),
                json!({ "checks": [] }),
            ),
            Some(json!({
                "remote_url": "https://tee.example",
                "proxy_url": "http://127.0.0.1:4181",
                "control_url": "http://127.0.0.1:4182",
                "policy": {},
            })),
        );

        assert_eq!(event["type"], "ready");
        assert_eq!(event["tee_type"], "tdx");
        assert_eq!(event["keyset_digest"], report.workload_keyset_digest);
        assert_eq!(event["tls_spki"], "sha256:observed");
        assert_eq!(event["control_url"], "http://127.0.0.1:4182");
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
            Arc::new(|_| {}),
            tokio_util::sync::CancellationToken::new(),
        ))
    }

    #[tokio::test]
    async fn client_cancelled_stream_is_not_a_failed_proof() {
        let (tx, mut outcomes) = mpsc::unbounded_channel();
        let state = state_over("http://127.0.0.1:9".to_string(), tx);
        let exchange = RecordedExchange {
            receipt_id: "rcpt-cancelled".to_string(),
            path: "/v1/responses".to_string(),
            status: 200,
            streamed: true,
            request: BodyDigest::of(REQUEST_BODY),
            response: BodyDigest::of(b"data: partial\n\n"),
            delivery: ResponseDelivery::Cancelled,
            pinned_sessions: Vec::new(),
            at: 1,
            verified: None,
            context: None,
            local_policy_applied: false,
        };
        let trusted = state.snapshot();

        audit_exchange(state, trusted, exchange, None);

        let outcome = outcomes.recv().await.expect("cancellation outcome");
        assert_eq!(outcome.verified, None);
        assert!(outcome.detail.contains("canceled or protection stopped"));
    }

    #[tokio::test]
    async fn standalone_control_lists_and_retries_recorded_exchanges() {
        let (tx, _outcomes) = mpsc::unbounded_channel();
        let state = state_over("http://127.0.0.1:9".to_string(), tx);
        state.record(RecordedExchange {
            receipt_id: "rcpt-control".to_string(),
            path: "/v1/responses".to_string(),
            status: 200,
            streamed: true,
            request: BodyDigest::of(REQUEST_BODY),
            response: BodyDigest::of(b"partial"),
            delivery: ResponseDelivery::Cancelled,
            pinned_sessions: Vec::new(),
            at: 17,
            verified: None,
            context: None,
            local_policy_applied: false,
        });
        let control = spawn_server(build_control_router(state)).await;

        let listed: Value = reqwest::get(format!("{control}/receipts"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed[0]["receipt_id"], "rcpt-control");
        assert_eq!(listed[0]["cancelled"], true);

        let retried = reqwest::Client::new()
            .post(format!("{control}/receipts/rcpt-control/verify"))
            .send()
            .await
            .unwrap();
        assert_eq!(retried.status(), StatusCode::BAD_GATEWAY);
        let body: Value = retried.json().await.unwrap();
        assert!(body["error"].as_str().unwrap().contains("canceled"));
    }

    #[tokio::test]
    async fn transient_receipt_and_session_fetches_are_retried() {
        let receipt_calls = Arc::new(AtomicUsize::new(0));
        let session_calls = Arc::new(AtomicUsize::new(0));
        let upstream = Router::new()
            .route(
                "/v1/aci/receipts/{id}",
                get({
                    let calls = receipt_calls.clone();
                    move || {
                        let calls = calls.clone();
                        async move {
                            if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                                return text_response(StatusCode::NOT_FOUND, "not ready");
                            }
                            json_response(StatusCode::OK, vector_receipt_envelope())
                        }
                    }
                }),
            )
            .route(
                "/v1/aci/sessions/{id}",
                get({
                    let calls = session_calls.clone();
                    move || {
                        let calls = calls.clone();
                        async move {
                            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                                return text_response(StatusCode::SERVICE_UNAVAILABLE, "not ready");
                            }
                            Response::builder()
                                .header("content-type", "application/json")
                                .body(Body::from(vector_session_bytes()))
                                .unwrap()
                        }
                    }
                }),
            );
        let base = spawn_server(upstream).await;
        let (tx, _outcomes) = mpsc::unbounded_channel();
        let state = state_over(base, tx);
        let exchange = RecordedExchange {
            receipt_id: "rcpt-0001".to_string(),
            path: "/v1/chat/completions".to_string(),
            status: 200,
            streamed: true,
            request: BodyDigest::of(REQUEST_BODY),
            response: BodyDigest::of(RESPONSE_BODY),
            delivery: ResponseDelivery::Complete,
            pinned_sessions: Vec::new(),
            at: 1,
            verified: None,
            context: None,
            local_policy_applied: false,
        };

        let (transcript, _) = verify_exchange(&state, &state.snapshot(), &exchange, None)
            .await
            .unwrap();

        assert!(transcript.verified());
        assert_eq!(receipt_calls.load(Ordering::SeqCst), 3);
        assert_eq!(session_calls.load(Ordering::SeqCst), 2);
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
        let session_bytes = private_ai_proxy::aci::digest::jcs_bytes(&record).unwrap();
        let current_id = private_ai_proxy::aci::digest::sha256_bare_hex(&session_bytes);
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
                "/v1/aci/sessions/{id}",
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
            Arc::new(|_| {}),
            tokio_util::sync::CancellationToken::new(),
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
        assert_eq!(outcome.verified, None);
        assert!(
            outcome.detail.contains("audit pending"),
            "{}",
            outcome.detail
        );
        // The refreshed set replaced the stale pin.
        assert_eq!(*state.policy_pins.lock().unwrap(), vec![current_id]);
    }

    async fn spawn_server(app: Router) -> String {
        private_ai_proxy::install_crypto_provider();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// Hermetic end-to-end: a mock upstream serving fixture artifacts, the
    /// proxy in front of it. Asserts byte-exact passthrough, that any POST
    /// path gets its exchange verified with the request's transient bearer
    /// (an Anthropic-style `/v1/messages` included), and that a receiptless
    /// 2xx POST fails loudly.
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
                "/v1/aci/receipts/{id}",
                get(|headers: HeaderMap| async move {
                    assert_eq!(
                        header_str(&headers, "authorization"),
                        Some("Bearer test-key")
                    );
                    Json(vector_receipt_envelope())
                }),
            )
            .route(
                // Sessions are served as their exact sealed bytes (§8).
                "/v1/aci/sessions/{id}",
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
        drop(state);

        let http = reqwest::Client::new();

        // Inference forward: byte-exact passthrough + receipt header surfaced;
        // the exchange is verified after the response completes.
        let resp = http
            .post(format!("{proxy}/v1/chat/completions"))
            .header("content-type", "application/json")
            .bearer_auth("test-key")
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
        assert_eq!(outcome.receipt_id.as_deref(), Some("rcpt-0001"));
        assert_eq!(outcome.verified, None);
        let audited = rx.recv().await.expect("inference audit reported");
        assert_eq!(audited.path, "/v1/chat/completions");
        assert_eq!(audited.verified, Some(true));

        // Any POST path is inference-capable: an Anthropic-style /v1/messages
        // forward is recorded the same way without being enumerated.
        let resp = http
            .post(format!("{proxy}/v1/messages"))
            .header("content-type", "application/json")
            .bearer_auth("test-key")
            .body(REQUEST_BODY.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(resp.bytes().await.unwrap().as_ref(), RESPONSE_BODY);
        let outcome = rx.recv().await.expect("messages outcome reported");
        assert_eq!(outcome.path, "/v1/messages");
        assert_eq!(outcome.verified, None);
        let audited = rx.recv().await.expect("messages audit reported");
        assert_eq!(audited.path, "/v1/messages");
        assert_eq!(audited.verified, Some(true));

        // A 2xx POST response with no receipt header fails loudly (spec 5.2).
        let resp = http
            .post(format!("{proxy}/v1/responses"))
            .header("content-type", "application/json")
            .body(REQUEST_BODY.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        assert!(resp.text().await.unwrap().contains("\"ok\":true"));
        let outcome = rx.recv().await.expect("responses outcome reported");
        assert_eq!(outcome.verified, Some(false));
        assert!(
            outcome.detail.contains("no X-Receipt-Id"),
            "{}",
            outcome.detail
        );

        // GET passthrough routes and reports without a receipt check.
        let models = http.get(format!("{proxy}/v1/models")).send().await.unwrap();
        assert_eq!(models.status().as_u16(), 200);
        let models_outcome = rx.recv().await.expect("models outcome reported");
        assert_eq!(models_outcome.method, Method::GET);
        assert_eq!(models_outcome.verified, None);
    }

    #[tokio::test]
    async fn client_drop_does_not_cancel_background_receipt_audit() {
        let split = RESPONSE_BODY.len() / 2;
        let finish_stream = Arc::new(tokio::sync::Notify::new());
        let upstream = Router::new()
            .route("/v1/chat/completions", post({
                let finish = finish_stream.clone();
                move || {
                    let finish = finish.clone();
                    async move {
                        let body = Body::from_stream(async_stream::stream! {
                            yield Ok::<_, std::io::Error>(Bytes::from_static(&RESPONSE_BODY[..split]));
                            finish.notified().await;
                            yield Ok::<_, std::io::Error>(Bytes::from_static(&RESPONSE_BODY[split..]));
                        });
                        Response::builder()
                            .header("content-type", "text/event-stream")
                            .header("x-receipt-id", "rcpt-0001")
                            .body(body)
                            .unwrap()
                    }
                }
            }))
            .route(
                "/v1/aci/receipts/{id}",
                get(|| async { json_response(StatusCode::OK, vector_receipt_envelope()) }),
            )
            .route("/v1/aci/sessions/{id}", get(|| async {
                ([(("content-type"), "application/json")], vector_session_bytes())
            }));
        let (tx, mut outcomes) = mpsc::unbounded_channel();
        let state = state_over(spawn_server(upstream).await, tx);
        let proxy = spawn_server(build_proxy_router(state)).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy}/v1/chat/completions"))
            .body(REQUEST_BODY.to_vec())
            .send()
            .await
            .unwrap();
        let mut body = response.bytes_stream();
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), body.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(first.as_ref(), &RESPONSE_BODY[..split]);
        drop(body);

        finish_stream.notify_one();
        let pending = tokio::time::timeout(std::time::Duration::from_secs(2), outcomes.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pending.verified, None);
        let audited = tokio::time::timeout(std::time::Duration::from_secs(5), outcomes.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(audited.verified, Some(true));
    }

    #[tokio::test]
    async fn streaming_audit_does_not_gate_delivery_on_receipt_success() {
        use futures_util::StreamExt;
        for mode in ["valid", "tampered", "unavailable"] {
            let response_bytes: &'static [u8] = if mode == "valid" {
                RESPONSE_BODY
            } else {
                b"data: first\n\ndata: [DONE]\n\n"
            };
            let split = response_bytes.len() / 2;
            let finish_stream = Arc::new(tokio::sync::Notify::new());
            let audit_started = Arc::new(tokio::sync::Notify::new());
            let finish_audit = Arc::new(tokio::sync::Notify::new());
            let upstream = Router::new()
                .route("/v1/chat/completions", post({
                    let finish = finish_stream.clone();
                    move || {
                        let finish = finish.clone();
                        async move {
                            let body = Body::from_stream(async_stream::stream! {
                                yield Ok::<_, std::io::Error>(Bytes::from_static(&response_bytes[..split]));
                                finish.notified().await;
                                yield Ok::<_, std::io::Error>(Bytes::from_static(&response_bytes[split..]));
                            });
                            Response::builder().header("content-type", "text/event-stream")
                                .header("x-receipt-id", "rcpt-0001").body(body).unwrap()
                        }
                    }
                }))
                .route("/v1/aci/receipts/{id}", get({
                    let started = audit_started.clone();
                    let finish = finish_audit.clone();
                    move || {
                        let started = started.clone();
                        let finish = finish.clone();
                        async move {
                            started.notify_one();
                            if mode == "unavailable" {
                                return text_response(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
                            }
                            finish.notified().await;
                            json_response(StatusCode::OK, vector_receipt_envelope())
                        }
                    }
                }))
                .route("/v1/aci/sessions/{id}", get(|| async {
                    ([("content-type", "application/json")], vector_session_bytes())
                }));
            let (tx, mut outcomes) = mpsc::unbounded_channel();
            let state = state_over(spawn_server(upstream).await, tx);
            let proxy = spawn_server(build_proxy_router(state)).await;
            let response = reqwest::Client::new()
                .post(format!("{proxy}/v1/chat/completions"))
                .body(REQUEST_BODY.to_vec())
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let mut stream = response.bytes_stream();
            let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(first.as_ref(), &response_bytes[..split]);
            assert!(outcomes.try_recv().is_err());
            finish_stream.notify_one();
            let remaining = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                let mut bytes = Vec::new();
                while let Some(chunk) = stream.next().await {
                    bytes.extend_from_slice(&chunk.unwrap());
                }
                bytes
            })
            .await
            .unwrap();
            assert_eq!(remaining, &response_bytes[split..]);
            tokio::time::timeout(std::time::Duration::from_secs(2), audit_started.notified())
                .await
                .unwrap();
            assert_eq!(outcomes.recv().await.unwrap().verified, None);
            finish_audit.notify_one();
            let audited = tokio::time::timeout(std::time::Duration::from_secs(5), outcomes.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(audited.status, 200);
            assert_eq!(
                audited.verified,
                match mode {
                    "valid" => Some(true),
                    "tampered" => Some(false),
                    _ => None,
                }
            );
        }
    }

    async fn terminated(receiver: &mut mpsc::UnboundedReceiver<VerifierEvent>) -> Option<String> {
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
                .await
                .expect("verifier termination event timed out")
                .expect("verifier event channel closed");
            if let VerifierEvent::Terminated { error } = event {
                return error;
            }
        }
    }

    fn test_launcher_events() -> (VerifierEventSink, mpsc::UnboundedReceiver<VerifierEvent>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Arc::new(move |event| {
                let _ = sender.send(event);
            }),
            receiver,
        )
    }

    #[tokio::test]
    async fn in_process_launcher_reports_panics_as_failures() {
        let launcher = InProcessVerifierLauncher::new(tokio::runtime::Handle::current());
        let (events, mut received) = test_launcher_events();
        let _task = launcher.spawn_task(events, |_| async move {
            panic!("synthetic verifier panic");
            #[allow(unreachable_code)]
            Ok(())
        });

        assert!(terminated(&mut received).await.is_some());
    }

    #[tokio::test]
    async fn in_process_launcher_explicit_stop_is_clean() {
        let launcher = InProcessVerifierLauncher::new(tokio::runtime::Handle::current());
        let (events, mut received) = test_launcher_events();
        let mut task = launcher.spawn_task(events, |cancelled| async move {
            cancelled.cancelled().await;
            Ok(())
        });

        task.stop().unwrap();
        assert_eq!(terminated(&mut received).await, None);
    }

    #[tokio::test]
    async fn dropping_in_process_launcher_task_cancels_it_cleanly() {
        let launcher = InProcessVerifierLauncher::new(tokio::runtime::Handle::current());
        let (events, mut received) = test_launcher_events();
        let task = launcher.spawn_task(events, |cancelled| async move {
            cancelled.cancelled().await;
            Ok(())
        });

        drop(task);
        assert_eq!(terminated(&mut received).await, None);
    }

    fn proxy_event(generation: u64, request_id: &str) -> ProxyEvent {
        ProxyEvent {
            generation,
            request_id: request_id.to_string(),
            session_id: "session-1".to_string(),
            agent: None,
            method: "POST".to_string(),
            path: "/v1/responses".to_string(),
            model: None,
            status: 200,
            streamed: false,
            receipt_id: None,
            verified: None,
            detail: String::new(),
            at: 1,
            local_policy_applied: None,
            rewritten: None,
            left_device: true,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost_usd: None,
        }
    }

    #[tokio::test]
    async fn managed_verdict_waits_for_a_full_queue_and_keeps_attribution() {
        let (events, mut received) = mpsc::channel(1);
        events.send(proxy_event(0, "queue-filler")).await.unwrap();
        let reporter = managed_reporter(events);
        reporter(RequestOutcome {
            method: Method::POST,
            path: "/v1/responses".to_string(),
            status: 200,
            streamed: true,
            receipt_id: Some("rcpt-1".to_string()),
            verified: Some(false),
            detail: "receipt failed".to_string(),
            context: Some(ForwardContext {
                generation: 9,
                request_id: "request-9".to_string(),
                session_id: "session-9".to_string(),
                agent: "codex".to_string(),
                model: Some("test-model".to_string()),
                at: 42,
            }),
            rewritten: Some(false),
            local_policy_applied: true,
        });

        assert_eq!(received.recv().await.unwrap().request_id, "queue-filler");
        let verdict = tokio::time::timeout(std::time::Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(verdict.generation, 9);
        assert_eq!(verdict.request_id, "request-9");
        assert_eq!(verdict.session_id, "session-9");
        assert_eq!(verdict.agent.as_deref(), Some("codex"));
        assert_eq!(verdict.model.as_deref(), Some("test-model"));
        assert_eq!(verdict.verified, Some(false));
    }

    #[tokio::test]
    async fn stopping_during_post_stream_does_not_report_a_failed_verdict() {
        let finish = Arc::new(tokio::sync::Notify::new());
        let upstream_finish = finish.clone();
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let finish = upstream_finish.clone();
                async move {
                    let body = Body::from_stream(async_stream::stream! {
                        yield Ok::<_, std::io::Error>(Bytes::from_static(b"first"));
                        finish.notified().await;
                        yield Ok::<_, std::io::Error>(Bytes::from_static(b"second"));
                    });
                    Response::builder()
                        .header("x-receipt-id", "rcpt-stopped")
                        .body(body)
                        .unwrap()
                }
            }),
        );
        let (sender, mut outcomes) = mpsc::unbounded_channel();
        let state = state_over(spawn_server(upstream).await, sender);
        let proxy = spawn_server(build_proxy_router(state.clone())).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy}/v1/chat/completions"))
            .body(REQUEST_BODY.to_vec())
            .send()
            .await
            .unwrap();
        let mut body = response.bytes_stream();
        assert_eq!(body.next().await.unwrap().unwrap(), "first");

        state.shutdown.cancel();
        finish.notify_waiters();
        let _ = body.next().await;
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), outcomes.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome.receipt_id.as_deref(), Some("rcpt-stopped"));
        assert_eq!(outcome.verified, None);
        assert!(outcome.detail.contains("protection stopped"));
        assert!(outcomes.try_recv().is_err());
    }
}
