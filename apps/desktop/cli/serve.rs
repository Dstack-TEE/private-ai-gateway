//! `private-ai-proxy serve`: a local verifying proxy that fails closed on the attested
//! service.
//!
//! Startup verifies `<base-url>` (spec 9.1) and refuses to listen unless
//! the verdict is VERIFIED. The proxy exposes a plaintext local API, rejects
//! E2EE request headers, and forwards accepted traffic over the SPKI-pinned
//! channel. Receipt auditing never delays response delivery.
//! Only digests and verdicts are retained after the request; bodies never go to disk.
//! No bodies are logged.
//!
//! A keyset rotation normally surfaces through the X-ACI-Keyset-Digest header
//! on a response (§3.4). When the service rotates its TLS key, the handshake
//! fails closed against the pin before any response exists, so a send the pin
//! refused is treated as the same rotation: block, re-verify, and re-pin.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::aci::types::{AttestationReport, PROVIDER_ACI_SESSION_IDS, PROVIDER_ACI_VERIFIED};
use crate::capture::{tee, CompletionHook, StreamEnd};
use agent_bridge::proxy::{hop_by_hop_names, ForwardContext, MAX_BODY_BYTES};
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use axum::Router;
use desktop_runtime::verifier_session::{IdentityEvent, VerifierEvent, VerifierEventSink};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::aci::tls::is_pin_mismatch;
use crate::args::ServeArgs;
use crate::checks::{BodyDigest, EstablishedIdentity, RequiredClaim};
use crate::client::AciClient;
use crate::sessions::audit_current_sessions;
use crate::verify::{verify_service, ServiceVerification};

mod audit;
mod control;
pub mod managed;
mod report;
#[cfg(test)]
mod tests;

use audit::audit_exchange;
use control::build_control_router;
use report::{
    default_reporter, identity_event, json_event_sink, json_reporter, lifecycle_json,
    write_json_event,
};

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
    /// Delivery gate for this identity, read in the same snapshot as the
    /// identity itself. Cancelled when the identity is blocked (a rotation,
    /// expiry, or refused pin observed under it) or replaced, so a request
    /// that passed the entry checks is refused rather than sent under a
    /// decision made for an identity no longer trusted. The proxy is blocked
    /// exactly while the current identity's gate is cancelled.
    delivery: CancellationToken,
}

impl TrustedIdentity {
    fn new(
        report: AttestationReport,
        identity: EstablishedIdentity,
        delivery: CancellationToken,
    ) -> Self {
        Self {
            keyset_digest: report.workload_keyset_digest.clone(),
            report: Arc::new(report),
            not_after: identity.keyset.not_after,
            identity: Arc::new(identity),
            delivery,
        }
    }
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
    /// Serializes the re-verify so a burst of blocked requests reverifies
    /// once, and holds the latest attempt's outcome for the requests that
    /// waited on it.
    reverify: tokio::sync::Mutex<Result<(), String>>,
    /// Completed re-verify attempts. A caller that saw an attempt finish while
    /// it waited shares that attempt's failure instead of attesting again.
    reverify_attempts: AtomicU64,
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
    /// lifetime. Every identity's delivery gate is a child of it.
    shutdown: CancellationToken,
    #[cfg(test)]
    pause: Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
    event_sink: VerifierEventSink,
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
        event_sink: VerifierEventSink,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            client,
            base_url,
            host,
            enforce_verified,
            accepted_composes,
            require_production_os,
            audits: Arc::new(tokio::sync::Semaphore::new(16)),
            trusted: Mutex::new(TrustedIdentity::new(
                report,
                identity,
                shutdown.child_token(),
            )),
            reverify: tokio::sync::Mutex::new(Ok(())),
            reverify_attempts: AtomicU64::new(0),
            recorded: Mutex::new(VecDeque::new()),
            fixed_pins,
            required_claims,
            policy_pins: Mutex::new(Vec::new()),
            reporter,
            event_sink,
            shutdown,
            #[cfg(test)]
            pause: Mutex::new(None),
        }
    }

    fn record(&self, exchange: RecordedExchange) {
        let mut recorded = self.recorded.lock().expect("recorded ring poisoned");
        if recorded.len() == RECORDED_CAP {
            recorded.pop_front();
        }
        recorded.push_back(exchange);
    }

    /// Trust a freshly verified identity under a new delivery gate, closing
    /// the previous identity's gate in the same critical section: under the
    /// trusted lock, a gate still open is the current identity's.
    fn adopt(&self, report: AttestationReport, identity: EstablishedIdentity) {
        let adopted = TrustedIdentity::new(report, identity, self.shutdown.child_token());
        let mut trusted = self.trusted.lock().expect("trusted identity poisoned");
        std::mem::replace(&mut *trusted, adopted).delivery.cancel();
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

    /// Whether forwards wait for a fresh verify: the current identity's
    /// gate was cancelled by a block observed under it (or by shutdown).
    fn is_blocked(&self) -> bool {
        self.snapshot().delivery.is_cancelled()
    }

    /// Block forwards until a fresh verify re-establishes trust: cancel the
    /// gate of the identity the change was `observed` under and report the
    /// rotation (the managed backend rebuilds its session on
    /// `keyset_changed`). Only the block that closes the current identity's
    /// gate reports it: an observation made under an identity since
    /// replaced, or one already blocked, only cancels a closed gate.
    fn block_for_keyset_change(&self, observed: &TrustedIdentity, reason: String) {
        let blocks_current = {
            // Serialized with `adopt`, which closes a replaced identity's
            // gate under this lock: an open gate here is the current one's.
            let _trusted = self.trusted.lock().expect("trusted identity poisoned");
            let open = !observed.delivery.is_cancelled();
            observed.delivery.cancel();
            open
        };
        if blocks_current {
            (self.event_sink)(VerifierEvent::Blocked {
                code: Some("keyset_changed".to_string()),
                reason,
            });
        }
    }

    /// When blocked by a keyset change, re-verify the service once and, on
    /// success, re-pin the TLS key and adopt the new identity. Returns `Ok`
    /// when forwarding may proceed.
    async fn ensure_unblocked(self: &Arc<Self>) -> Result<(), String> {
        if !self.is_blocked() {
            return Ok(());
        }
        let attempts = self.reverify_attempts.load(Ordering::SeqCst);
        self.reverify_blocked(attempts, None).await
    }

    /// The single re-verify funnel. `attempts` is the count the caller read
    /// before it started waiting: an attempt that failed since is shared, not
    /// repeated (single flight). Only the caller that ran a failed attempt
    /// reports it, and not once the verifier is stopping: a managed backend
    /// stops this verifier on `keyset_changed` to rebuild the session, and
    /// that expected stop is not a verification failure.
    ///
    /// `refused_pins` heals a stale TLS pin: a send whose handshake that pin
    /// set rejected blocks like a keyset rotation (the TLS key is part of the
    /// attested keyset). A key is only ever adopted from a VERIFIED report;
    /// a failed verify keeps the old pin.
    async fn reverify_blocked(
        self: &Arc<Self>,
        attempts: u64,
        refused_pins: Option<&[String]>,
    ) -> Result<(), String> {
        let mut last = self.reverify.lock().await;
        // Decided under the lock, where no identity can be adopted: when
        // another request's heal already replaced the refused pin, this one
        // only needs to send again.
        if let Some(refused) = refused_pins {
            let current = self.snapshot();
            if !current.delivery.is_cancelled() && self.client.pinned_spkis(&self.host) == refused {
                self.block_for_keyset_change(
                    &current,
                    "upstream TLS key is not in the attested pin set; re-verification required"
                        .to_string(),
                );
            }
        }
        if !self.is_blocked() {
            return Ok(());
        }
        if self.reverify_attempts.load(Ordering::SeqCst) != attempts {
            if let Err(reason) = &*last {
                return Err(reason.clone());
            }
        }
        *last = self.reverify().await;
        self.reverify_attempts.fetch_add(1, Ordering::SeqCst);
        let result = last.clone();
        drop(last);
        if let Err(reason) = &result {
            if !self.shutdown.is_cancelled() {
                (self.event_sink)(VerifierEvent::Blocked {
                    code: None,
                    reason: reason.clone(),
                });
            }
        }
        result
    }

    async fn reverify(self: &Arc<Self>) -> Result<(), String> {
        let verification = tokio::select! {
            biased;
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
        let stale_pins = self.client.pinned_spkis(&self.host);
        let pins = verification.attested_spkis();
        let verification_summary = verification.transcript.to_json(false);
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
        // Re-pin last among the fallible steps, so a failed re-verify never
        // leaves the new key pinned under the old identity.
        self.client.pin(&self.host, &pins)?;
        self.adopt(verification.report, identity);
        (self.event_sink)(identity_event);
        let current_pins = self.client.pinned_spkis(&self.host);
        if !stale_pins.is_empty() && stale_pins != current_pins {
            tracing::info!(
                "private-ai-proxy serve: re-verified after keyset change; TLS pin {} -> {}; \
                 resuming forwards",
                stale_pins.join(","),
                current_pins.join(",")
            );
        } else {
            tracing::info!(
                "private-ai-proxy serve: re-verified after keyset change; resuming forwards"
            );
        }
        Ok(())
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

/// Clear of the web UI (4182), which a desktop installation may bind at the
/// same time.
const DEFAULT_CONTROL: &str = "127.0.0.1:4183";
/// Connections each listener holds open at once; later ones wait to be accepted.
const MAX_CONNECTIONS: usize = 128;

async fn run_inner(args: ServeArgs, require_production_os: bool) -> Result<i32, String> {
    let reporter: Reporter = if args.json_events {
        Arc::new(json_reporter)
    } else {
        Arc::new(default_reporter)
    };
    let event_sink: VerifierEventSink = if args.json_events {
        Arc::new(json_event_sink)
    } else {
        Arc::new(|_| {})
    };
    let (state, ready_identity, base_url) = initialize(
        VerifierOptions {
            base_url: args.base_url.clone(),
            accepted_composes: args.accepted_composes.clone(),
            require_production_os,
            enforce_verified: !args.allow_unverified,
            fixed_pins: args.sessions.clone(),
            required_claims: args.require_claims.clone(),
            print_progress: !args.json_events,
        },
        reporter,
        event_sink,
        CancellationToken::new(),
    )
    .await?;

    let listen = args.listen.as_deref().unwrap_or("127.0.0.1:4180");
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| format!("cannot bind {listen}: {e}"))?;
    let local = listener
        .local_addr()
        .map_err(|e| format!("cannot read listen address: {e}"))?;
    let control = args.control.as_deref().unwrap_or(DEFAULT_CONTROL);
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

    // Both serve until the process is interrupted.
    let (control, proxy) = (
        build_control_router(state.clone()),
        build_proxy_router(state),
    );
    tokio::join!(
        async {
            desktop_core::serve::serve(
                control_listener,
                control,
                MAX_CONNECTIONS,
                std::future::pending(),
            )
            .await
            .await
        },
        async {
            desktop_core::serve::serve(listener, proxy, MAX_CONNECTIONS, std::future::pending())
                .await
                .await
        },
    );
    Ok(0)
}

/// The verification policy one proxy state is built from: standalone `serve`
/// derives it from its arguments, the managed backend from its verifier config.
struct VerifierOptions {
    base_url: String,
    /// Compose hashes accepted on the startup verify and every re-verify (§1.3).
    accepted_composes: Vec<String>,
    require_production_os: bool,
    /// Demand verified attested-session serving on every inference (§5.3).
    enforce_verified: bool,
    /// `--session`: a fixed §5.3 accepted set.
    fixed_pins: Vec<String>,
    /// `--require-claim`: the §9.2(3) policy that derives the pin set.
    required_claims: Vec<RequiredClaim>,
    /// Print the startup transcript and pinned sessions for a human reader.
    print_progress: bool,
}

async fn initialize(
    options: VerifierOptions,
    reporter: Reporter,
    event_sink: VerifierEventSink,
    shutdown: CancellationToken,
) -> Result<(Arc<ProxyState>, IdentityEvent, String), String> {
    let verification = verify_service(
        &options.base_url,
        None,
        &options.accepted_composes,
        options.require_production_os,
        false,
    )
    .await?;
    if options.print_progress {
        println!("== service verification: {} ==", verification.base_url);
        print!("{}", verification.transcript.render_human(false));
    }
    if !verification.transcript.verified() {
        return Err(
            "service verification failed; refusing to start the proxy (fail closed)".to_string(),
        );
    }

    let verification_summary = verification.transcript.to_json(false);
    let pins = verification.attested_spkis();
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
    // Pin the just-verified TLS keys on every future hop to this host.
    client.pin(&host, &pins)?;
    let state = ProxyState::new(
        client,
        base_url.clone(),
        host,
        options.enforce_verified,
        options.accepted_composes,
        options.require_production_os,
        options.fixed_pins,
        options.required_claims,
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
        if options.print_progress {
            println!("policy-accepted sessions pinned ({}):", pins.len());
            for pin in &pins {
                println!("  {pin}");
            }
        }
        *state.policy_pins.lock().expect("policy pins poisoned") = pins;
    }

    Ok((state, ready_identity, base_url))
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
    let observed = state.snapshot();
    proxy_observed(state, observed, method, uri, headers, body, context).await
}

/// One request arriving while `observed` was the trusted identity. Every
/// method re-checks verification (§3.4): an expired or rotated keyset blocks
/// GET passthrough exactly like inference, and an expiry seen in `observed`
/// closes only that identity's gate. A re-verification that changed the
/// service identity must not carry this request either: it was admitted
/// upstream against the old identity, so it is refused with a retryable
/// status until the desktop publishes the new identity.
async fn proxy_observed(
    state: Arc<ProxyState>,
    observed: TrustedIdentity,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    context: Option<ForwardContext>,
) -> Response {
    let path = uri.path().to_string();
    if desktop_core::now_secs() >= observed.not_after {
        observed.delivery.cancel();
    }
    let identity_before = observed.keyset_digest;
    if let Err(reason) = state.ensure_unblocked().await {
        // A managed verifier stopping is retryable, not a failed verification.
        if state.shutdown.is_cancelled() {
            return delivery_revoked_response(&path);
        }
        tracing::warn!("!! {method} {path} -> 503 blocked: {reason}");
        return text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "upstream keyset changed or expired and re-verification failed; refusing to forward\n",
        );
    }
    // The identity this request is admitted under, and its delivery gate,
    // from one snapshot.
    let admitted = state.snapshot();
    if admitted.keyset_digest != identity_before {
        return identity_changed_response(&method, &path);
    }
    if method == Method::POST {
        proxy_inference(state, admitted, uri, headers, body, context).await
    } else {
        proxy_passthrough(state, admitted, method, uri, headers, body, context).await
    }
}

/// Non-POST passthrough: streamed byte-exact, no receipt to check.
async fn proxy_passthrough(
    state: Arc<ProxyState>,
    mut admitted: TrustedIdentity,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    context: Option<ForwardContext>,
) -> Response {
    let path = uri.path().to_string();
    let url = join_url(&state.base_url, &uri);
    let send = || {
        let req = forward_headers(state.client.request(method.clone(), &url), &headers);
        if body.is_empty() {
            req.send()
        } else {
            req.body(body.to_vec()).send()
        }
    };
    let mut resp =
        match send_upstream(&state, &mut admitted, (&method, &path, &context), send).await {
            Ok(resp) => resp,
            Err(response) => return *response,
        };
    let status = resp.status().as_u16();
    let resp_headers = resp.headers().clone();
    rotation_gate(&state, &admitted, &resp_headers);
    let delivery = admitted.delivery;
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
    mut trusted: TrustedIdentity,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    context: Option<ForwardContext>,
) -> Response {
    let path = uri.path().to_string();

    if has_e2ee_request_headers(&headers) {
        tracing::warn!("!! POST {path} -> 400 E2EE request rejected by plaintext local API");
        return text_response(
            StatusCode::BAD_REQUEST,
            "private-ai-proxy serve accepts plaintext requests only; remove E2EE request headers\n",
        );
    }

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
                tracing::warn!("!! POST {path} -> 400: {reason}");
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
    let mut resp = match send_upstream(
        &state,
        &mut trusted,
        (&Method::POST, &path, &context),
        || send(request_body.clone()),
    )
    .await
    {
        Ok(resp) => resp,
        Err(response) => return *response,
    };
    let delivery = trusted.delivery.clone();
    // A 412 refusal against a policy-derived pin set means the sessions
    // rotated under us (§8 supersession): refresh the set from the service's
    // current sessions and retry once. §5.3 refuses before serving, so
    // nothing ran on the refused attempt. A user-fixed --session list is
    // never refreshed; its 412 surfaces as-is.
    if resp.status().as_u16() == 412 && injected_policy_pins {
        match derive_policy_pins(&state).await {
            Ok(pins) if !pins.is_empty() && pins != active_pins => {
                tracing::warn!(
                    "private-ai-proxy serve: pinned sessions refused (412); policy re-accepted {} current \
                     session(s), retrying",
                    pins.len()
                );
                *state.policy_pins.lock().expect("policy pins poisoned") = pins.clone();
                request_body = match apply_constraints(body.to_vec(), state.enforce_verified, &pins)
                {
                    Ok(body) => body,
                    Err(reason) => {
                        tracing::warn!("private-ai-proxy serve: refreshed session policy rejected request: {reason}");
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
                tracing::warn!("private-ai-proxy serve: policy pin refresh after 412 failed: {e}")
            }
        }
    }
    let status = resp.status().as_u16();
    let resp_headers = resp.headers().clone();
    rotation_gate(&state, &trusted, &resp_headers);

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
                at: desktop_core::now_secs(),
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

/// One upstream send under the admitted identity's delivery gate. A
/// handshake the TLS pin refused can never reach the §3.4 rotation gate (it
/// aborts before any response), so that refusal is healed through the
/// keyset-rotation path. Only this request's own pin refusal qualifies; it
/// happens before any request byte is written, so a request that may already
/// have reached the service is never replayed. Afterwards the request is sent
/// once more only if the identity it was admitted under still holds; a
/// changed identity gets the same retryable 503 as `proxy_request`. Any other
/// failure (DNS, refused port, timeout, a dropped response) stays a 502.
async fn send_upstream<Fut>(
    state: &Arc<ProxyState>,
    admitted: &mut TrustedIdentity,
    (method, path, context): (&Method, &str, &Option<ForwardContext>),
    send: impl Fn() -> Fut,
) -> Result<reqwest::Response, Box<Response>>
where
    Fut: std::future::Future<Output = reqwest::Result<reqwest::Response>>,
{
    let failed = |e| {
        Box::new(send_error(
            state,
            method.clone(),
            path.to_string(),
            e,
            context.clone(),
        ))
    };
    let pins = state.client.pinned_spkis(&state.host);
    let attempts = state.reverify_attempts.load(Ordering::SeqCst);
    let error = match race_delivery(&admitted.delivery, send()).await {
        None => return Err(Box::new(delivery_revoked_response(path))),
        Some(Ok(resp)) => return Ok(resp),
        Some(Err(e)) => e,
    };
    if !is_pin_mismatch(&error) {
        return Err(failed(error));
    }
    if let Err(reason) = state.reverify_blocked(attempts, Some(&pins)).await {
        // A managed verifier stops on `keyset_changed` while the backend
        // rebuilds its session; the request is retryable, not failed.
        if state.shutdown.is_cancelled() {
            return Err(Box::new(delivery_revoked_response(path)));
        }
        tracing::warn!("!! {method} {path} -> 502 stale TLS pin; re-verification failed: {reason}");
        return Err(failed(error));
    }
    let current = state.snapshot();
    if current.keyset_digest != admitted.keyset_digest {
        return Err(Box::new(identity_changed_response(method, path)));
    }
    // A heal (this request's or another's) re-adopted the same identity under
    // a new delivery gate; send under that one.
    *admitted = current;
    match race_delivery(&admitted.delivery, send()).await {
        None => Err(Box::new(delivery_revoked_response(path))),
        Some(Ok(resp)) => Ok(resp),
        Some(Err(e)) => Err(failed(e)),
    }
}

/// A re-verification replaced the identity this request was admitted under:
/// refuse it with a retryable status until the new identity is published.
fn identity_changed_response(method: &Method, path: &str) -> Response {
    tracing::warn!("!! {method} {path} -> 503 identity changed during re-verification; retry");
    text_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "service identity changed during re-verification; retry once the gateway is verified again\n",
    )
}

/// Race an upstream send against the delivery gate; `None` means revoked
/// before (or while) sending, and nothing may be treated as delivered.
async fn race_delivery<T>(
    token: &CancellationToken,
    send: impl std::future::Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        _ = token.cancelled() => None,
        result = send => Some(result),
    }
}

fn delivery_revoked_response(path: &str) -> Response {
    tracing::warn!("!! {path} -> 503 verification changed before the request was sent");
    text_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "verification changed before the request was sent; retry once the gateway is verified again\n",
    )
}

/// Keyset-rotation gate (§3.4): a response advertising a digest other than
/// the trusted one blocks further inference forwards until a fresh verify
/// re-establishes trust.
fn rotation_gate(state: &ProxyState, admitted: &TrustedIdentity, headers: &HeaderMap) {
    let trusted_digest = &admitted.keyset_digest;
    if let Some(observed) = header_str(headers, "x-aci-keyset-digest") {
        if observed != trusted_digest {
            state.block_for_keyset_change(admitted, format!(
                "upstream keyset digest changed ({observed} != {trusted_digest}); re-verification required"
            ));
            tracing::warn!(
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
        tracing::warn!(
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
