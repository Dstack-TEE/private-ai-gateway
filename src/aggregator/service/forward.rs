use std::collections::{BTreeMap, HashSet};

use serde::Serialize;
use serde_json::Value;

use crate::aci::digest;
use crate::aci::receipt::{
    ChannelBinding, ReceiptBuilder, ReceiptError, SignedReceipt, UpstreamVerifiedEvent,
    VerificationResult, VerificationRound,
};
use crate::aci::upstream::{
    PreparedUpstreamRequest, UpstreamError, UpstreamRequest, UpstreamResponse,
    UpstreamStreamResponse,
};
use crate::aggregator::metrics::{RequestMode, StreamErrorKind};
use crate::aggregator::session::{
    AttestedSession, EvidenceRef, SessionClaims, SessionDocument, WorkloadIdentityRef,
    SESSION_API_VERSION,
};
use crate::aggregator::session_store::SessionLease;

use super::claims::{
    instance_id, is_multi_instance, per_instance_session_claims, session_claims_for_event,
};
use super::e2ee_crypto::encrypt_e2ee_response_body;
use super::e2ee_crypto::is_sse_content_type;
use super::helpers::{
    accepted_response_model, collect_upstream_body, extract_chat_id, generate_receipt_id,
};
use super::streaming::{E2eeSseTransformer, ReceiptFinalizingStream};
use super::{
    reviewed_session_material, AciService, ChatCompletionRequest, E2eeError, E2eeResponseInfo,
    ForwardResult, GatewayRequestContext, MiddlewareReceiptJournal, ReceiptOwner, ServiceError,
    StreamingForwardResult, StreamingForwardStream, StreamingUpstreamError,
    UpstreamVerificationError, UpstreamVerificationRequest, CHANNEL_BINDING_REVERIFY_ATTEMPTS,
    CHAT_COMPLETIONS_PATH,
};

/// Outcome of [`AciService::forward_with_binding_reverify`]. The caller maps
/// each non-success variant to its own policy — abort (single forward) or fail
/// over to the next candidate (middleware).
pub(super) enum ReverifyOutcome<R> {
    /// Forwarding succeeded, after zero or more transparent reverify rounds.
    Forwarded(R, Box<AdmittedUpstream>),
    /// A reverify (cached-event refresh) attempt itself failed.
    RefreshFailed(ServiceError),
    /// Forwarding failed: either a terminal channel-binding mismatch (after the
    /// verifier cache was invalidated per policy) or an unrelated upstream
    /// error. Both map to the caller's failure path; the helper has already
    /// applied any mismatch invalidation.
    Failed(UpstreamError),
}

/// A response with an upstream's status and no body: what an adapter that
/// withholds unverified bodies answered ([`UpstreamError::Upstream`]).
pub(super) trait WithheldResponse {
    fn withheld(status_code: u16) -> Self;
}

impl WithheldResponse for UpstreamResponse {
    fn withheld(status_code: u16) -> Self {
        Self {
            status_code,
            body: Vec::new(),
            headers: Default::default(),
            served_instance_id: None,
        }
    }
}

impl WithheldResponse for UpstreamStreamResponse {
    fn withheld(status_code: u16) -> Self {
        Self {
            status_code,
            headers: Default::default(),
            body: Box::pin(futures_util::stream::empty()),
            served_instance_id: None,
        }
    }
}

/// A complete logical channel, or one independently selectable Chutes or 0G instance.
pub(super) struct SessionGroup {
    instance_key: Option<String>,
    bindings: Vec<ChannelBinding>,
    pub(super) lease: SessionLease,
}

/// JCS ordering preserves every field and removes only exact duplicates.
fn canonical_bindings(bindings: &[ChannelBinding]) -> Result<Vec<ChannelBinding>, ServiceError> {
    let mut canonical = BTreeMap::new();
    for binding in bindings {
        let value =
            serde_json::to_value(binding).map_err(|e| ServiceError::SessionStore(e.to_string()))?;
        let bytes =
            digest::jcs_bytes(&value).map_err(|e| ServiceError::SessionStore(e.to_string()))?;
        canonical.insert(bytes, binding.clone());
    }
    Ok(canonical.into_values().collect())
}

/// Owned authorization for exactly one forwarding attempt.
pub(super) struct AdmittedUpstream {
    pub(super) event: UpstreamVerifiedEvent,
    pub(super) admitted_at: u64,
    sessions: Vec<SessionGroup>,
    requires_instance: bool,
}

/// The admitted session selected by the response. Sibling leases are released
/// at selection, before buffering or streaming ownership is handed off.
pub(super) struct SelectedUpstream {
    pub(super) event: UpstreamVerifiedEvent,
    pub(super) admitted_at: u64,
    pub(super) lease: Option<SessionLease>,
}

impl AdmittedUpstream {
    fn withheld_without_instance(mut self) -> Self {
        if self.requires_instance {
            self.sessions.clear();
            self.requires_instance = false;
            self.event.channel_bindings.clear();
            self.event.result = VerificationResult::Failed;
            self.event.reason = Some(
                "upstream withheld an unverified response without a serving instance".to_string(),
            );
        }
        self
    }

    pub(super) fn select(
        mut self,
        served_instance_id: Option<&str>,
    ) -> Result<SelectedUpstream, ServiceError> {
        let lease = if self.requires_instance {
            let id = served_instance_id.ok_or(ServiceError::AdmissionInvariant(
                "multi-instance response omitted its instance identifier",
            ))?;
            let index = self
                .sessions
                .iter()
                .position(|s| s.instance_key.as_deref() == Some(id))
                .ok_or(ServiceError::AdmissionInvariant(
                    "response instance was not admitted",
                ))?;
            Some(self.sessions.swap_remove(index).lease)
        } else {
            self.sessions.pop().map(|s| s.lease)
        };
        Ok(SelectedUpstream {
            event: self.event,
            admitted_at: self.admitted_at,
            lease,
        })
    }
}

/// Whether a route with this TEE classification may serve a request restricted
/// to ACI-verified upstreams. Every forwarding path applies this eligibility
/// check before running the fail-closed verifier gate.
///
/// Only `Some(true)` qualifies: an unclassified route is not *known* to be
/// attested, so no verifier (nor a caller-supplied event) can stand in for the
/// classification.
pub(super) fn attested_route_eligible(is_tee: Option<bool>) -> bool {
    is_tee == Some(true)
}

/// Local key over a channel's verified material — the dedup handle the hot
/// path uses to find "the current session for this channel" without sealing a
/// new document per request. Never served; carries no protocol meaning.
#[derive(Serialize)]
struct ChannelMaterial<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    schema: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_type: Option<&'a str>,
    upstream_name: &'a str,
    endpoint: &'a Option<String>,
    verifier_id: &'a str,
    identity: &'a Option<WorkloadIdentityRef>,
    channel_binding: &'a [ChannelBinding],
    claims: &'a SessionClaims,
    evidence_digest: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verification_round: Option<&'a VerificationRound>,
}

impl ChannelMaterial<'_> {
    fn fingerprint(&self) -> Result<String, serde_json::Error> {
        // JCS form: `claims.extra` holds provider JSON whose member order
        // follows the parse, so a plain serialization would fingerprint the
        // same material differently across rounds.
        let value = serde_json::to_value(self)?;
        let bytes = digest::jcs_bytes(&value).map_err(serde::ser::Error::custom)?;
        Ok(digest::sha256_hex(&bytes))
    }
}

impl AciService {
    pub async fn forward_chat_completion(
        &self,
        received_body: &[u8],
        forwarded_body: Option<Vec<u8>>,
        aci_required: bool,
        upstream_verification_event: Option<UpstreamVerifiedEvent>,
    ) -> Result<ForwardResult, ServiceError> {
        self.forward_chat_completion_request(ChatCompletionRequest {
            context: GatewayRequestContext::default(),
            endpoint_path: CHAT_COMPLETIONS_PATH,
            received_body,
            forwarded_body,
            aci_required,
            aci_session_ids: Vec::new(),
            upstream_verification_event,
            requester: None,
            e2ee: None,
        })
        .await
    }

    /// Rich variant of [`Self::forward_chat_completion`] that also takes
    /// the receipt owner so the receipt store can authenticate later
    /// lookups (ACI §7.6).
    pub async fn forward_chat_completion_request(
        &self,
        req: ChatCompletionRequest<'_>,
    ) -> Result<ForwardResult, ServiceError> {
        // §5.3: a direct service satisfies `aci_verified` by construction — the
        // workload the client verified (§9.1) is the one serving (§4.1); pinned
        // session lists are still refused in `admit_upstream`.
        let aci_required = req.requires_aci_verification() && !self.serves_directly();
        let received_body = req.received_body;
        let endpoint_path = req.endpoint_path;
        self.metrics.record_request(
            endpoint_path,
            RequestMode::Buffered,
            req.e2ee.as_ref().is_some(),
        );
        let target_route_id = req.context.target_route_id.clone();
        let backend_input_body = req.forwarded_body.unwrap_or_else(|| received_body.to_vec());
        let middleware_forwarded_body =
            target_route_id.as_ref().map(|_| backend_input_body.clone());
        let prepared = self.upstream.prepare(UpstreamRequest {
            body: backend_input_body,
            path: Some(endpoint_path.to_string()),
            target_route_id: target_route_id.clone(),
            ..Default::default()
        })?;
        if aci_required && !attested_route_eligible(prepared.is_tee) {
            return Err(ServiceError::UpstreamVerification(
                UpstreamVerificationError::NoEligibleAttestedRoute(
                    req.context.user_model.clone().unwrap_or_default(),
                ),
            ));
        }
        let forwarded_body = prepared.request.body.clone();
        let caller_supplied_upstream_event = req.upstream_verification_event.is_some();
        let verification_required = aci_required;
        let recorded_event = self
            .recorded_upstream_event(
                &prepared,
                verification_required,
                req.upstream_verification_event,
            )
            .await?;
        let admission = self.admit_upstream(
            recorded_event,
            &req.aci_session_ids,
            &prepared.model_id,
            None,
        )?;

        let (upstream_response, admission) = match self
            .forward_with_binding_reverify(
                &prepared,
                admission,
                verification_required,
                caller_supplied_upstream_event,
                &req.aci_session_ids,
                None,
                // Single forward: only flush the cache for an event we own.
                false,
                |prepared, event| async move {
                    self.upstream
                        .forward_verified_prepared(prepared, &event)
                        .await
                },
            )
            .await
        {
            ReverifyOutcome::Forwarded(response, admission) => (response, admission),
            ReverifyOutcome::RefreshFailed(err) => return Err(err),
            ReverifyOutcome::Failed(err) => return Err(err.into()),
        };
        let selected = (*admission).select(upstream_response.served_instance_id.as_deref())?;
        let response_model =
            accepted_response_model(upstream_response.status_code, &upstream_response.body);
        self.metrics.record_upstream_response(
            endpoint_path,
            RequestMode::Buffered,
            upstream_response.status_code,
            response_model.as_deref(),
        );

        // An upstream error reaches the client as the middleware and the
        // streaming path give it: the gateway's own envelope at a mapped status,
        // so the provider's wording and its auth statuses (which would read as
        // the caller's) never pass through, and a client image-URL fetch failure
        // becomes a 400. This is decided on the cleartext body here — before E2EE
        // encryption and before the receipt is built — so the receipt attests
        // exactly the body/status the client receives.
        let mut upstream_headers = upstream_response.headers;
        let (client_status, client_body) = if (200..300).contains(&upstream_response.status_code) {
            (
                upstream_response.status_code,
                upstream_response.body.clone(),
            )
        } else {
            // The envelope is JSON; don't let the client inherit a non-JSON
            // upstream content-type (some backends 5xx with text/*).
            upstream_headers.insert("content-type".to_string(), "application/json".to_string());
            crate::middleware::errors::normalize_upstream_error_parts(
                crate::middleware::errors::surface_for_path(endpoint_path),
                upstream_response.status_code,
                &upstream_response.body,
                received_body,
                None,
            )
        };

        let e2ee = req.e2ee.as_ref();
        let wire_response_body = match e2ee {
            Some(ctx) => encrypt_e2ee_response_body(&client_body, ctx, endpoint_path)?,
            None => client_body.clone(),
        };
        let e2ee_response = e2ee.as_ref().map(|ctx| E2eeResponseInfo {
            version: ctx.version.clone(),
            algo: ctx.algo.clone(),
        });

        // Receipt construction with bytes the service actually
        // observed. X-Request-Hash is never trusted here because we
        // do not even consult it; the byte source is the body the
        // service received from axum.
        let receipt_id = generate_receipt_id();
        let chat_id = extract_chat_id(&client_body);
        let served_at = selected.admitted_at;
        let keyset = self.current_keyset();
        let mut builder = ReceiptBuilder::new(
            receipt_id,
            chat_id,
            req.context.user_model.clone(),
            keyset.digest().to_string(),
            endpoint_path.to_string(),
            "POST".to_string(),
            served_at,
        );
        builder.add_request_received(received_body)?;
        if let Some(body) = middleware_forwarded_body.as_deref() {
            builder.add_middleware_forwarded(body)?;
        }
        if let Some(route_id) = target_route_id.as_deref() {
            builder.add_route_selected(route_id)?;
        }
        builder.add_request_forwarded(&forwarded_body)?;
        if !self.serves_directly() {
            let recorded = selected
                .lease
                .as_ref()
                .map(|lease| lease.session_id().to_string());
            Self::append_upstream_verified(&mut builder, &selected.event, recorded)?;
            // The session is keyed on the requested (routed) model; record the
            // exact upstream-served model in the receipt's event.
            builder.set_upstream_verified_model_id(response_model.clone());
        }
        // §7.4 wire bytes: whatever is actually served — after E2EE sealing
        // or an image-input 400 remap, when either applies.
        builder.add_response_returned(&wire_response_body)?;

        let receipt = builder.finalize(self.keys.as_ref(), &self.default_receipt_key_id)?;
        self.store_receipt(
            receipt.clone(),
            req.requester.clone(),
            selected.lease.as_ref(),
        )?;
        self.metrics.record_receipt_issued(
            endpoint_path,
            RequestMode::Buffered,
            response_model.as_deref(),
        );

        Ok(ForwardResult {
            receipt,
            upstream_status: client_status,
            upstream_body: wire_response_body,
            upstream_headers,
            e2ee: e2ee_response,
        })
    }

    /// Forward a middleware-selected request without finalizing the receipt.
    ///
    /// The backend records trust-critical provider facts into the returned
    /// draft. The public frontend must append `response.returned`, sign, and
    /// store the receipt after middleware returns the final user-visible body.
    ///
    /// Failover is not recorded: the receipt attests only the served route,
    /// and the attempt count reaches ops through an attribution header.
    #[allow(clippy::too_many_arguments)]
    pub async fn forward_chat_completion_stream_request(
        &self,
        req: ChatCompletionRequest<'_>,
    ) -> Result<StreamingForwardResult, ServiceError> {
        // §5.3: a direct service satisfies `aci_verified` by construction — the
        // workload the client verified (§9.1) is the one serving (§4.1); pinned
        // session lists are still refused in `admit_upstream`.
        let aci_required = req.requires_aci_verification() && !self.serves_directly();
        let received_body = req.received_body;
        let endpoint_path = req.endpoint_path;
        self.metrics.record_request(
            endpoint_path,
            RequestMode::Streaming,
            req.e2ee.as_ref().is_some(),
        );
        let target_route_id = req.context.target_route_id.clone();
        let request_id = req.context.request_id.clone();
        let backend_input_body = req.forwarded_body.unwrap_or_else(|| received_body.to_vec());
        let middleware_forwarded_body =
            target_route_id.as_ref().map(|_| backend_input_body.clone());
        let prepared = self.upstream.prepare(UpstreamRequest {
            body: backend_input_body,
            path: Some(endpoint_path.to_string()),
            target_route_id: target_route_id.clone(),
            ..Default::default()
        })?;
        if aci_required && !attested_route_eligible(prepared.is_tee) {
            return Err(ServiceError::UpstreamVerification(
                UpstreamVerificationError::NoEligibleAttestedRoute(
                    req.context.user_model.clone().unwrap_or_default(),
                ),
            ));
        }
        let forwarded_body = prepared.request.body.clone();
        let caller_supplied_upstream_event = req.upstream_verification_event.is_some();
        let verification_required = aci_required;
        let recorded_event = self
            .recorded_upstream_event(
                &prepared,
                verification_required,
                req.upstream_verification_event,
            )
            .await?;
        let admission = self.admit_upstream(
            recorded_event,
            &req.aci_session_ids,
            &prepared.model_id,
            None,
        )?;

        let (upstream_response, admission) = match self
            .forward_with_binding_reverify(
                &prepared,
                admission,
                verification_required,
                caller_supplied_upstream_event,
                &req.aci_session_ids,
                None,
                // Single forward: only flush the cache for an event we own.
                false,
                |prepared, event| async move {
                    self.upstream
                        .forward_stream_verified_prepared(prepared, &event)
                        .await
                },
            )
            .await
        {
            ReverifyOutcome::Forwarded(response, admission) => (response, admission),
            ReverifyOutcome::RefreshFailed(err) => return Err(err),
            ReverifyOutcome::Failed(err) => return Err(err.into()),
        };
        let selected = (*admission).select(upstream_response.served_instance_id.as_deref())?;
        // Match dstack-vllm-proxy compatibility behavior: streaming
        // requests whose upstream response is not exactly HTTP 200
        // are returned as ordinary buffered error responses. No
        // receipt is issued because there is no completed inference
        // stream to bind.
        if upstream_response.status_code != 200 {
            self.metrics.record_upstream_response(
                endpoint_path,
                RequestMode::Streaming,
                upstream_response.status_code,
                None,
            );
            self.metrics
                .record_stream_error(endpoint_path, StreamErrorKind::UpstreamNon2xx);
            let upstream_status = upstream_response.status_code;
            let upstream_headers = upstream_response.headers;
            let upstream_body = collect_upstream_body(upstream_response.body).await?;
            return Ok(StreamingForwardResult::UpstreamError(
                StreamingUpstreamError {
                    upstream_status,
                    upstream_headers,
                    upstream_body,
                },
            ));
        }

        let receipt_id = generate_receipt_id();
        let served_at = selected.admitted_at;
        let keyset = self.current_keyset();
        let mut builder = ReceiptBuilder::new(
            receipt_id.clone(),
            None,
            req.context.user_model.clone(),
            keyset.digest().to_string(),
            endpoint_path.to_string(),
            "POST".to_string(),
            served_at,
        );
        builder.add_request_received(received_body)?;
        if let Some(body) = middleware_forwarded_body.as_deref() {
            builder.add_middleware_forwarded(body)?;
        }
        if let Some(route_id) = target_route_id.as_deref() {
            builder.add_route_selected(route_id)?;
        }
        builder.add_request_forwarded(&forwarded_body)?;
        if !self.serves_directly() {
            let recorded = selected
                .lease
                .as_ref()
                .map(|lease| lease.session_id().to_string());
            Self::append_upstream_verified(&mut builder, &selected.event, recorded)?;
        }

        // This is the streaming entry point, so a response with no declared
        // media type is taken to be an event stream — the same assumption the
        // middleware path makes. Only a header that says otherwise turns the
        // protocol observation off.
        let is_sse = upstream_response
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .is_none_or(|(_, value)| is_sse_content_type(Some(value)));
        // E2EE stream encryption only knows how to encrypt SSE event payloads. A
        // non-SSE body (an upstream that answered `application/json`) cannot be
        // encrypted, and must not be returned in the clear or signed as if it
        // were — the middleware path rejects the same combination.
        if req.e2ee.is_some() && !is_sse {
            return Err(ServiceError::E2ee(E2eeError::EncryptionFailed));
        }

        let e2ee_response = req.e2ee.as_ref().map(|ctx| E2eeResponseInfo {
            version: ctx.version.clone(),
            algo: ctx.algo.clone(),
        });
        let e2ee_transformer = req
            .e2ee
            .clone()
            .map(|ctx| E2eeSseTransformer::new(ctx, endpoint_path.to_string()));

        let body = ReceiptFinalizingStream::new(
            self,
            upstream_response.body,
            builder,
            selected.lease,
            req.requester,
            endpoint_path.to_string(),
            e2ee_transformer,
            Some(request_id),
            is_sse,
        );

        Ok(StreamingForwardResult::Stream(StreamingForwardStream {
            receipt_id,
            keyset_digest: keyset.digest().to_string(),
            upstream_status: upstream_response.status_code,
            upstream_headers: upstream_response.headers,
            e2ee: e2ee_response,
            body: Box::pin(body),
        }))
    }

    pub(super) async fn recorded_upstream_event(
        &self,
        prepared: &PreparedUpstreamRequest,
        verification_required: bool,
        upstream_verification_event: Option<UpstreamVerifiedEvent>,
    ) -> Result<UpstreamVerifiedEvent, ServiceError> {
        let verification_required =
            verification_required || self.upstream.requires_verified_binding(prepared);
        let mut upstream_verification_event = match upstream_verification_event {
            Some(event) => Some(event),
            None => match &self.upstream_verifier {
                Some(verifier) => {
                    let request =
                        self.upstream_verification_request(prepared, verification_required);
                    Some(verifier.verify(request).await)
                }
                None => None,
            },
        };
        if let Some(event) = upstream_verification_event.as_mut() {
            // `required` is the effective policy for this request — a TEE-only
            // serving endpoint, the client's §5.3 constraint, or a route that
            // cannot be served unverified. The verifier
            // may report the upstream result, but the service owns the
            // client-facing decision recorded in the receipt (§7.5).
            event.required = verification_required;
        }

        let missing_verifier_result = upstream_verification_event.is_none();
        let event = upstream_verification_event.unwrap_or_else(|| UpstreamVerifiedEvent {
            upstream_name: prepared.upstream_name.clone(),
            model_id: prepared.model_id.clone(),
            url_origin: prepared.url_origin.clone(),
            verifier_id: "none".to_string(),
            result: VerificationResult::Failed,
            required: verification_required,
            reason: Some("no upstream verifier configured".to_string()),
            ..Default::default()
        });
        self.metrics.record_upstream_verification(&event);

        // Fail-closed gate. Run before any upstream IO.
        if verification_required {
            if missing_verifier_result {
                return Err(ServiceError::UpstreamVerification(
                    UpstreamVerificationError::NoVerifierResult,
                ));
            }
            if event.result != VerificationResult::Verified {
                let reason = event
                    .reason
                    .clone()
                    .unwrap_or_else(|| "upstream verification failed".to_string());
                return Err(ServiceError::UpstreamVerification(
                    UpstreamVerificationError::VerifierFailed(reason),
                ));
            }
            // §1.2: verification must obtain an *enforceable* channel binding.
            // A verified result with none pins nothing, so no session could
            // record what was enforced — refuse it like a failure.
            if event.channel_bindings.is_empty() {
                return Err(ServiceError::UpstreamVerification(
                    UpstreamVerificationError::VerifierFailed(
                        "no enforceable channel binding".to_string(),
                    ),
                ));
            }
        }

        // Aggregator receipts always carry an `upstream.verified` event. An
        // unconstrained request records a synthesized failed event so
        // downstream verifiers see the actual state.
        Ok(event)
    }

    /// Forward with an owned admission, refreshing and readmitting on a binding
    /// mismatch up to [`CHANNEL_BINDING_REVERIFY_ATTEMPTS`] times. Return the
    /// admission belonging to the successful attempt.
    ///
    /// `caller_supplied_event` (the gateway does not own the event) suppresses
    /// reverify entirely. On a *terminal* mismatch the gateway's verifier cache
    /// is invalidated when the gateway owns the event (`!caller_supplied_event`),
    /// or unconditionally when `always_invalidate_on_mismatch` is set — the
    /// failover path's conservative "flush a possibly-stale binding" default.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn forward_with_binding_reverify<R, Fwd, Fut>(
        &self,
        prepared: &PreparedUpstreamRequest,
        mut admission: AdmittedUpstream,
        verification_required: bool,
        caller_supplied_event: bool,
        aci_session_ids: &[String],
        receipt_journal: Option<&MiddlewareReceiptJournal>,
        always_invalidate_on_mismatch: bool,
        mut forward: Fwd,
    ) -> ReverifyOutcome<R>
    where
        R: WithheldResponse,
        Fwd: FnMut(PreparedUpstreamRequest, UpstreamVerifiedEvent) -> Fut,
        Fut: std::future::Future<Output = Result<R, UpstreamError>>,
    {
        let mut reverify_attempts = 0;
        loop {
            // The future owns its inputs; the admission keeps its leases until
            // the attempt returns or is cancelled.
            match forward(prepared.clone(), admission.event.clone()).await {
                Ok(response) => return ReverifyOutcome::Forwarded(response, Box::new(admission)),
                // Preserve the adapter's non-2xx status even when its body was
                // withheld. No serving instance was authenticated in this case,
                // so do not cite an arbitrary instance's admitted session.
                Err(UpstreamError::Upstream { status }) if !(200..300).contains(&status) => {
                    admission = admission.withheld_without_instance();
                    return ReverifyOutcome::Forwarded(R::withheld(status), Box::new(admission));
                }
                Err(UpstreamError::ChannelBindingMismatch(_))
                    if !caller_supplied_event
                        && reverify_attempts < CHANNEL_BINDING_REVERIFY_ATTEMPTS =>
                {
                    drop(admission);
                    reverify_attempts += 1;
                    match self
                        .refresh_upstream_event(prepared, verification_required)
                        .await
                    {
                        Ok(event) => {
                            admission = match self.admit_upstream(
                                event,
                                aci_session_ids,
                                &prepared.model_id,
                                receipt_journal,
                            ) {
                                Ok(admission) => admission,
                                Err(err) => return ReverifyOutcome::RefreshFailed(err),
                            };
                        }
                        Err(err) => return ReverifyOutcome::RefreshFailed(err),
                    }
                }
                Err(err) => {
                    // Reached on a terminal channel-binding mismatch (retries
                    // exhausted, or suppressed because the event is
                    // caller-supplied) OR any other upstream error. Only a
                    // mismatch flushes the cache, and only when we own the event
                    // or the failover path asks us to always flush.
                    if matches!(err, UpstreamError::ChannelBindingMismatch(_))
                        && (always_invalidate_on_mismatch || !caller_supplied_event)
                    {
                        self.invalidate_upstream_event(prepared, verification_required);
                    }
                    return ReverifyOutcome::Failed(err);
                }
            }
        }
    }

    pub(super) async fn refresh_upstream_event(
        &self,
        prepared: &PreparedUpstreamRequest,
        verification_required: bool,
    ) -> Result<UpstreamVerifiedEvent, ServiceError> {
        self.invalidate_upstream_event(prepared, verification_required);
        self.recorded_upstream_event(prepared, verification_required, None)
            .await
    }

    pub(super) fn invalidate_upstream_event(
        &self,
        prepared: &PreparedUpstreamRequest,
        verification_required: bool,
    ) {
        let Some(verifier) = &self.upstream_verifier else {
            return;
        };
        let request = self.upstream_verification_request(prepared, verification_required);
        verifier.invalidate(&request);
    }

    pub(super) fn upstream_verification_request(
        &self,
        prepared: &PreparedUpstreamRequest,
        required: bool,
    ) -> UpstreamVerificationRequest {
        UpstreamVerificationRequest {
            upstream_name: prepared.upstream_name.clone(),
            url_origin: prepared.url_origin.clone(),
            model_id: prepared.model_id.clone(),
            forwarded_body_hash: digest::sha256_hex(&prepared.request.body),
            required,
        }
    }

    /// Group complete verified channels before sealing or applying pins. Listing
    /// and admission share this path, using only the current verified event.
    pub(super) fn record_attested_upstream_session(
        &self,
        event: &UpstreamVerifiedEvent,
        now: u64,
    ) -> Result<Vec<SessionGroup>, ServiceError> {
        if self.serves_directly() || event.result != VerificationResult::Verified {
            return Ok(Vec::new());
        }
        let groups = if is_multi_instance(event.provider_type.as_deref()) {
            // Independent instances retain verifier priority (0G selects the
            // first eligible provider); only bindings within a group are canonical.
            let mut instances = BTreeMap::new();
            let mut groups = Vec::new();
            for binding in &event.channel_bindings {
                let id = instance_id(event, binding)
                    .filter(|id| !id.is_empty())
                    .ok_or(ServiceError::AdmissionInvariant(
                        "multi-instance binding has no instance identifier",
                    ))?;
                if let Some(previous) = instances.insert(id, binding) {
                    if previous != binding {
                        return Err(ServiceError::AdmissionInvariant(
                            "conflicting instance bindings",
                        ));
                    }
                    continue;
                }
                groups.push((Some(id.to_string()), vec![binding.clone()]));
            }
            groups
        } else {
            let bindings = canonical_bindings(&event.channel_bindings)?;
            if bindings.len() <= 1 {
                if bindings.is_empty() {
                    Vec::new()
                } else {
                    vec![(None, bindings)]
                }
            } else {
                let ChannelBinding::TlsSpkiSha256 { origin, .. } = &bindings[0] else {
                    return Err(ServiceError::AdmissionInvariant(
                        "unsupported multi-binding channel",
                    ));
                };
                if !bindings.iter().all(|binding| {
                    matches!(binding,
                    ChannelBinding::TlsSpkiSha256 { origin: other, .. } if other == origin)
                }) || event
                    .url_origin
                    .as_ref()
                    .is_some_and(|endpoint| endpoint != origin)
                {
                    return Err(ServiceError::AdmissionInvariant(
                        "mixed or unsupported channel origins",
                    ));
                }
                vec![(None, bindings)]
            }
        };
        let identity = Self::identity_from_provider_claims(event);
        groups
            .into_iter()
            .map(|(instance_key, bindings)| {
                let claims = match instance_key.as_deref() {
                    Some(id) => per_instance_session_claims(event, id),
                    None => session_claims_for_event(event),
                };
                let lease = self.seal_attested_session(
                    event,
                    identity.clone(),
                    bindings.clone(),
                    claims,
                    now,
                )?;
                Ok(SessionGroup {
                    instance_key,
                    bindings,
                    lease,
                })
            })
            .collect()
    }

    /// Finalize a fail-closed refusal into a receipt (§7.5): the exact request
    /// bytes the workload received, a failed `upstream.verified` event naming
    /// the requested model and the reason, and the exact error bytes served.
    /// No prompt was forwarded, so there is no `request.forwarded` event.
    pub fn issue_upstream_refusal_receipt(
        &self,
        endpoint_path: &str,
        model: Option<String>,
        received_body: &[u8],
        reason: &str,
        wire_error_body: &[u8],
        requester: Option<ReceiptOwner>,
    ) -> Result<SignedReceipt, ServiceError> {
        let keyset = self.current_keyset();
        let mut builder = ReceiptBuilder::new(
            generate_receipt_id(),
            None,
            model.clone(),
            keyset.digest().to_string(),
            endpoint_path.to_string(),
            "POST".to_string(),
            self.clock.now_secs(),
        );
        builder.add_request_received(received_body)?;
        // A refusal only happens when verification was required (§7.5).
        builder.add_upstream_verified_failed(
            model.as_deref().unwrap_or_default(),
            reason,
            true,
            None,
        )?;
        builder.add_response_returned(wire_error_body)?;
        let receipt = builder.finalize(self.keys.as_ref(), &self.default_receipt_key_id)?;
        self.store_receipt(receipt.clone(), requester, None)?;
        Ok(receipt)
    }

    /// Admit against the current verifier event, never against historical storage
    /// alone. Persist and acquire leases before any backend I/O, with or without pins.
    pub(super) fn admit_upstream(
        &self,
        mut event: UpstreamVerifiedEvent,
        allowed_ids: &[String],
        model_id: &str,
        receipt_journal: Option<&MiddlewareReceiptJournal>,
    ) -> Result<AdmittedUpstream, ServiceError> {
        let sessions = self.record_attested_upstream_session(&event, self.clock.now_secs())?;
        // Acquisition may wait behind a session established by another caller.
        // The forwarding decision follows acquisition, never its lookup snapshot.
        let admitted_at = self.clock.now_secs();
        // Early SSE may have committed a keyset while verification was pending.
        // A later initial, failover, or refreshed attempt must still be valid
        // under that member. Refuse before any prompt reaches the backend.
        if receipt_journal
            .and_then(MiddlewareReceiptJournal::early_response_keyset)
            .is_some_and(|keyset| admitted_at >= keyset.keyset().not_after)
        {
            return Err(ServiceError::EarlyResponseKeysetExpired);
        }
        let allowed: HashSet<&str> = allowed_ids.iter().map(String::as_str).collect();
        let mut bindings = Vec::new();
        let mut admitted_sessions = Vec::new();
        for session in sessions {
            if allowed.is_empty() || allowed.contains(session.lease.session_id()) {
                if !(session.lease.established_at() <= admitted_at
                    && admitted_at < session.lease.expires_at())
                {
                    return Err(ServiceError::AdmissionInvariant(
                        "store returned a session outside its validity window",
                    ));
                }
                bindings.extend(session.bindings.iter().cloned());
                admitted_sessions.push(session);
            }
        }
        if !allowed.is_empty() && admitted_sessions.is_empty() {
            return Err(ServiceError::UpstreamVerification(
                UpstreamVerificationError::NoEligibleAttestedSession(model_id.to_string()),
            ));
        }
        // Preserve direct and failed/non-TEE events; they have no sessions.
        if !admitted_sessions.is_empty() {
            event.channel_bindings = bindings;
        }
        let requires_instance = !self.serves_directly()
            && is_multi_instance(event.provider_type.as_deref())
            && event.result == VerificationResult::Verified;
        Ok(AdmittedUpstream {
            requires_instance,
            event,
            admitted_at,
            sessions: admitted_sessions,
        })
    }

    /// Return the channel's current session id, sealing (serializing once) and
    /// persisting a fresh document only when no session with identical
    /// verified material has a live validity period. The store's channel
    /// fingerprint provides the dedup — the document bytes themselves change
    /// with every validity period, so the id cannot. A new session stores the
    /// event's evidence even when the fingerprint leaves it out.
    fn seal_attested_session(
        &self,
        event: &UpstreamVerifiedEvent,
        identity: Option<WorkloadIdentityRef>,
        channel_bindings: Vec<ChannelBinding>,
        claims: SessionClaims,
        now: u64,
    ) -> Result<SessionLease, ServiceError> {
        let evidence = event.evidence.as_ref();
        let projected = serde_json::to_value(&claims.extra)
            .map_err(|err| ServiceError::SessionStore(err.to_string()))?;
        let reviewed = reviewed_session_material(event, &projected);
        let material = ChannelMaterial {
            schema: reviewed.then_some("aci-session-material/1"),
            provider_type: if reviewed {
                event.provider_type.as_deref()
            } else {
                None
            },
            upstream_name: &event.upstream_name,
            endpoint: &event.url_origin,
            verifier_id: &event.verifier_id,
            identity: &identity,
            channel_binding: &channel_bindings,
            claims: &claims,
            evidence_digest: if reviewed {
                None
            } else {
                evidence.and_then(|e| e["digest"].as_str())
            },
            verification_round: if reviewed {
                None
            } else {
                event.verification_round.as_ref()
            },
        };
        let fingerprint = material
            .fingerprint()
            .map_err(|err| ServiceError::SessionStore(format!("channel fingerprint: {err}")))?;

        let mut expires_at = now
            .checked_add(self.config.attested_session_ttl_seconds)
            .ok_or_else(|| {
                ServiceError::SessionStore("attested session expiry overflow".to_string())
            })?;
        if let Some(binding) = event.aci_identity.as_ref() {
            expires_at = expires_at.min(binding.keyset.not_after);
        }
        if expires_at <= now {
            return Err(ServiceError::AdmissionInvariant(
                "attested keyset expired before admission",
            ));
        }

        // Initial retention covers validity and at least one receipt TTL.
        let retention_until = now.saturating_add(self.config.receipt_ttl_seconds);
        // Runs only when the channel has no current session, so the evidence
        // is copied only for a new session.
        let mut seal = || {
            AttestedSession::seal(SessionDocument {
                api_version: SESSION_API_VERSION.to_string(),
                upstream_name: event.upstream_name.clone(),
                endpoint: event.url_origin.clone(),
                verifier_id: event.verifier_id.clone(),
                established_at: now,
                expires_at,
                identity: identity.clone(),
                channel_binding: channel_bindings.clone(),
                claims: claims.clone(),
                evidence: evidence.map(EvidenceRef::from_value).unwrap_or_default(),
            })
            .map_err(std::io::Error::other)
        };
        self.session_store
            .current_or_seal(&fingerprint, retention_until, now, &mut seal)
            .map_err(|err| {
                ServiceError::SessionStore(format!(
                    "failed to seal or persist attested session: {err}"
                ))
            })
    }

    /// Lift signing identity from provider claims and native ACI identity from
    /// the accepted original-JSON binding, never from arbitrary claims.
    fn identity_from_provider_claims(event: &UpstreamVerifiedEvent) -> Option<WorkloadIdentityRef> {
        let mut identity = WorkloadIdentityRef::default();
        if let Some(Value::Object(map)) = event.provider_claims.as_ref() {
            if let Some(addr) = map.get("signing_address").and_then(Value::as_str) {
                identity.signing_address = Some(addr.to_string());
            }
        }
        if let Some(binding) = event.aci_identity.as_ref() {
            identity.extra.insert(
                "workload_keyset_digest".into(),
                binding.keyset_digest.clone().into(),
            );
            identity.extra.insert(
                "subject".into(),
                binding
                    .keyset
                    .subject
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            );
            identity
                .extra
                .insert("keyset_not_after".into(), binding.keyset.not_after.into());
        }
        (!identity.is_empty()).then_some(identity)
    }

    /// Append the `upstream.verified` receipt event, attaching the session id and
    /// the typed claim verdicts when a verified session was recorded.
    pub(super) fn append_upstream_verified(
        builder: &mut ReceiptBuilder,
        event: &UpstreamVerifiedEvent,
        recorded: Option<String>,
    ) -> Result<(), ReceiptError> {
        // §8.3: the receipt cites the session; the claims live on the session
        // record, so a verified event carries an id and a failed one a reason.
        match recorded {
            Some(session_id) => builder.add_upstream_verified_with_session(event, &session_id),
            None => builder.add_upstream_verified_failed(
                &event.model_id,
                event
                    .reason
                    .as_deref()
                    .unwrap_or("no enforceable verified binding"),
                event.required,
                Some(&event.upstream_name),
            ),
        }
    }
}
