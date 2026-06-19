//! The optional UDS-middleware split path: forwarding a request to the
//! external middleware and finalizing the receipt/response it returns.
//!

use super::e2ee_crypto::{encrypt_e2ee_final_response, is_sse_content_type};
use super::forward::ReverifyOutcome;
use super::helpers::{
    accepted_response_model, collect_upstream_body, extract_chat_id, generate_receipt_id,
};
use super::streaming::{
    E2eeSseTransformer, MiddlewareProviderResponseDraftingStream,
    MiddlewareResponseFinalizingStream, SseChatIdParser,
};
use super::{
    AciService, ChatCompletionRequest, E2eeError, E2eeRequestContext, E2eeResponseInfo,
    ForwardCandidate, MiddlewareForwardResult, MiddlewareForwarded,
    MiddlewareGeneratedFinalization, MiddlewareReceiptDraft, MiddlewareReceiptFinalization,
    MiddlewareReceiptJournal, MiddlewareStreamFinalization, MiddlewareStreamingForwarded,
    ReceiptOwner, ServiceError, ServiceResponseStream, StreamingUpstreamError,
};
use crate::aci::receipt::{ReceiptBuilder, TransparencyEventKind, UpstreamVerifiedEvent};
use crate::aci::upstream::{UpstreamError, UpstreamRequest};
use crate::aggregator::metrics::{RequestMode, StreamErrorKind};
use crate::aggregator::session::SessionClaims;
use std::collections::HashMap;

fn is_retryable_provider_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

/// Track the highest-priority failover error so that, when every candidate
/// fails, the returned error reflects the most informative failure.
/// Priority order: verification (3), then transport (2), then routing (1).
fn upgrade_err(slot: &mut Option<(u8, ServiceError)>, priority: u8, err: ServiceError) {
    if slot.as_ref().map(|(p, _)| priority >= *p).unwrap_or(true) {
        *slot = Some((priority, err));
    }
}

/// The request/response context observed for one forwarded candidate,
/// captured inside the TEE. Grouped so
/// [`AciService::build_middleware_receipt_prefix`] reads by field name rather
/// than ten positional arguments.
pub(super) struct MiddlewareReceiptInputs<'a> {
    pub receipt_id: &'a str,
    pub chat_id: Option<String>,
    pub served_at: u64,
    pub endpoint_path: &'a str,
    pub received_body: &'a [u8],
    pub middleware_forwarded_body: &'a [u8],
    pub selected_route_id: &'a str,
    pub forwarded_body: &'a [u8],
    pub recorded_event: UpstreamVerifiedEvent,
    pub recorded: Option<(String, SessionClaims)>,
}

impl AciService {
    pub(super) fn build_middleware_receipt_prefix(
        &self,
        inputs: MiddlewareReceiptInputs<'_>,
    ) -> Result<ReceiptBuilder, ServiceError> {
        let MiddlewareReceiptInputs {
            receipt_id,
            chat_id,
            served_at,
            endpoint_path,
            received_body,
            middleware_forwarded_body,
            selected_route_id,
            forwarded_body,
            recorded_event,
            recorded,
        } = inputs;
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

        // Optional x-user-tier passed through to every upstream attempt.
        let mut upstream_headers: HashMap<String, String> = HashMap::new();
        if let Some(tier) = req.context.user_tier.as_deref() {
            upstream_headers.insert("x-user-tier".to_string(), tier.to_string());
        }

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
                headers: upstream_headers.clone(),
                path: Some(endpoint_path.to_string()),
                target_route_id: Some(route_id.clone()),
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
                let upstream_response = match self
                    .forward_with_binding_reverify(
                        &prepared,
                        &mut recorded_event,
                        candidate_required,
                        caller_supplied_upstream_event,
                        // Failover path: flush a possibly-stale binding on any
                        // terminal mismatch so the next candidate/request re-verifies.
                        true,
                        |prepared, event| async move {
                            self.upstream
                                .forward_stream_verified_prepared(prepared, &event)
                                .await
                        },
                    )
                    .await
                {
                    ReverifyOutcome::Forwarded(response) => Some(response),
                    ReverifyOutcome::RefreshFailed(err) => {
                        let priority = if matches!(err, ServiceError::UpstreamVerification(_)) {
                            3
                        } else {
                            2
                        };
                        upgrade_err(&mut aggregated_err, priority, err);
                        None
                    }
                    ReverifyOutcome::Failed(err) => {
                        // Terminal binding mismatch and transport errors
                        // intentionally share failover priority 2 (a failed
                        // reverify outranks them at 3).
                        upgrade_err(&mut aggregated_err, 2, err.into());
                        None
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
                let builder = self.build_middleware_receipt_prefix(MiddlewareReceiptInputs {
                    receipt_id: &receipt_id,
                    chat_id: None,
                    served_at,
                    endpoint_path,
                    received_body,
                    middleware_forwarded_body: &candidate.body,
                    selected_route_id: &route_id,
                    forwarded_body: &forwarded_body,
                    recorded_event,
                    recorded,
                })?;
                receipt_journal.reserve_receipt_id(receipt_id.clone());

                let body = MiddlewareProviderResponseDraftingStream::new(
                    upstream_response.body,
                    builder,
                    receipt_journal,
                    receipt_id.clone(),
                    endpoint_path.to_string(),
                    self.metrics.clone(),
                    status,
                );

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
            let upstream_response = match self
                .forward_with_binding_reverify(
                    &prepared,
                    &mut recorded_event,
                    candidate_required,
                    caller_supplied_upstream_event,
                    // Failover path: flush a possibly-stale binding on any
                    // terminal mismatch so the next candidate/request re-verifies.
                    true,
                    |prepared, event| async move {
                        self.upstream
                            .forward_verified_prepared(prepared, &event)
                            .await
                    },
                )
                .await
            {
                ReverifyOutcome::Forwarded(response) => Some(response),
                ReverifyOutcome::RefreshFailed(err) => {
                    let priority = if matches!(err, ServiceError::UpstreamVerification(_)) {
                        3
                    } else {
                        2
                    };
                    upgrade_err(&mut aggregated_err, priority, err);
                    None
                }
                ReverifyOutcome::Failed(err) => {
                    // Terminal binding mismatch and transport errors
                    // intentionally share failover priority 2 (a failed
                    // reverify outranks them at 3).
                    upgrade_err(&mut aggregated_err, 2, err.into());
                    None
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
            let mut builder = self.build_middleware_receipt_prefix(MiddlewareReceiptInputs {
                receipt_id: &receipt_id,
                chat_id,
                served_at,
                endpoint_path,
                received_body,
                middleware_forwarded_body: &candidate.body,
                selected_route_id: &route_id,
                forwarded_body: &forwarded_body,
                recorded_event,
                recorded,
            })?;
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
        let body = MiddlewareResponseFinalizingStream::new(
            self,
            cleartext_stream,
            journal,
            requester,
            endpoint_path.to_string(),
            e2ee_transformer,
            e2ee_response.is_some(),
        );
        Ok(MiddlewareStreamFinalization {
            body: Box::pin(body),
            e2ee: e2ee_response,
        })
    }
}
