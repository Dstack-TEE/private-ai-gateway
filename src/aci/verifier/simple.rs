//! Simple upstream verifiers: static and preverified.

use async_trait::async_trait;

use crate::aci::receipt::{UpstreamVerifiedEvent, VerificationResult};
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};

/// Returns a caller-supplied event verbatim. The event's `required`
/// field is overwritten by the service to reflect the client's
/// effective verification mode (see `forward_chat_completion_request`),
/// so the caller's value there is advisory.
pub struct StaticUpstreamVerifier {
    event: UpstreamVerifiedEvent,
}

impl StaticUpstreamVerifier {
    pub fn new(event: UpstreamVerifiedEvent) -> Self {
        Self { event }
    }

    /// Convenience: build a `verified` event tagged with a fixed
    /// `verifier_id`.
    pub fn verified(verifier_id: impl Into<String>) -> Self {
        Self::new(UpstreamVerifiedEvent {
            upstream_name: String::new(),
            model_id: String::new(),
            verifier_id: verifier_id.into(),
            result: VerificationResult::Verified,
            required: true,
            ..Default::default()
        })
    }

    /// Convenience: build a `failed` event tagged with a fixed reason.
    pub fn failed(verifier_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(UpstreamVerifiedEvent {
            upstream_name: String::new(),
            model_id: String::new(),
            verifier_id: verifier_id.into(),
            result: VerificationResult::Failed,
            required: true,
            reason: Some(reason.into()),
            ..Default::default()
        })
    }

    fn event_for(&self, request: &UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        // Populate the vendor / model_id / url_origin fields from the
        // request when the static event left them blank, so a static
        // configuration does not erase per-request context that helps
        // downstream verifiers.
        let mut event = self.event.clone();
        if event.upstream_name.is_empty() {
            event.upstream_name = request.upstream_name.clone();
        }
        if event.model_id.is_empty() {
            event.model_id = request.model_id.clone();
        }
        if event.url_origin.is_none() {
            event.url_origin = request.url_origin.clone();
        }
        event
    }
}

#[async_trait]
impl UpstreamVerifier for StaticUpstreamVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.event_for(&request)
    }

    fn cached(&self, request: &UpstreamVerificationRequest) -> Option<UpstreamVerifiedEvent> {
        Some(self.event_for(request))
    }
}

/// Returns a `verified` event whose vendor / model_id / url_origin are
/// taken directly from the per-request [`UpstreamVerificationRequest`].
/// Useful as a placeholder verifier when the aggregator is in a
/// deployment where the upstream is already trusted out-of-band and the
/// only thing ACI needs is a deterministic `verifier_id` traceable to
/// the aggregator's source provenance.
pub struct PreverifiedUpstreamVerifier {
    verifier_id: String,
}

impl PreverifiedUpstreamVerifier {
    pub fn new(verifier_id: impl Into<String>) -> Self {
        Self {
            verifier_id: verifier_id.into(),
        }
    }

    fn event_for(&self, request: &UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        UpstreamVerifiedEvent {
            upstream_name: request.upstream_name.clone(),
            model_id: request.model_id.clone(),
            url_origin: request.url_origin.clone(),
            verifier_id: self.verifier_id.clone(),
            result: VerificationResult::Verified,
            required: request.required,
            ..Default::default()
        }
    }
}

#[async_trait]
impl UpstreamVerifier for PreverifiedUpstreamVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        self.event_for(&request)
    }

    fn cached(&self, request: &UpstreamVerificationRequest) -> Option<UpstreamVerifiedEvent> {
        Some(self.event_for(request))
    }
}
