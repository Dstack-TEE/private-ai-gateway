//! HTTP wiring for the gateway.

pub mod app;

pub use app::{
    build_internal_backend_router, build_router, build_router_with_admin,
    build_router_with_admin_and_http_middleware, build_router_with_http_middleware,
    GatewayRequestStore, StoredGatewayRequest,
};
