//! The middleware seam: forwarding a request on behalf of the
//! middleware and finalizing the receipt/response it returns.
//!

use super::e2ee_crypto::{encrypt_e2ee_final_response, is_sse_content_type};
use super::forward::{attested_route_eligible, cite_served_session, ReverifyOutcome};
use super::helpers::{
    accepted_response_model, collect_upstream_body, extract_chat_id, generate_receipt_id,
};
use super::streaming::{
    E2eeSseTransformer, MiddlewareProviderResponseDraftingStream,
    MiddlewareResponseFinalizingStream,
};
use super::{
    AciService, ChatCompletionRequest, E2eeError, E2eeRequestContext, E2eeResponseInfo,
    ForwardCandidate, MiddlewareAllFailed, MiddlewareForwardResult, MiddlewareForwarded,
    MiddlewareGeneratedFinalization, MiddlewareReceiptDraft, MiddlewareReceiptFinalization,
    MiddlewareReceiptJournal, MiddlewareStreamFinalization, MiddlewareStreamingForwarded,
    MiddlewareUpstreamError, ReceiptOwner, ServiceError, ServiceResponseStream,
    StreamingUpstreamError, UpstreamVerificationError,
};
use crate::aci::receipt::{ReceiptBuilder, TransparencyEventKind, UpstreamVerifiedEvent};
use crate::aci::upstream::{UpstreamError, UpstreamRequest, UpstreamResponse};
use crate::aggregator::metrics::{RequestMode, StreamErrorKind};
use crate::aggregator::session::SessionClaims;
use crate::sse_framing::SseFramingObserver;
use std::collections::HashMap;

// Provider statuses that make this candidate worth abandoning for the next one.
// Beyond the transient 429/5xx signals, an auth/account failure specific to this
// provider — 401 (invalid key), 402 (out of credit), 403 (key lacks access) — can
// still be served by a sibling candidate on a different account. Request-level 4xx
// (400/404/422) are excluded: they would fail identically on every candidate.
fn is_retryable_provider_status(status: u16) -> bool {
    matches!(status, 401 | 402 | 403 | 429 | 500 | 502 | 503 | 504)
}

// Whether to abandon this candidate and try the next. The status must be a
// provider-specific/transient failure AND the error must not be the client's own
// fault: a fetch failure on a client-supplied image URL fails identically on every
// candidate, so it is terminal (committed and surfaced as a 400) rather than retried.
fn should_fail_over(status: u16, received_body: &[u8], upstream_body: &[u8]) -> bool {
    is_retryable_provider_status(status)
        && crate::middleware::errors::classify_image_input_error(
            received_body,
            status,
            upstream_body,
        )
        .is_none()
}

/// Track the highest-priority failover error so that, when every candidate
/// fails, the returned error reflects the most informative failure.
/// Priority order: verification (3), then transport (2), then routing (1),
/// then a route the ACI constraint made ineligible (0) — it never got the
/// chance to fail, so any real failure must outrank it in either order.
fn upgrade_err(slot: &mut Option<(u8, ServiceError)>, priority: u8, err: ServiceError) {
    if slot.as_ref().map(|(p, _)| priority >= *p).unwrap_or(true) {
        *slot = Some((priority, err));
    }
}

/// A candidate's real upstream response, held back while the failover walk
/// keeps looking for a 2xx. A later candidate may never reach an upstream at
/// all (unroutable, unverified, transport error); without retention its
/// failure would overwrite this answer and the client's status would depend on
/// candidate order.
enum RetainedResponse {
    /// Relayed as an upstream error — a stream that never completed has no
    /// receipt to bind.
    Streaming {
        error: StreamingUpstreamError,
        route_id: String,
        attempt_slot: usize,
    },
    /// Committed like any other buffered response, receipt included.
    Buffered {
        inputs: Box<BufferedCommit>,
        attempt_slot: usize,
    },
}

impl RetainedResponse {
    /// Where this candidate's own entry sits in `failed_attempts`. Removing it
    /// on commit keeps the committed attempt last: attempts are reported by
    /// position, and a request's user-facing status is read as the one at the
    /// highest attempt index.
    fn attempt_slot(&self) -> usize {
        match self {
            Self::Streaming { attempt_slot, .. } | Self::Buffered { attempt_slot, .. } => {
                *attempt_slot
            }
        }
    }
}

/// Everything [`AciService::commit_buffered_response`] needs to turn one
/// buffered upstream response into a committed result.
pub(super) struct BufferedCommit {
    pub response: UpstreamResponse,
    /// Resolved where the response arrived, alongside its one metrics count.
    pub response_model: Option<String>,
    pub recorded_event: UpstreamVerifiedEvent,
    pub route_id: String,
    pub middleware_forwarded_body: Vec<u8>,
    pub forwarded_body: Vec<u8>,
}

/// The request/response context observed for one forwarded candidate,
/// captured inside the TEE. Grouped so
/// [`AciService::build_middleware_receipt_prefix`] reads by field name rather
/// than ten positional arguments.
pub(super) struct MiddlewareReceiptInputs<'a> {
    pub receipt_id: &'a str,
    pub chat_id: Option<String>,
    /// The user-requested model (received request's top-level `model`), recorded
    /// as the receipt's top-level `model`; `None` when the request carried none.
    pub model: Option<String>,
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
            model,
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
            model,
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

    /// Turn one buffered upstream response into a committed result: seal the
    /// attested session, build the receipt, and report the attempts that
    /// preceded it. Shared by the in-loop commit and the retained-response
    /// commit after the walk.
    fn commit_buffered_response(
        &self,
        commit: BufferedCommit,
        failed_attempts: Vec<(String, u16)>,
        endpoint_path: &str,
        received_body: &[u8],
        user_model: Option<String>,
    ) -> Result<MiddlewareForwardResult, ServiceError> {
        let BufferedCommit {
            response,
            response_model,
            recorded_event,
            route_id,
            middleware_forwarded_body,
            forwarded_body,
        } = commit;
        let status = response.status_code;

        let receipt_id = generate_receipt_id();
        let served_at = self.clock.now_secs();
        let chat_id = extract_chat_id(&response.body);
        let sealed = self.record_attested_upstream_session(&recorded_event)?;
        let recorded = cite_served_session(&sealed, response.served_instance_id.as_deref());
        let session_id = recorded.as_ref().map(|(id, _)| id.clone());
        let mut builder = self.build_middleware_receipt_prefix(MiddlewareReceiptInputs {
            receipt_id: &receipt_id,
            chat_id,
            model: user_model,
            served_at,
            endpoint_path,
            received_body,
            middleware_forwarded_body: &middleware_forwarded_body,
            selected_route_id: &route_id,
            forwarded_body: &forwarded_body,
            recorded_event,
            recorded,
        })?;
        // The session is keyed on the requested (routed) model; record the
        // exact upstream-served model in the receipt's upstream.verified.
        builder.set_upstream_verified_model_id(response_model.clone());
        let provider_response_hash = builder.add_response_received(&response.body)?;

        Ok(MiddlewareForwardResult::Forwarded(Box::new(
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
                upstream_body: response.body,
                upstream_headers: response.headers,
                selected_route: route_id,
                failed_attempts,
                session_id,
            },
        )))
    }

    pub async fn forward_chat_completion_for_middleware(
        &self,
        req: ChatCompletionRequest<'_>,
        candidates: Vec<ForwardCandidate>,
        stream: bool,
        receipt_journal: MiddlewareReceiptJournal,
    ) -> Result<MiddlewareForwardResult, ServiceError> {
        let aci_required = req.requires_aci_verification();
        let received_body = req.received_body;
        let endpoint_path = req.endpoint_path;
        // The user-requested model, recorded as the receipt's top-level `model`.
        let user_model = req.context.user_model.clone();
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

        // Candidates that failed and were failed over, as (route_id, status),
        // in the order tried. The committed route is carried separately via
        // `selected_route`; these are surfaced to the caller so every attempt
        // is observable, not just the one that served the response. Non-HTTP
        // failures (prepare/verification/transport) record 502.
        let mut failed_attempts: Vec<(String, u16)> = Vec::new();

        // The most recent candidate that actually answered, held back in case
        // nothing better follows. See [`RetainedResponse`].
        let mut retained: Option<RetainedResponse> = None;

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
                    failed_attempts.push((route_id.clone(), 502));
                    upgrade_err(
                        &mut aggregated_err,
                        1,
                        ServiceError::Upstream(UpstreamError::Routing(message)),
                    );
                    continue;
                }
                Err(err) => {
                    failed_attempts.push((route_id.clone(), 502));
                    upgrade_err(&mut aggregated_err, 2, err.into());
                    continue;
                }
            };

            // A route not known to be attested cannot serve an
            // `aci_verified` request. Kept out of `failed_attempts`: this is a
            // policy decision, and those are reported per route, which would
            // charge a provider for a request it never saw.
            if aci_required && !attested_route_eligible(prepared.is_tee) {
                upgrade_err(
                    &mut aggregated_err,
                    0,
                    ServiceError::UpstreamVerification(
                        UpstreamVerificationError::NoEligibleAttestedRoute(
                            user_model.clone().unwrap_or_default(),
                        ),
                    ),
                );
                continue;
            }

            // Per-route fail-closed mode. Under `aci_required` it cannot be
            // waived: a request pinned to an attested upstream must not also
            // opt out of the attestation.
            let candidate_required =
                self.upstream_required_for_prepared(&prepared, req.upstream_required, aci_required);

            let mut recorded_event = match self
                .recorded_upstream_event(&prepared, candidate_required, single_caller_event.clone())
                .await
            {
                Ok(event) => event,
                Err(ServiceError::UpstreamVerification(uv)) => {
                    failed_attempts.push((route_id.clone(), 502));
                    upgrade_err(
                        &mut aggregated_err,
                        3,
                        ServiceError::UpstreamVerification(uv),
                    );
                    continue;
                }
                Err(err) => return Err(err),
            };

            if let Err(err) = self.apply_aci_session_constraint(
                &mut recorded_event,
                &req.aci_session_ids,
                &prepared.model_id,
            ) {
                if matches!(
                    err,
                    ServiceError::UpstreamVerification(
                        UpstreamVerificationError::NoEligibleAttestedSession(_)
                    )
                ) {
                    upgrade_err(&mut aggregated_err, 0, err);
                    continue;
                }
                return Err(err);
            }

            let forwarded_body = prepared.request.body.clone();

            if stream {
                let upstream_response = match self
                    .forward_with_binding_reverify(
                        &prepared,
                        &mut recorded_event,
                        candidate_required,
                        caller_supplied_upstream_event,
                        &req.aci_session_ids,
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
                    failed_attempts.push((route_id.clone(), 502));
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
                    // Collect the (small) error body up front so the failover
                    // decision can inspect it. A truncated/unreadable error body
                    // must not abort the remaining candidates, so it degrades to
                    // empty — the caller's normalizer emits its generic message.
                    let upstream_headers = upstream_response.headers;
                    let upstream_body = collect_upstream_body(upstream_response.body)
                        .await
                        .unwrap_or_default();
                    if !is_last && should_fail_over(status, received_body, &upstream_body) {
                        retained = Some(RetainedResponse::Streaming {
                            error: StreamingUpstreamError {
                                upstream_status: status,
                                upstream_headers,
                                upstream_body,
                            },
                            route_id: route_id.clone(),
                            attempt_slot: failed_attempts.len(),
                        });
                        failed_attempts.push((route_id.clone(), status));
                        continue;
                    }
                    self.metrics
                        .record_stream_error(endpoint_path, StreamErrorKind::UpstreamNon2xx);
                    return Ok(MiddlewareForwardResult::UpstreamError(Box::new(
                        MiddlewareUpstreamError {
                            error: StreamingUpstreamError {
                                upstream_status: status,
                                upstream_headers,
                                upstream_body,
                            },
                            selected_route: route_id,
                            failed_attempts,
                        },
                    )));
                }

                // Commit this candidate.
                let upstream_headers = upstream_response.headers;
                let receipt_id = generate_receipt_id();
                let served_at = self.clock.now_secs();
                let sealed = self.record_attested_upstream_session(&recorded_event)?;
                let recorded =
                    cite_served_session(&sealed, upstream_response.served_instance_id.as_deref());
                let session_id = recorded.as_ref().map(|(id, _)| id.clone());
                let builder = self.build_middleware_receipt_prefix(MiddlewareReceiptInputs {
                    receipt_id: &receipt_id,
                    chat_id: None,
                    model: user_model.clone(),
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
                        failed_attempts: std::mem::take(&mut failed_attempts),
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
                    &req.aci_session_ids,
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
                failed_attempts.push((route_id.clone(), 502));
                continue;
            };

            // Counted once, here, where the response arrives — a response that is
            // held back and committed after the walk must not be counted again
            // on commit.
            let status = upstream_response.status_code;
            let response_model = accepted_response_model(status, &upstream_response.body);
            self.metrics.record_upstream_response(
                endpoint_path,
                RequestMode::Buffered,
                status,
                response_model.as_deref(),
            );
            if !is_last && should_fail_over(status, received_body, &upstream_response.body) {
                retained = Some(RetainedResponse::Buffered {
                    inputs: Box::new(BufferedCommit {
                        response: upstream_response,
                        response_model,
                        recorded_event,
                        route_id: route_id.clone(),
                        middleware_forwarded_body: candidate.body.clone(),
                        forwarded_body,
                    }),
                    attempt_slot: failed_attempts.len(),
                });
                failed_attempts.push((route_id.clone(), status));
                continue;
            }

            // Commit this candidate.
            return self.commit_buffered_response(
                BufferedCommit {
                    response: upstream_response,
                    response_model,
                    recorded_event,
                    route_id,
                    middleware_forwarded_body: candidate.body.clone(),
                    forwarded_body,
                },
                std::mem::take(&mut failed_attempts),
                endpoint_path,
                received_body,
                user_model.clone(),
            );
        }

        // No candidate produced a 2xx, but one may still have answered — commit
        // that rather than a status synthesized from a candidate that never
        // reached an upstream. Its own entry leaves `failed_attempts` so the
        // committed attempt is once again the last one reported.
        if let Some(retained) = retained {
            let mut failed_attempts = failed_attempts;
            failed_attempts.remove(retained.attempt_slot());
            return match retained {
                RetainedResponse::Streaming {
                    error, route_id, ..
                } => {
                    // Counted here rather than where the response arrived: this
                    // is the point at which it becomes the stream's outcome,
                    // matching the in-loop return below.
                    self.metrics
                        .record_stream_error(endpoint_path, StreamErrorKind::UpstreamNon2xx);
                    Ok(MiddlewareForwardResult::UpstreamError(Box::new(
                        MiddlewareUpstreamError {
                            error,
                            selected_route: route_id,
                            failed_attempts,
                        },
                    )))
                }
                RetainedResponse::Buffered { inputs, .. } => self.commit_buffered_response(
                    *inputs,
                    failed_attempts,
                    endpoint_path,
                    received_body,
                    user_model,
                ),
            };
        }

        // No candidate answered at all. Return the highest-priority failure
        // together with every attempt's outcome — the caller reports the
        // attempts (they are unrecoverable from the error alone) and derives
        // the client status from the failure mix.
        let error = aggregated_err.map(|(_, err)| err).unwrap_or_else(|| {
            ServiceError::Upstream(UpstreamError::Routing(format!(
                "all upstream routes failed (attempted: {})",
                candidate_route_ids.join(", ")
            )))
        });
        Ok(MiddlewareForwardResult::AllFailed(Box::new(
            MiddlewareAllFailed {
                failed_attempts,
                error,
            },
        )))
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
            let mut parser = SseFramingObserver::identifiers_only();
            parser.observe(final_cleartext_body);
            if parser.chat_id().is_some() {
                draft.builder.set_chat_id(parser.chat_id());
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

    #[allow(clippy::too_many_arguments)]
    pub fn finalize_middleware_response_stream(
        &self,
        journal: MiddlewareReceiptJournal,
        cleartext_stream: ServiceResponseStream,
        endpoint_path: &str,
        content_type: Option<&str>,
        requester: Option<ReceiptOwner>,
        e2ee: Option<E2eeRequestContext>,
        request_id: Option<String>,
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
            request_id,
            is_sse,
        );
        Ok(MiddlewareStreamFinalization {
            body: Box::pin(body),
            e2ee: e2ee_response,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::is_retryable_provider_status;

    #[test]
    fn retryable_covers_transient_and_account_specific_statuses() {
        // Transient provider trouble (429/5xx) plus auth/account failures
        // (401 invalid key, 402 out of credit, 403 no access) fail over.
        for status in [401, 402, 403, 429, 500, 502, 503, 504] {
            assert!(
                is_retryable_provider_status(status),
                "{status} should retry"
            );
        }
        // Request-level errors would fail identically on every candidate.
        for status in [400, 404, 422] {
            assert!(
                !is_retryable_provider_status(status),
                "{status} should not retry"
            );
        }
    }
}
