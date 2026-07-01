//! HTTP wiring for the gateway.

pub mod app;

pub use app::{build_router, build_router_with_admin, build_router_with_admin_and_middleware};
