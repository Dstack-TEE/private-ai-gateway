//! The desktop-owned local agent gateway.
//!
//! Coding agents talk to a stable loopback endpoint served over TLS with a
//! per-installation identity, authenticate with machine-local tokens, and are
//! forwarded only to the ACI-verified sidecar, where the agent token is swapped
//! for the RedPill API key held in the OS credential store. The verified remote
//! catalog is the single source of model truth; agent configs are projected
//! from it and restored field by field.

pub mod agents;
pub mod catalog;
pub mod config_doc;
pub mod lock;
pub mod proxy;
pub mod secrets;
pub mod tls;
pub mod tokens;
