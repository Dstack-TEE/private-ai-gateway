//! HTTP wiring for the gateway.

pub mod app;

pub use crate::aggregator::service::MiddlewareReceiptJournal;
pub use app::{
    bind_unix_listener, build_internal_backend_router, build_router, build_router_with_admin,
    build_router_with_admin_and_uds_middleware, build_router_with_uds_middleware,
    serve_unix_listener, serve_unix_router, GatewayRequestStore, StoredGatewayRequest,
};
