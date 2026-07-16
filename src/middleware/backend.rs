use async_trait::async_trait;
use axum::response::Response;

use crate::aggregator::service::AciService;

use super::completion::{CompletionInput, InternalForwardRequest};

#[async_trait]
pub trait MiddlewareBackend: Send + Sync {
    fn name(&self) -> &'static str;

    async fn handle_catalog(&self, v1_path: &str) -> Response;

    async fn handle_completion(&self, service: &AciService, input: CompletionInput) -> Response;

    fn internal_token(&self) -> Option<&str> {
        None
    }

    async fn handle_internal_forward(
        &self,
        _service: &AciService,
        _input: InternalForwardRequest,
    ) -> Response {
        super::errors::error_response(
            super::errors::Surface::Openai,
            404,
            "not_found",
            "/internal/forward is not enabled for this middleware backend",
            None,
        )
    }
}
