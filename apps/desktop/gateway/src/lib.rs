//! The desktop-owned local agent gateway.
//!
//! Coding agents talk to a stable loopback HTTP endpoint, authenticate with
//! machine-local tokens, and are relayed unchanged to the ACI-verified
//! sidecar, where the agent token is swapped for the active Confidential AI
//! profile credential held in the OS credential store. The verified remote
//! catalog is the single source
//! of model truth; agent configs are projected
//! from it and restored field by field.

pub mod agents;
pub mod brand;
pub mod catalog;
pub mod config_doc;
pub mod lock;
pub mod proxy;
pub mod secrets;
pub mod tokens;
