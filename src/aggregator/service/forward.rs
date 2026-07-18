use serde_json::{json, Value};

use crate::aci::receipt::{
    ChannelBinding, ReceiptBuilder, ReceiptError, TransparencyEventKind, UpstreamVerifiedEvent,
    VerificationResult,
};
use crate::aci::types::Receipt;
use crate::aci::upstream::{PreparedUpstreamRequest, UpstreamError, UpstreamRequest};
use crate::aggregator::metrics::{RequestMode, StreamErrorKind};
use crate::aggregator::session::{
    AttestedSession, EvidenceRef, SessionClaims, WorkloadIdentityRef,
};

use super::claims::{chutes_instance_id, per_instance_session_claims, session_claims_for_event};
use super::e2ee_crypto::encrypt_e2ee_response_body;
use super::errors::forwarding_error;
use super::helpers::{
    accepted_response_model, collect_upstream_body, extract_chat_id, generate_receipt_id,
};
use super::streaming::{E2eeSseTransformer, ReceiptFinalizingStream};
use super::{
    AciService, ChatCompletionRequest, E2eeResponseInfo, ForwardResult, GatewayRequestContext,
    ReceiptOwner, ServiceError, StreamingForwardResult, StreamingForwardStream,
    StreamingUpstreamError, UpstreamVerificationError, UpstreamVerificationRequest,
    CHANNEL_BINDING_REVERIFY_ATTEMPTS, CHAT_COMPLETIONS_PATH,
};

/// Outcome of [`AciService::forward_with_binding_reverify`]. The caller maps
/// each non-success variant to its own policy — abort (single forward) or fail
/// over to the next candidate (middleware).
pub(super) enum ReverifyOutcome<R> {
    /// Forwarding succeeded, after zero or more transparent reverify rounds.
    Forwarded(R),
    /// A reverify (cached-event refresh) attempt itself failed.
    RefreshFailed(ServiceError),
    /// Forwarding failed: either a terminal channel-binding mismatch (after the
    /// verifier cache was invalidated per policy) or an unrelated upstream
    /// error. Both map to the caller's failure path; the helper has already
    /// applied any mismatch invalidation.
    Failed(UpstreamError),
}

/// A session sealed for one verified channel binding (one per Chutes instance).
pub(super) struct SealedSession {
    /// The per-instance key (Chutes instance id) for a multi-instance backend;
    /// `None` for a single-channel backend.
    instance_key: Option<String>,
    session_id: String,
    claims: SessionClaims,
}

/// Pick which sealed session the receipt cites. For a backend that fronts
/// several instances (Chutes), cite the one that actually served; a
/// single-channel backend reports no served instance, so its one session is
/// used. A served instance with no matching sealed session (e.g. it dropped out
/// of this verification) cites nothing — never the wrong instance.
pub(super) fn cite_served_session(
    sealed: &[SealedSession],
    served_instance_id: Option<&str>,
) -> Option<(String, SessionClaims)> {
    let chosen = match served_instance_id {
        Some(id) => sealed
            .iter()
            .find(|s| s.instance_key.as_deref() == Some(id)),
        None => sealed.first(),
    };
    chosen.map(|s| (s.session_id.clone(), s.claims.clone()))
}

impl AciService {
    pub async fn forward_chat_completion(
        &self,
        received_body: &[u8],
        forwarded_body: Option<Vec<u8>>,
        upstream_required: Option<bool>,
        upstream_verification_event: Option<UpstreamVerifiedEvent>,
    ) -> Result<ForwardResult, ServiceError> {
        self.forward_chat_completion_request(ChatCompletionRequest {
            context: GatewayRequestContext::default(),
            endpoint_path: CHAT_COMPLETIONS_PATH,
            received_body,
            forwarded_body,
            upstream_required,
            upstream_verification_event,
            requester: None,
            e2ee: None,
        })
        .await
    }

    /// Rich variant of [`Self::forward_chat_completion`] that also takes
    /// the receipt owner so the receipt store can authenticate later
    /// lookups (ACI §9.1, §9.5).
    pub async fn forward_chat_completion_request(
        &self,
        req: ChatCompletionRequest<'_>,
    ) -> Result<ForwardResult, ServiceError> {
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
        let forwarded_body = prepared.request.body.clone();
        let caller_supplied_upstream_event = req.upstream_verification_event.is_some();
        let upstream_required =
            self.upstream_required_for_prepared(&prepared, req.upstream_required);
        let mut recorded_event = self
            .recorded_upstream_event(
                &prepared,
                upstream_required,
                req.upstream_verification_event,
            )
            .await?;

        let mut upstream_response = match self
            .forward_with_binding_reverify(
                &prepared,
                &mut recorded_event,
                upstream_required,
                caller_supplied_upstream_event,
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
            ReverifyOutcome::Forwarded(response) => response,
            ReverifyOutcome::RefreshFailed(err) => return Err(err),
            ReverifyOutcome::Failed(err) => return Err(forwarding_error(err)),
        };
        Self::attach_provider_response_claims(
            &mut recorded_event,
            upstream_response.provider_response_claims.take(),
        );
        let response_model =
            accepted_response_model(upstream_response.status_code, &upstream_response.body);
        self.metrics.record_upstream_response(
            endpoint_path,
            RequestMode::Buffered,
            upstream_response.status_code,
            response_model.as_deref(),
        );

        // A client image-URL fetch failure the upstream reports as a 5xx is the
        // caller's bad input: remap it to a surface-correct 400. This is decided on
        // the cleartext body here — before E2EE encryption and before the receipt is
        // built — so the receipt attests exactly the body/status the client receives.
        let mut upstream_headers = upstream_response.headers;
        let (client_status, client_body) = match crate::middleware::errors::image_input_error_parts(
            crate::middleware::errors::surface_for_path(endpoint_path),
            received_body,
            upstream_response.status_code,
            &upstream_response.body,
            None,
        ) {
            Some((status, body)) => {
                // The remapped body is a JSON envelope; don't let the client inherit
                // a non-JSON upstream content-type (some backends 5xx with text/*).
                upstream_headers.insert("content-type".to_string(), "application/json".to_string());
                (status, body)
            }
            None => (
                upstream_response.status_code,
                upstream_response.body.clone(),
            ),
        };

        let e2ee = req.e2ee.as_ref();
        let wire_response_body = match e2ee {
            Some(ctx) => encrypt_e2ee_response_body(&client_body, ctx, endpoint_path)?,
            None => client_body.clone(),
        };
        let e2ee_response = e2ee.map(|ctx| E2eeResponseInfo {
            version: ctx.version.clone(),
            algo: ctx.algo.clone(),
        });

        // Receipt construction with bytes the service actually
        // observed. X-Request-Hash is never trusted here because we
        // do not even consult it; the byte source is the body the
        // service received from axum.
        let receipt_id = generate_receipt_id();
        let chat_id = extract_chat_id(&client_body);
        let served_at = self.clock.now_secs();
        let mut builder = ReceiptBuilder::new(
            receipt_id,
            chat_id,
            req.context.user_model.clone(),
            self.workload_id.clone(),
            self.workload_keyset_digest.clone(),
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
        if received_body != forwarded_body.as_slice() {
            builder.add_transparency_event(TransparencyEventKind::RequestModified)?;
        }
        let sealed = self.record_attested_upstream_session(&recorded_event)?;
        // When the backend fronts several instances (Chutes), cite the session of
        // the instance that actually served this request.
        let recorded =
            cite_served_session(&sealed, upstream_response.served_instance_id.as_deref());
        Self::append_upstream_verified(&mut builder, recorded_event, recorded)?;
        // The session is keyed on the requested (routed) model; record the exact
        // upstream-served model in the receipt's upstream.verified event.
        builder.set_upstream_verified_model_id(response_model.clone());
        // Modified when the returned bytes differ from what the upstream sent —
        // whether from E2EE encryption or an image-input 400 remap.
        if upstream_response.body != wire_response_body {
            builder.add_transparency_event(TransparencyEventKind::ResponseModified)?;
        }
        builder.add_response_returned(&client_body, &wire_response_body)?;

        let receipt = builder.finalize(self.keys.as_ref(), &self.default_receipt_key_id)?;
        self.store_receipt(receipt.clone(), req.requester.clone());
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
    /// Build the receipt event prefix shared by the buffered and
    /// streaming commit paths: request.received → middleware.forwarded →
    /// route.selected → request.forwarded (+transparency) →
    /// upstream.verified. The caller appends response.received afterwards
    /// (buffered now, streaming at end). Failover is not recorded in the
    /// receipt — the receipt attests only the served (selected) route; the
    /// attempt count is surfaced to ops via an attribution header.
    #[allow(clippy::too_many_arguments)]
    pub async fn forward_chat_completion_stream_request(
        &self,
        req: ChatCompletionRequest<'_>,
    ) -> Result<StreamingForwardResult, ServiceError> {
        let received_body = req.received_body;
        let endpoint_path = req.endpoint_path;
        self.metrics.record_request(
            endpoint_path,
            RequestMode::Streaming,
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
        let forwarded_body = prepared.request.body.clone();
        let caller_supplied_upstream_event = req.upstream_verification_event.is_some();
        let upstream_required =
            self.upstream_required_for_prepared(&prepared, req.upstream_required);
        let mut recorded_event = self
            .recorded_upstream_event(
                &prepared,
                upstream_required,
                req.upstream_verification_event,
            )
            .await?;

        let upstream_response = match self
            .forward_with_binding_reverify(
                &prepared,
                &mut recorded_event,
                upstream_required,
                caller_supplied_upstream_event,
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
            ReverifyOutcome::Forwarded(response) => response,
            ReverifyOutcome::RefreshFailed(err) => return Err(err),
            ReverifyOutcome::Failed(err) => return Err(forwarding_error(err)),
        };
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
        let served_at = self.clock.now_secs();
        let mut builder = ReceiptBuilder::new(
            receipt_id.clone(),
            None,
            req.context.user_model.clone(),
            self.workload_id.clone(),
            self.workload_keyset_digest.clone(),
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
        if received_body != forwarded_body.as_slice() {
            builder.add_transparency_event(TransparencyEventKind::RequestModified)?;
        }
        let sealed = self.record_attested_upstream_session(&recorded_event)?;
        // When the backend fronts several instances (Chutes), cite the session of
        // the instance that actually served this request.
        let recorded =
            cite_served_session(&sealed, upstream_response.served_instance_id.as_deref());
        Self::append_upstream_verified(&mut builder, recorded_event, recorded)?;

        let e2ee_response = req.e2ee.as_ref().map(|ctx| E2eeResponseInfo {
            version: ctx.version.clone(),
            algo: ctx.algo.clone(),
        });
        let response_modified = req.e2ee.is_some();
        let e2ee_transformer = req
            .e2ee
            .clone()
            .map(|ctx| E2eeSseTransformer::new(ctx, endpoint_path.to_string()));

        let body = ReceiptFinalizingStream::new(
            self,
            upstream_response.body,
            builder,
            req.requester,
            endpoint_path.to_string(),
            e2ee_transformer,
            response_modified,
        );

        Ok(StreamingForwardResult::Stream(StreamingForwardStream {
            receipt_id,
            upstream_status: upstream_response.status_code,
            upstream_headers: upstream_response.headers,
            e2ee: e2ee_response,
            body: Box::pin(body),
        }))
    }

    pub(super) async fn recorded_upstream_event(
        &self,
        prepared: &PreparedUpstreamRequest,
        upstream_required: Option<bool>,
        upstream_verification_event: Option<UpstreamVerifiedEvent>,
    ) -> Result<UpstreamVerifiedEvent, ServiceError> {
        let upstream_required = upstream_required.unwrap_or(self.config.upstream_required_default);
        let mut upstream_verification_event = match upstream_verification_event {
            Some(event) => Some(event),
            None => match &self.upstream_verifier {
                Some(verifier) => {
                    let request = self.upstream_verification_request(prepared, upstream_required);
                    Some(verifier.verify(request).await)
                }
                None => None,
            },
        };
        if let Some(event) = upstream_verification_event.as_mut() {
            // `required` is the client's effective mode for this request. The
            // verifier may report the upstream result, but the service owns the
            // client-facing downgrade decision recorded in the receipt.
            event.required = upstream_required;
        }

        let missing_verifier_result = upstream_verification_event.is_none();
        let event = upstream_verification_event.unwrap_or_else(|| UpstreamVerifiedEvent {
            upstream_name: prepared.upstream_name.clone(),
            model_id: prepared.model_id.clone(),
            url_origin: prepared.url_origin.clone(),
            verifier_id: "none".to_string(),
            result: VerificationResult::Failed,
            required: upstream_required,
            reason: Some("no upstream verifier configured".to_string()),
            ..Default::default()
        });
        self.metrics.record_upstream_verification(&event);

        // Fail-closed gate. Run before any upstream IO.
        if upstream_required {
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
        }

        // Aggregator receipts always carry an `upstream.verified`
        // event. The opt-out path records a synthesized failed event
        // so downstream verifiers see the actual state.
        Ok(event)
    }

    /// Forward `prepared` against `recorded_event`, transparently reverifying
    /// (refreshing the cached upstream event) and retrying on a channel-binding
    /// mismatch up to [`CHANNEL_BINDING_REVERIFY_ATTEMPTS`] times. A successful
    /// refresh is written back through `recorded_event`, so the caller sees the
    /// event actually forwarded with.
    ///
    /// `caller_supplied_event` (the gateway does not own the event) suppresses
    /// reverify entirely. On a *terminal* mismatch the gateway's verifier cache
    /// is invalidated when the gateway owns the event (`!caller_supplied_event`),
    /// or unconditionally when `always_invalidate_on_mismatch` is set — the
    /// failover path's conservative "flush a possibly-stale binding" default.
    pub(super) async fn forward_with_binding_reverify<R, Fwd, Fut>(
        &self,
        prepared: &PreparedUpstreamRequest,
        recorded_event: &mut UpstreamVerifiedEvent,
        upstream_required: Option<bool>,
        caller_supplied_event: bool,
        always_invalidate_on_mismatch: bool,
        mut forward: Fwd,
    ) -> ReverifyOutcome<R>
    where
        Fwd: FnMut(PreparedUpstreamRequest, UpstreamVerifiedEvent) -> Fut,
        Fut: std::future::Future<Output = Result<R, UpstreamError>>,
    {
        let mut reverify_attempts = 0;
        loop {
            // `recorded_event` is cloned per attempt because the forwarded future
            // owns its inputs. Bounded to CHANNEL_BINDING_REVERIFY_ATTEMPTS + 1,
            // and `prepared` was already cloned per attempt before this refactor.
            match forward(prepared.clone(), recorded_event.clone()).await {
                Ok(response) => return ReverifyOutcome::Forwarded(response),
                Err(UpstreamError::ChannelBindingMismatch(_))
                    if !caller_supplied_event
                        && reverify_attempts < CHANNEL_BINDING_REVERIFY_ATTEMPTS =>
                {
                    reverify_attempts += 1;
                    match self
                        .refresh_upstream_event(prepared, upstream_required)
                        .await
                    {
                        Ok(event) => *recorded_event = event,
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
                        self.invalidate_upstream_event(prepared, upstream_required);
                    }
                    return ReverifyOutcome::Failed(err);
                }
            }
        }
    }

    pub(super) fn upstream_required_for_prepared(
        &self,
        prepared: &PreparedUpstreamRequest,
        requested: Option<bool>,
    ) -> Option<bool> {
        if prepared.is_tee == Some(false) {
            Some(false)
        } else {
            requested
        }
    }

    pub(super) async fn refresh_upstream_event(
        &self,
        prepared: &PreparedUpstreamRequest,
        upstream_required: Option<bool>,
    ) -> Result<UpstreamVerifiedEvent, ServiceError> {
        let upstream_required = upstream_required.unwrap_or(self.config.upstream_required_default);
        self.invalidate_upstream_event(prepared, Some(upstream_required));
        self.recorded_upstream_event(prepared, Some(upstream_required), None)
            .await
    }

    pub(super) fn invalidate_upstream_event(
        &self,
        prepared: &PreparedUpstreamRequest,
        upstream_required: Option<bool>,
    ) {
        let Some(verifier) = &self.upstream_verifier else {
            return;
        };
        let required = upstream_required.unwrap_or(self.config.upstream_required_default);
        let request = self.upstream_verification_request(prepared, required);
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
            forwarded_body_hash: crate::aci::canonical::sha256_hex(&prepared.request.body),
            required,
        }
    }

    /// Seal + persist one attested session per verified channel binding, and
    /// return them. A single-TEE provider yields one; Chutes yields one per
    /// instance. The receipt cites one of these (see [`cite_served_session`]);
    /// each persisted session also carries the evidence + reasons (deep audit).
    pub(super) fn record_attested_upstream_session(
        &self,
        event: &UpstreamVerifiedEvent,
    ) -> Result<Vec<SealedSession>, ServiceError> {
        if event.result != VerificationResult::Verified {
            return Ok(Vec::new());
        }

        let now = self.clock.now_secs();
        // Retention window (`receipt_ttl_seconds`), so a relying party verifying a
        // citing receipt can resolve its `session_id`. The session is sealed
        // slightly before its receipt, so it expires up to one request-processing
        // interval (sub-second) sooner than that receipt — both use the same TTL
        // off a per-call `now`. This is a retention deadline, not a binding
        // validity one (the forwarding path only ever uses a fresh lease).
        let expires_at = now.saturating_add(self.config.receipt_ttl_seconds);

        // One content-addressed session per channel binding: a single-TEE
        // provider has one binding (its channel); Chutes has one per instance, so
        // each instance becomes its own session. This relies on every
        // non-instance provider verifying a single channel with exactly one
        // binding; a channel that emitted several bindings would be split into
        // separate sessions here, which would be wrong for one logical channel.
        let identity = Self::identity_from_provider_claims(event.provider_claims.as_ref());
        let mut sealed = Vec::with_capacity(event.channel_bindings.len());
        for binding in &event.channel_bindings {
            let instance = chutes_instance_id(event, binding);
            let claims = match instance {
                Some(instance_id) => per_instance_session_claims(event, instance_id),
                None => session_claims_for_event(event),
            };
            // A per-instance (Chutes) binding excludes the shared, nonce-bound raw
            // evidence so re-verifying the same instance is a no-op; a single
            // channel keeps the event's evidence.
            let evidence = if instance.is_some() {
                EvidenceRef::default()
            } else {
                event
                    .evidence
                    .as_ref()
                    .map(EvidenceRef::from_value)
                    .unwrap_or_default()
            };
            let session_id = self.seal_attested_session(
                event,
                identity.clone(),
                vec![binding.clone()],
                claims.clone(),
                evidence,
                now,
                expires_at,
            )?;
            sealed.push(SealedSession {
                instance_key: instance.map(str::to_string),
                session_id,
                claims,
            });
        }
        Ok(sealed)
    }

    /// Seal (or renew) one session for a verified channel binding and return its
    /// content-addressed id. The id commits to the evidence *digest*, not its
    /// bytes, so a digest-only seal derives the same id: if the session is
    /// already recorded and live, renew its deadline rather than re-sealing and
    /// re-appending its evidence each request.
    #[allow(clippy::too_many_arguments)]
    fn seal_attested_session(
        &self,
        event: &UpstreamVerifiedEvent,
        identity: Option<WorkloadIdentityRef>,
        channel_bindings: Vec<ChannelBinding>,
        claims: SessionClaims,
        evidence: EvidenceRef,
        now: u64,
        expires_at: u64,
    ) -> Result<String, ServiceError> {
        let session_id = AttestedSession::seal(
            event.upstream_name.clone(),
            event.url_origin.clone(),
            event.verifier_id.clone(),
            identity.clone(),
            channel_bindings.clone(),
            claims.clone(),
            EvidenceRef {
                digest: evidence.digest.clone(),
                data_uri: None,
            },
            now,
            expires_at,
        )?
        .session_id;
        if self
            .session_store
            .renew_session(&session_id, expires_at, now)
        {
            return Ok(session_id);
        }

        // First sighting (or the prior record lapsed): seal the full evidence
        // and persist it once.
        let session = AttestedSession::seal(
            event.upstream_name.clone(),
            event.url_origin.clone(),
            event.verifier_id.clone(),
            identity,
            channel_bindings,
            claims,
            evidence,
            now,
            expires_at,
        )?;
        debug_assert_eq!(
            session.session_id, session_id,
            "digest-only probe must derive the same id as the full seal"
        );
        self.session_store
            .put_session(session, now)
            .map_err(|err| {
                ServiceError::SessionStore(format!(
                    "failed to persist attested session {session_id}: {err}"
                ))
            })?;
        Ok(session_id)
    }

    /// Lift the response-signing address out of provider claims into a verified
    /// identity, when present.
    fn identity_from_provider_claims(
        provider_claims: Option<&Value>,
    ) -> Option<WorkloadIdentityRef> {
        let mut identity = WorkloadIdentityRef::default();
        if let Some(Value::Object(map)) = provider_claims {
            if let Some(addr) = map.get("signing_address").and_then(Value::as_str) {
                identity.signing_address = Some(addr.to_string());
            }
        }
        (!identity.is_empty()).then_some(identity)
    }

    /// Append the `upstream.verified` receipt event, attaching the session id and
    /// the typed claim verdicts when a verified session was recorded.
    pub(super) fn append_upstream_verified(
        builder: &mut ReceiptBuilder,
        event: UpstreamVerifiedEvent,
        recorded: Option<(String, SessionClaims)>,
    ) -> Result<(), ReceiptError> {
        // A sealed session and its claims are inseparable: either both (verified)
        // or neither (failed / no binding).
        match recorded {
            Some((session_id, claims)) => {
                builder.add_upstream_verified_with_session(event, &session_id, &claims)
            }
            None => builder.add_upstream_verified(event),
        }
    }

    pub(super) fn attach_provider_response_claims(
        event: &mut UpstreamVerifiedEvent,
        claims: Option<Value>,
    ) {
        let Some(claims) = claims else {
            return;
        };
        event.provider_claims = Some(json!({
            "response_evidence": claims
        }));
    }

    pub(super) fn store_receipt(&self, receipt: Receipt, requester: Option<ReceiptOwner>) {
        let now = self.clock.now_secs();
        let expires_at = now.saturating_add(self.config.receipt_ttl_seconds);
        self.receipt_store.put(receipt, requester, now, expires_at);
    }
}

#[cfg(test)]
mod tests {
    use super::{cite_served_session, SealedSession};
    use crate::aggregator::session::SessionClaims;

    fn sealed(instance_key: Option<&str>, session_id: &str) -> SealedSession {
        SealedSession {
            instance_key: instance_key.map(str::to_string),
            session_id: session_id.to_string(),
            claims: SessionClaims::default(),
        }
    }

    #[test]
    fn cite_picks_the_serving_instances_session() {
        let sessions = vec![
            sealed(Some("inst-a"), "as_a"),
            sealed(Some("inst-b"), "as_b"),
        ];
        assert_eq!(
            cite_served_session(&sessions, Some("inst-b")).map(|(id, _)| id),
            Some("as_b".to_string()),
        );
        // A served instance with no sealed session cites nothing, not the wrong one.
        assert!(cite_served_session(&sessions, Some("inst-z")).is_none());
    }

    #[test]
    fn cite_uses_the_single_session_for_a_single_channel() {
        let sessions = vec![sealed(None, "as_one")];
        // No served instance (single-channel backend) -> the one sealed session.
        assert_eq!(
            cite_served_session(&sessions, None).map(|(id, _)| id),
            Some("as_one".to_string()),
        );
        // Nothing sealed -> nothing cited.
        assert!(cite_served_session(&[], None).is_none());
    }
}
