//! HTTP wiring for the gateway.

pub mod app;

pub use app::{
    bind_unix_listener, build_router, build_router_with_admin,
    build_router_with_admin_and_middleware, serve_unix_listener, serve_unix_router,
};
