use super::*;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::claims::*;
use super::e2ee_crypto::*;
use super::helpers::*;
use super::streaming::*;
use crate::aci::receipt::{
    ReceiptBuilder, ReceiptError, TransparencyEventKind, UpstreamVerifiedEvent, VerificationResult,
};
use crate::aci::types::Receipt;
use crate::aci::upstream::{PreparedUpstreamRequest, UpstreamError, UpstreamRequest};
use crate::aggregator::metrics::{RequestMode, StreamErrorKind};
use crate::aggregator::session::{
    AttestedSession, EvidenceRef, SessionClaims, WorkloadIdentityRef,
};

pub(super) fn is_retryable_provider_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

/// Track the highest-priority failover error so that, when every candidate
/// fails, the returned error reflects the most informative failure.
/// Priority order: verification (3), then transport (2), then routing (1).
pub(super) fn upgrade_err(slot: &mut Option<(u8, ServiceError)>, priority: u8, err: ServiceError) {
    if slot.as_ref().map(|(p, _)| priority >= *p).unwrap_or(true) {
        *slot = Some((priority, err));
    }
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
        let mut recorded_event = self
            .recorded_upstream_event(
                &prepared,
                req.upstream_required,
                req.upstream_verification_event,
            )
            .await?;

        let mut reverify_attempts = 0;
        let upstream_response = loop {
            match self
                .upstream
                .forward_verified_prepared(prepared.clone(), &recorded_event)
                .await
            {
                Ok(response) => break response,
                Err(UpstreamError::ChannelBindingMismatch(_))
                    if !caller_supplied_upstream_event
                        && reverify_attempts < CHANNEL_BINDING_REVERIFY_ATTEMPTS =>
                {
                    reverify_attempts += 1;
                    recorded_event = self
                        .refresh_upstream_event(&prepared, req.upstream_required)
                        .await?;
                }
                Err(err @ UpstreamError::ChannelBindingMismatch(_))
                    if !caller_supplied_upstream_event =>
                {
                    self.invalidate_upstream_event(&prepared, req.upstream_required);
                    return Err(err.into());
                }
                Err(err) => return Err(err.into()),
            }
        };
        let response_model =
            accepted_response_model(upstream_response.status_code, &upstream_response.body);
        self.metrics.record_upstream_response(
            endpoint_path,
            RequestMode::Buffered,
            upstream_response.status_code,
            response_model.as_deref(),
        );

        let e2ee = req.e2ee.as_ref();
        let wire_response_body = match e2ee {
            Some(ctx) => encrypt_e2ee_response_body(&upstream_response.body, ctx, endpoint_path)?,
            None => upstream_response.body.clone(),
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
        let chat_id = extract_chat_id(&upstream_response.body);
        let served_at = self.clock.now_secs();
        let mut builder = ReceiptBuilder::new(
            receipt_id,
            chat_id,
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
        let recorded = self.record_attested_upstream_session(&recorded_event)?;
        Self::append_upstream_verified(&mut builder, recorded_event, recorded)?;
        // The session is keyed on the requested (routed) model; record the exact
        // upstream-served model in the receipt's upstream.verified event.
        builder.set_upstream_verified_model_id(response_model.clone());
        if upstream_response.body != wire_response_body {
            builder.add_transparency_event(TransparencyEventKind::ResponseModified)?;
        }
        builder.add_response_returned(&upstream_response.body, &wire_response_body)?;

        let receipt = builder.finalize(self.keys.as_ref(), &self.default_receipt_key_id)?;
        self.store_receipt(receipt.clone(), req.requester.clone());
        self.metrics.record_receipt_issued(
            endpoint_path,
            RequestMode::Buffered,
            response_model.as_deref(),
        );

        Ok(ForwardResult {
            receipt,
            upstream_status: upstream_response.status_code,
            upstream_body: wire_response_body,
            upstream_headers: upstream_response.headers,
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
    pub(super) fn build_middleware_receipt_prefix(
        &self,
        receipt_id: &str,
        chat_id: Option<String>,
        served_at: u64,
        endpoint_path: &str,
        received_body: &[u8],
        middleware_forwarded_body: &[u8],
        selected_route_id: &str,
        forwarded_body: &[u8],
        recorded_event: UpstreamVerifiedEvent,
        recorded: Option<(String, SessionClaims)>,
    ) -> Result<ReceiptBuilder, ServiceError> {
        let mut builder = ReceiptBuilder::new(
            receipt_id.to_string(),
            chat_id,
            self.workload_id.clone(),
            self.workload_keyset_digest.clone(),
            endpoint_path.to_string(),
            "POST".to_string(),
            served_at,
        );
        builder.add_request_received(received_body)?;
        builder.add_middleware_forwarded(middleware_forwarded_body)?;
        builder.add_route_selected(selected_route_id)?;
        builder.add_request_forwarded(forwarded_body)?;
        if received_body != forwarded_body {
            builder.add_transparency_event(TransparencyEventKind::RequestModified)?;
        }
        Self::append_upstream_verified(&mut builder, recorded_event, recorded)?;
        Ok(builder)
    }

    pub async fn forward_chat_completion_for_middleware(
        &self,
        req: ChatCompletionRequest<'_>,
        candidates: Vec<ForwardCandidate>,
        stream: bool,
        receipt_journal: MiddlewareReceiptJournal,
    ) -> Result<MiddlewareForwardResult, ServiceError> {
        let received_body = req.received_body;
        let endpoint_path = req.endpoint_path;
        let mode = if stream {
            RequestMode::Streaming
        } else {
            RequestMode::Buffered
        };
        self.metrics
            .record_request(endpoint_path, mode, req.e2ee.as_ref().is_some());

        if candidates.is_empty() {
            return Err(ServiceError::Upstream(UpstreamError::Routing(
                "no candidate routes supplied".to_string(),
            )));
        }
        // A caller-supplied verifier event only applies to a single
        // explicit candidate (non-failover). With an ordered list the
        // backend always computes per-candidate events.
        let caller_supplied_upstream_event =
            req.upstream_verification_event.is_some() && candidates.len() == 1;
        let single_caller_event = if caller_supplied_upstream_event {
            req.upstream_verification_event.clone()
        } else {
            None
        };
        let candidate_route_ids: Vec<String> =
            candidates.iter().map(|c| c.route_id.clone()).collect();
        let last_index = candidates.len() - 1;

        // Highest-priority error across exhausted candidates, returned if
        // no candidate succeeds.
        //
        // The number of candidates attempted (`index + 1` when one succeeds)
        // is surfaced via a response header for the caller's metrics. Failover
        // is internal to this forwarder and is NOT recorded in the user-facing
        // receipt; the receipt attests only the served request (route.selected
        // + upstream.verified + hashes).
        let mut aggregated_err: Option<(u8, ServiceError)> = None;

        for (index, candidate) in candidates.iter().enumerate() {
            let route_id = candidate.route_id.clone();
            let is_last = index == last_index;

            let prepared = match self.upstream.prepare(UpstreamRequest {
                body: candidate.body.clone(),
                path: Some(endpoint_path.to_string()),
                target_route_id: Some(route_id.clone()),
                ..Default::default()
            }) {
                Ok(prepared) => prepared,
                Err(UpstreamError::Routing(message)) => {
                    upgrade_err(
                        &mut aggregated_err,
                        1,
                        ServiceError::Upstream(UpstreamError::Routing(message)),
                    );
                    continue;
                }
                Err(err) => {
                    upgrade_err(&mut aggregated_err, 2, err.into());
                    continue;
                }
            };

            // Per-route fail-closed mode: explicitly non-TEE routes never
            // fail closed; TEE and unclassified routes honour the
            // request-level `upstream_required` flag.
            let non_tee = prepared.is_tee == Some(false);
            let candidate_required = if non_tee {
                Some(false)
            } else {
                req.upstream_required
            };

            let mut recorded_event = match self
                .recorded_upstream_event(&prepared, candidate_required, single_caller_event.clone())
                .await
            {
                Ok(event) => event,
                Err(ServiceError::UpstreamVerification(uv)) => {
                    upgrade_err(
                        &mut aggregated_err,
                        3,
                        ServiceError::UpstreamVerification(uv),
                    );
                    continue;
                }
                Err(err) => return Err(err),
            };

            let forwarded_body = prepared.request.body.clone();

            if stream {
                let mut reverify_attempts = 0;
                let upstream_response = loop {
                    match self
                        .upstream
                        .forward_stream_verified_prepared(prepared.clone(), &recorded_event)
                        .await
                    {
                        Ok(response) => break Some(response),
                        Err(UpstreamError::ChannelBindingMismatch(_))
                            if !caller_supplied_upstream_event
                                && reverify_attempts < CHANNEL_BINDING_REVERIFY_ATTEMPTS =>
                        {
                            reverify_attempts += 1;
                            match self
                                .refresh_upstream_event(&prepared, candidate_required)
                                .await
                            {
                                Ok(event) => recorded_event = event,
                                // A reverify failure must fail over to the
                                // next candidate, not abort the whole list.
                                Err(err) => {
                                    let priority =
                                        if matches!(err, ServiceError::UpstreamVerification(_)) {
                                            3
                                        } else {
                                            2
                                        };
                                    upgrade_err(&mut aggregated_err, priority, err);
                                    break None;
                                }
                            }
                        }
                        Err(err @ UpstreamError::ChannelBindingMismatch(_)) => {
                            self.invalidate_upstream_event(&prepared, candidate_required);
                            upgrade_err(&mut aggregated_err, 2, err.into());
                            break None;
                        }
                        Err(err) => {
                            upgrade_err(&mut aggregated_err, 2, err.into());
                            break None;
                        }
                    }
                };
                let Some(upstream_response) = upstream_response else {
                    continue;
                };

                let status = upstream_response.status_code;
                if status != 200 {
                    self.metrics.record_upstream_response(
                        endpoint_path,
                        RequestMode::Streaming,
                        status,
                        None,
                    );
                    if is_retryable_provider_status(status) && !is_last {
                        continue;
                    }
                    self.metrics
                        .record_stream_error(endpoint_path, StreamErrorKind::UpstreamNon2xx);
                    let upstream_headers = upstream_response.headers;
                    let upstream_body = collect_upstream_body(upstream_response.body).await?;
                    return Ok(MiddlewareForwardResult::UpstreamError(
                        StreamingUpstreamError {
                            upstream_status: status,
                            upstream_headers,
                            upstream_body,
                        },
                    ));
                }

                // Commit this candidate.
                let upstream_headers = upstream_response.headers;
                let receipt_id = generate_receipt_id();
                let served_at = self.clock.now_secs();
                let recorded = self.record_attested_upstream_session(&recorded_event)?;
                let session_id = recorded.as_ref().map(|(id, _)| id.clone());
                let builder = self.build_middleware_receipt_prefix(
                    &receipt_id,
                    None,
                    served_at,
                    endpoint_path,
                    received_body,
                    &candidate.body,
                    &route_id,
                    &forwarded_body,
                    recorded_event,
                    recorded,
                )?;
                receipt_journal.reserve_receipt_id(receipt_id.clone());

                let body = MiddlewareProviderResponseDraftingStream {
                    inner: upstream_response.body,
                    builder: Some(builder),
                    journal: receipt_journal,
                    provider_response_hasher: Sha256::new(),
                    receipt_id: receipt_id.clone(),
                    endpoint_path: endpoint_path.to_string(),
                    sse_parser: SseChatIdParser::default(),
                    metrics: self.metrics.clone(),
                    upstream_status: status,
                    upstream_ended: false,
                    finished: false,
                };

                return Ok(MiddlewareForwardResult::Stream(Box::new(
                    MiddlewareStreamingForwarded {
                        receipt_id: receipt_id.clone(),
                        upstream_status: status,
                        upstream_headers,
                        body: Box::pin(body),
                        selected_route: route_id.clone(),
                        attempts: index + 1,
                        session_id,
                    },
                )));
            }

            // Buffered forward.
            let mut reverify_attempts = 0;
            let upstream_response = loop {
                match self
                    .upstream
                    .forward_verified_prepared(prepared.clone(), &recorded_event)
                    .await
                {
                    Ok(response) => break Some(response),
                    Err(UpstreamError::ChannelBindingMismatch(_))
                        if !caller_supplied_upstream_event
                            && reverify_attempts < CHANNEL_BINDING_REVERIFY_ATTEMPTS =>
                    {
                        reverify_attempts += 1;
                        match self
                            .refresh_upstream_event(&prepared, candidate_required)
                            .await
                        {
                            Ok(event) => recorded_event = event,
                            // A reverify failure must fail over to the next
                            // candidate, not abort the whole list.
                            Err(err) => {
                                let priority =
                                    if matches!(err, ServiceError::UpstreamVerification(_)) {
                                        3
                                    } else {
                                        2
                                    };
                                upgrade_err(&mut aggregated_err, priority, err);
                                break None;
                            }
                        }
                    }
                    Err(err @ UpstreamError::ChannelBindingMismatch(_)) => {
                        self.invalidate_upstream_event(&prepared, candidate_required);
                        upgrade_err(&mut aggregated_err, 2, err.into());
                        break None;
                    }
                    Err(err) => {
                        upgrade_err(&mut aggregated_err, 2, err.into());
                        break None;
                    }
                }
            };
            let Some(upstream_response) = upstream_response else {
                continue;
            };

            let status = upstream_response.status_code;
            if is_retryable_provider_status(status) && !is_last {
                self.metrics.record_upstream_response(
                    endpoint_path,
                    RequestMode::Buffered,
                    status,
                    None,
                );
                continue;
            }

            // Commit this candidate.
            let response_model = accepted_response_model(status, &upstream_response.body);
            self.metrics.record_upstream_response(
                endpoint_path,
                RequestMode::Buffered,
                status,
                response_model.as_deref(),
            );

            let receipt_id = generate_receipt_id();
            let served_at = self.clock.now_secs();
            let chat_id = extract_chat_id(&upstream_response.body);
            let recorded = self.record_attested_upstream_session(&recorded_event)?;
            let session_id = recorded.as_ref().map(|(id, _)| id.clone());
            let mut builder = self.build_middleware_receipt_prefix(
                &receipt_id,
                chat_id,
                served_at,
                endpoint_path,
                received_body,
                &candidate.body,
                &route_id,
                &forwarded_body,
                recorded_event,
                recorded,
            )?;
            // The session is keyed on the requested (routed) model; record the
            // exact upstream-served model in the receipt's upstream.verified.
            builder.set_upstream_verified_model_id(response_model.clone());
            let provider_response_hash = builder.add_response_received(&upstream_response.body)?;

            return Ok(MiddlewareForwardResult::Forwarded(Box::new(
                MiddlewareForwarded {
                    receipt_id: receipt_id.clone(),
                    receipt: MiddlewareReceiptDraft {
                        receipt_id: receipt_id.clone(),
                        builder,
                        provider_response_hash,
                        endpoint_path: endpoint_path.to_string(),
                        request_mode: RequestMode::Buffered,
                        response_model,
                    },
                    upstream_status: status,
                    upstream_body: upstream_response.body,
                    upstream_headers: upstream_response.headers,
                    selected_route: route_id.clone(),
                    attempts: index + 1,
                    session_id,
                },
            )));
        }

        // No candidate succeeded. Return the highest-priority failure, with
        // the attempted route ids for context.
        Err(aggregated_err.map(|(_, err)| err).unwrap_or_else(|| {
            ServiceError::Upstream(UpstreamError::Routing(format!(
                "all upstream routes failed (attempted: {})",
                candidate_route_ids.join(", ")
            )))
        }))
    }

    /// Start a streaming chat completion. The response stream hashes
    /// every byte in order and stores the receipt only after the
    /// upstream stream completes.
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
        let mut recorded_event = self
            .recorded_upstream_event(
                &prepared,
                req.upstream_required,
                req.upstream_verification_event,
            )
            .await?;

        let mut reverify_attempts = 0;
        let upstream_response = loop {
            match self
                .upstream
                .forward_stream_verified_prepared(prepared.clone(), &recorded_event)
                .await
            {
                Ok(response) => break response,
                Err(UpstreamError::ChannelBindingMismatch(_))
                    if !caller_supplied_upstream_event
                        && reverify_attempts < CHANNEL_BINDING_REVERIFY_ATTEMPTS =>
                {
                    reverify_attempts += 1;
                    recorded_event = self
                        .refresh_upstream_event(&prepared, req.upstream_required)
                        .await?;
                }
                Err(err @ UpstreamError::ChannelBindingMismatch(_))
                    if !caller_supplied_upstream_event =>
                {
                    self.invalidate_upstream_event(&prepared, req.upstream_required);
                    return Err(err.into());
                }
                Err(err) => return Err(err.into()),
            }
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
        let recorded = self.record_attested_upstream_session(&recorded_event)?;
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

        let body = ReceiptFinalizingStream {
            inner: upstream_response.body,
            builder: Some(builder),
            cleartext_hasher: Sha256::new(),
            wire_hasher: Sha256::new(),
            keys: self.keys.clone(),
            receipt_store: self.receipt_store.clone(),
            key_id: self.default_receipt_key_id.clone(),
            requester: req.requester,
            receipt_ttl_seconds: self.config.receipt_ttl_seconds,
            clock: self.clock.clone(),
            metrics: self.metrics.clone(),
            endpoint_path: endpoint_path.to_string(),
            sse_parser: SseChatIdParser::default(),
            e2ee_transformer,
            response_modified,
            upstream_ended: false,
            finished: false,
        };

        Ok(StreamingForwardResult::Stream(StreamingForwardStream {
            receipt_id,
            upstream_status: upstream_response.status_code,
            upstream_headers: upstream_response.headers,
            e2ee: e2ee_response,
            body: Box::pin(body),
        }))
    }

    pub fn finalize_middleware_receipt(
        &self,
        mut draft: MiddlewareReceiptDraft,
        final_cleartext_body: &[u8],
        content_type: Option<&str>,
        requester: Option<ReceiptOwner>,
        e2ee: Option<E2eeRequestContext>,
    ) -> Result<MiddlewareReceiptFinalization, ServiceError> {
        let is_sse = is_sse_content_type(content_type);
        if is_sse {
            let mut parser = SseChatIdParser::default();
            parser.observe(final_cleartext_body);
            if parser.chat_id.is_some() {
                draft.builder.set_chat_id(parser.chat_id);
            }
        } else if let Some(chat_id) = extract_chat_id(final_cleartext_body) {
            draft.builder.set_chat_id(Some(chat_id));
        }

        let wire_body = match e2ee.as_ref() {
            Some(ctx) => encrypt_e2ee_final_response(
                final_cleartext_body,
                ctx,
                &draft.endpoint_path,
                is_sse,
            )?,
            None => final_cleartext_body.to_vec(),
        };
        let e2ee_response = e2ee.as_ref().map(|ctx| E2eeResponseInfo {
            version: ctx.version.clone(),
            algo: ctx.algo.clone(),
        });

        let final_cleartext_hash = crate::aci::canonical::sha256_hex(final_cleartext_body);
        if draft.provider_response_hash != final_cleartext_hash || wire_body != final_cleartext_body
        {
            draft
                .builder
                .add_transparency_event(TransparencyEventKind::ResponseModified)?;
        }
        draft
            .builder
            .add_response_returned(final_cleartext_body, &wire_body)?;
        let receipt = draft
            .builder
            .finalize(self.keys.as_ref(), &self.default_receipt_key_id)?;
        self.store_receipt(receipt.clone(), requester);
        self.metrics.record_receipt_issued(
            &draft.endpoint_path,
            draft.request_mode,
            draft.response_model.as_deref(),
        );

        Ok(MiddlewareReceiptFinalization {
            receipt,
            wire_body,
            e2ee: e2ee_response,
        })
    }

    pub fn finalize_middleware_generated_response(
        &self,
        endpoint_path: &str,
        cleartext_body: &[u8],
        content_type: Option<&str>,
        e2ee: Option<E2eeRequestContext>,
    ) -> Result<MiddlewareGeneratedFinalization, ServiceError> {
        let is_sse = is_sse_content_type(content_type);
        let wire_body = match e2ee.as_ref() {
            Some(ctx) => encrypt_e2ee_final_response(cleartext_body, ctx, endpoint_path, is_sse)?,
            None => cleartext_body.to_vec(),
        };
        let e2ee_response = e2ee.as_ref().map(|ctx| E2eeResponseInfo {
            version: ctx.version.clone(),
            algo: ctx.algo.clone(),
        });
        Ok(MiddlewareGeneratedFinalization {
            wire_body,
            e2ee: e2ee_response,
        })
    }

    pub fn finalize_middleware_response_stream(
        &self,
        journal: MiddlewareReceiptJournal,
        cleartext_stream: ServiceResponseStream,
        endpoint_path: &str,
        content_type: Option<&str>,
        requester: Option<ReceiptOwner>,
        e2ee: Option<E2eeRequestContext>,
    ) -> Result<MiddlewareStreamFinalization, ServiceError> {
        let is_sse = is_sse_content_type(content_type);
        if e2ee.is_some() && !is_sse {
            return Err(E2eeError::EncryptionFailed.into());
        }
        let e2ee_response = e2ee.as_ref().map(|ctx| E2eeResponseInfo {
            version: ctx.version.clone(),
            algo: ctx.algo.clone(),
        });
        let e2ee_transformer = e2ee
            .clone()
            .map(|ctx| E2eeSseTransformer::new(ctx, endpoint_path.to_string()));
        let body = MiddlewareResponseFinalizingStream {
            inner: cleartext_stream,
            journal,
            cleartext_hasher: Sha256::new(),
            wire_hasher: Sha256::new(),
            keys: self.keys.clone(),
            receipt_store: self.receipt_store.clone(),
            key_id: self.default_receipt_key_id.clone(),
            requester,
            receipt_ttl_seconds: self.config.receipt_ttl_seconds,
            clock: self.clock.clone(),
            metrics: self.metrics.clone(),
            endpoint_path: endpoint_path.to_string(),
            sse_parser: SseChatIdParser::default(),
            e2ee_transformer,
            response_modified_by_wire: e2ee_response.is_some(),
            upstream_ended: false,
            finished: false,
        };
        Ok(MiddlewareStreamFinalization {
            body: Box::pin(body),
            e2ee: e2ee_response,
        })
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
            provider: None,
            model_id: prepared.model_id.clone(),
            url_origin: prepared.url_origin.clone(),
            verifier_id: "none".to_string(),
            result: VerificationResult::Failed,
            required: upstream_required,
            reason: Some("no upstream verifier configured".to_string()),
            evidence: None,
            channel_bindings: Vec::new(),
            provider_claims: None,
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

    /// Seal + persist the attested session for a verified event, and return its
    /// `(session_id, claim-verdicts)`. The verdicts are surfaced inline in the
    /// receipt's `upstream.verified` (shallow audit), while the persisted session
    /// also carries the evidence + reasons (deep audit).
    pub(super) fn record_attested_upstream_session(
        &self,
        event: &UpstreamVerifiedEvent,
    ) -> Result<Option<(String, SessionClaims)>, ServiceError> {
        if event.result != VerificationResult::Verified || event.channel_bindings.is_empty() {
            return Ok(None);
        }

        let now = self.clock.now_secs();
        // Retention window (`receipt_ttl_seconds`), so a relying party verifying a
        // citing receipt can resolve its `session_id`. The session is sealed
        // slightly before its receipt, so it expires up to one request-processing
        // interval (sub-second) sooner than that receipt — both use the same TTL
        // off a per-call `now`. This is a retention deadline, not a binding
        // validity one (the forwarding path only ever uses a fresh lease).
        let expires_at = now.saturating_add(self.config.receipt_ttl_seconds);

        let channel_binding = AttestedSession::bindings_to_values(&event.channel_bindings);
        let claims = session_claims_for_event(event);

        // Lift the response-signing address into the verified identity when present.
        let mut identity = WorkloadIdentityRef::default();
        if let Some(Value::Object(map)) = event.provider_claims.as_ref() {
            if let Some(addr) = map.get("signing_address").and_then(Value::as_str) {
                identity.signing_address = Some(addr.to_string());
            }
        }
        let identity = (!identity.is_empty()).then_some(identity);

        let evidence = event
            .evidence
            .as_ref()
            .map(EvidenceRef::from_value)
            .unwrap_or_default();

        let session = AttestedSession::seal(
            event.upstream_name.clone(),
            event.url_origin.clone(),
            event.verifier_id.clone(),
            identity,
            channel_binding,
            claims.clone(),
            evidence,
            now,
            expires_at,
        )?;

        let session_id = session.session_id.clone();
        if let Err(err) = self.session_store.put_session(session, now) {
            // Persisting the audit record must not break inference; a missing
            // session simply resolves to "not found" for relying parties.
            tracing::warn!(error = %err, session_id = %session_id, "failed to persist attested session");
        }
        Ok(Some((session_id, claims)))
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

    pub(super) fn store_receipt(&self, receipt: Receipt, requester: Option<ReceiptOwner>) {
        let now = self.clock.now_secs();
        let expires_at = now.saturating_add(self.config.receipt_ttl_seconds);
        self.receipt_store.put(receipt, requester, expires_at);
    }
}
