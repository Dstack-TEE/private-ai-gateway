//! Model-routing backend: dispatches each request to a per-model backend.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use super::openai::{request_model_id, rewrite_request_model};
use super::{
    PreparedUpstreamRequest, UpstreamBackend, UpstreamError, UpstreamRequest, UpstreamResponse,
    UpstreamStreamResponse,
};
use crate::aci::receipt::UpstreamVerifiedEvent;

pub struct ModelRoute {
    pub public_model_id: String,
    pub upstream_model_id: String,
    pub upstream: Arc<dyn UpstreamBackend>,
    pub route_id: String,
    /// Per-upstream POST path (e.g. `/v1/messages` for native Anthropic
    /// upstreams). `None` keeps the caller-supplied request path unchanged.
    pub path: Option<String>,
    /// Whether this route's provider is an attested (TEE) provider.
    /// `None` means unclassified (routes built directly via
    /// [`Self::new`], e.g. in tests, defer to request-level
    /// `upstream_required`). See [`PreparedUpstreamRequest::is_tee`].
    pub is_tee: Option<bool>,
}

impl ModelRoute {
    pub fn new(
        public_model_id: impl Into<String>,
        upstream_model_id: impl Into<String>,
        upstream: Arc<dyn UpstreamBackend>,
        route_id: impl Into<String>,
    ) -> Result<Self, UpstreamError> {
        let public_model_id = public_model_id.into();
        let upstream_model_id = upstream_model_id.into();
        let route_id = route_id.into();
        if public_model_id.trim().is_empty() {
            return Err(UpstreamError::Routing(
                "public model id must not be empty".to_string(),
            ));
        }
        if upstream_model_id.trim().is_empty() {
            return Err(UpstreamError::Routing(
                "upstream model id must not be empty".to_string(),
            ));
        }
        if route_id.trim().is_empty() {
            return Err(UpstreamError::Routing(
                "route id must not be empty".to_string(),
            ));
        }
        Ok(Self {
            public_model_id,
            upstream_model_id,
            upstream,
            route_id,
            path: None,
            is_tee: None,
        })
    }

    /// Set the per-upstream POST path. A leading `/` is enforced. `None`
    /// leaves the caller-supplied downstream path untouched.
    pub fn with_path(mut self, path: Option<String>) -> Self {
        self.path = path.map(|mut p| {
            if !p.starts_with('/') {
                p.insert(0, '/');
            }
            p
        });
        self
    }

    /// Classify whether this route's provider is attested (TEE).
    pub fn with_is_tee(mut self, is_tee: Option<bool>) -> Self {
        self.is_tee = is_tee;
        self
    }
}

/// Model-id router for OpenAI-compatible request bodies.
///
/// A route maps one public model id to one concrete upstream and one
/// upstream-accepted model id. The rewrite happens in [`Self::prepare`],
/// before upstream verification and receipt hashing, so the receipt
/// covers the exact bytes sent to the selected upstream.
pub struct ModelRouterBackend {
    name: String,
    routes: HashMap<String, ModelRoute>,
    default_routes: HashMap<String, String>,
    order: Vec<String>,
}

impl ModelRouterBackend {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            routes: HashMap::new(),
            default_routes: HashMap::new(),
            order: Vec::new(),
        }
    }

    pub fn add_route(&mut self, route: ModelRoute) -> Result<(), UpstreamError> {
        if self.routes.contains_key(&route.route_id) {
            return Err(UpstreamError::Routing(format!(
                "duplicate route id {:?}",
                route.route_id
            )));
        }
        if !self.default_routes.contains_key(&route.public_model_id) {
            self.order.push(route.public_model_id.clone());
            self.default_routes
                .insert(route.public_model_id.clone(), route.route_id.clone());
        }
        self.routes.insert(route.route_id.clone(), route);
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    fn route_for(&self, public_model_id: &str) -> Result<&ModelRoute, UpstreamError> {
        let route_id = self.default_routes.get(public_model_id).ok_or_else(|| {
            UpstreamError::Routing(format!("no upstream route for model {public_model_id:?}"))
        })?;
        self.route_for_id(route_id)
    }

    fn route_for_id(&self, route_id: &str) -> Result<&ModelRoute, UpstreamError> {
        self.routes
            .get(route_id)
            .ok_or_else(|| UpstreamError::Routing(format!("unknown target route {route_id:?}")))
    }

    fn route_from_prepared(
        &self,
        req: &PreparedUpstreamRequest,
    ) -> Result<&ModelRoute, UpstreamError> {
        let route_id = req.route_id.as_deref().ok_or_else(|| {
            UpstreamError::Routing("prepared router request is missing route id".to_string())
        })?;
        self.route_for_id(route_id)
    }
}

#[async_trait]
impl UpstreamBackend for ModelRouterBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn url_origin(&self) -> Option<&str> {
        None
    }

    fn prepare(&self, req: UpstreamRequest) -> Result<PreparedUpstreamRequest, UpstreamError> {
        let body_model_id = request_model_id(&req.body).ok_or_else(|| {
            UpstreamError::Routing("request body must contain a string model field".to_string())
        })?;
        let route = match req.target_route_id.as_deref() {
            Some(route_id) => self.route_for_id(route_id)?,
            None => self.route_for(&body_model_id)?,
        };
        let mut request = req;
        request.body = rewrite_request_model(&request.body, &route.upstream_model_id)?;
        // A configured per-upstream path overrides the chat-completions
        // surface only (e.g. native Anthropic `/v1/messages`). Other
        // surfaces (`/v1/completions`, `/v1/embeddings`) keep the
        // caller-supplied path so they route to the matching upstream path.
        if let Some(route_path) = &route.path {
            let on_chat_surface = request
                .path
                .as_deref()
                .map(|path| path == "/v1/chat/completions")
                .unwrap_or(true);
            if on_chat_surface {
                request.path = Some(route_path.clone());
            }
        }
        Ok(PreparedUpstreamRequest {
            request,
            upstream_name: route.upstream.name().to_string(),
            url_origin: route.upstream.url_origin().map(str::to_string),
            model_id: route.upstream_model_id.clone(),
            route_id: Some(route.route_id.clone()),
            is_tee: route.is_tee,
        })
    }

    async fn forward(&self, req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        let prepared = self.prepare(req)?;
        self.forward_prepared(prepared).await
    }

    async fn forward_prepared(
        &self,
        req: PreparedUpstreamRequest,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let route = self.route_from_prepared(&req)?;
        route.upstream.forward(req.request).await
    }

    async fn forward_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let route = self.route_from_prepared(&req)?;
        route.upstream.forward_verified_prepared(req, event).await
    }

    async fn forward_stream(
        &self,
        req: UpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        let prepared = self.prepare(req)?;
        self.forward_stream_prepared(prepared).await
    }

    async fn forward_stream_prepared(
        &self,
        req: PreparedUpstreamRequest,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        let route = self.route_from_prepared(&req)?;
        route.upstream.forward_stream(req.request).await
    }

    async fn forward_stream_verified_prepared(
        &self,
        req: PreparedUpstreamRequest,
        event: &UpstreamVerifiedEvent,
    ) -> Result<UpstreamStreamResponse, UpstreamError> {
        let route = self.route_from_prepared(&req)?;
        route
            .upstream
            .forward_stream_verified_prepared(req, event)
            .await
    }

    async fn models(&self) -> Result<UpstreamResponse, UpstreamError> {
        let data = self
            .order
            .iter()
            .filter_map(|public| self.default_routes.get(public))
            .filter_map(|route_id| self.routes.get(route_id))
            .map(|route| {
                json!({
                    "id": route.public_model_id,
                    "object": "model",
                    "owned_by": self.name.as_str(),
                })
            })
            .collect::<Vec<_>>();
        let body = serde_json::to_vec(&json!({
            "object": "list",
            "data": data,
        }))
        .map_err(|e| UpstreamError::Routing(e.to_string()))?;
        Ok(UpstreamResponse {
            status_code: 200,
            body,
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubBackend {
        name: &'static str,
    }

    #[async_trait]
    impl UpstreamBackend for StubBackend {
        fn name(&self) -> &str {
            self.name
        }

        fn url_origin(&self) -> Option<&str> {
            None
        }

        async fn forward(&self, _req: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
            Err(UpstreamError::Transport("not used".to_string()))
        }
    }

    #[test]
    fn model_router_routes_duplicate_public_models_by_route_id() {
        let mut router = ModelRouterBackend::new("model-router");
        router
            .add_route(
                ModelRoute::new(
                    "openai/gpt-oss-120b",
                    "near-model",
                    Arc::new(StubBackend { name: "near-ai" }),
                    "near-ai:openai/gpt-oss-120b",
                )
                .unwrap(),
            )
            .unwrap();
        router
            .add_route(
                ModelRoute::new(
                    "openai/gpt-oss-120b",
                    "secret-model",
                    Arc::new(StubBackend {
                        name: "secretai-107",
                    }),
                    "secretai-107:openai/gpt-oss-120b",
                )
                .unwrap(),
            )
            .unwrap();

        let body = br#"{"model":"openai/gpt-oss-120b","messages":[]}"#.to_vec();
        let default_prepared = router
            .prepare(UpstreamRequest {
                body: body.clone(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(default_prepared.upstream_name, "near-ai");
        assert_eq!(
            request_model_id(&default_prepared.request.body).as_deref(),
            Some("near-model")
        );

        let targeted_prepared = router
            .prepare(UpstreamRequest {
                body,
                target_route_id: Some("secretai-107:openai/gpt-oss-120b".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(targeted_prepared.upstream_name, "secretai-107");
        assert_eq!(
            request_model_id(&targeted_prepared.request.body).as_deref(),
            Some("secret-model")
        );
    }
}
