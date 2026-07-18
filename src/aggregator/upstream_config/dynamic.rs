//! Dynamic backend/verifier wrappers over the swappable upstream config state.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde_json::json;

use super::ConfiguredUpstreams;
use crate::aci::receipt::{UpstreamVerifiedEvent, VerificationResult};
use crate::aci::upstream::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
    UpstreamStreamResponse,
};
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};

pub(super) struct EmptyUpstreamBackend;

#[async_trait]
impl UpstreamBackend for EmptyUpstreamBackend {
    fn name(&self) -> &str {
        "unconfigured"
    }

    fn url_origin(&self) -> Option<&str> {
        None
    }

    fn prepare(&self, _req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        Err(UpstreamError::Routing(
            "no upstreams configured".to_string(),
        ))
    }

    async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        Err(UpstreamError::Routing(
            "no upstreams configured".to_string(),
        ))
    }

    async fn forward_stream(
        &self,
        _req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        Err(UpstreamError::Routing(
            "no upstreams configured".to_string(),
        ))
    }

    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        Ok(UpstreamResponse {
            status_code: 200,
            body: serde_json::to_vec(&json!({"object": "list", "data": []}))
                .map_err(|e| UpstreamError::Routing(e.to_string()))?,
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
            served_instance_id: None,
            provider_response_claims: None,
        })
    }
}

pub(super) struct DynamicUpstreamBackend {
    pub(super) state: Arc<RwLock<Arc<ConfiguredUpstreams>>>,
}

impl DynamicUpstreamBackend {
    fn backend(&self) -> Arc<dyn UpstreamBackend> {
        self.state
            .read()
            .expect("upstream config manager state poisoned")
            .backend
            .clone()
    }
}

#[async_trait]
impl UpstreamBackend for DynamicUpstreamBackend {
    fn name(&self) -> &str {
        "dynamic-upstream-config"
    }

    fn url_origin(&self) -> Option<&str> {
        None
    }

    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        self.backend().prepare(req)
    }

    async fn forward(&self, req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        self.backend().forward(req).await
    }

    async fn forward_prepared(
        &self,
        req: PreparedUpstreamRequest,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.backend().forward_prepared(req).await
    }

    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        self.backend().forward_verified_prepared(req, event).await
    }

    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        self.backend().models().await
    }

    async fn forward_stream(
        &self,
        req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.backend().forward_stream(req).await
    }

    async fn forward_stream_prepared(
        &self,
        req: PreparedUpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.backend().forward_stream_prepared(req).await
    }

    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        self.backend()
            .forward_stream_verified_prepared(req, event)
            .await
    }

    async fn chutes_attestation_report(
        &self,
        model: &str,
    ) -> Result<serde_json::Value, UpstreamError> {
        self.backend().chutes_attestation_report(model).await
    }
}

pub(super) struct DynamicUpstreamVerifier {
    pub(super) state: Arc<RwLock<Arc<ConfiguredUpstreams>>>,
}

#[async_trait]
impl UpstreamVerifier for DynamicUpstreamVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let verifier = self
            .state
            .read()
            .expect("upstream config manager state poisoned")
            .verifier
            .clone();
        match verifier {
            Some(verifier) => verifier.verify(request).await,
            None => UpstreamVerifiedEvent {
                upstream_name: request.upstream_name,
                model_id: request.model_id,
                url_origin: request.url_origin,
                verifier_id: "none".to_string(),
                result: VerificationResult::Failed,
                required: request.required,
                reason: Some("no upstream verifier configured".to_string()),
                ..Default::default()
            },
        }
    }

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let verifier = self
            .state
            .read()
            .expect("upstream config manager state poisoned")
            .verifier
            .clone();
        match verifier {
            Some(verifier) => verifier.refresh(request).await,
            None => UpstreamVerifiedEvent {
                upstream_name: request.upstream_name,
                model_id: request.model_id,
                url_origin: request.url_origin,
                verifier_id: "none".to_string(),
                result: VerificationResult::Failed,
                required: request.required,
                reason: Some("no upstream verifier configured".to_string()),
                ..Default::default()
            },
        }
    }

    fn invalidate(&self, request: &UpstreamVerificationRequest) {
        let verifier = self
            .state
            .read()
            .expect("upstream config manager state poisoned")
            .verifier
            .clone();
        if let Some(verifier) = verifier {
            verifier.invalidate(request);
        }
    }
}
