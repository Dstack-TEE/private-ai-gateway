//! The coding-agent bridge: the Local API proxy and agent configuration.
//!
//! Coding agents talk to a stable loopback HTTP endpoint, authenticate with
//! machine-local tokens, and are relayed unchanged to the in-process ACI
//! verifier after the agent token is swapped for the active Confidential AI
//! profile credential held in the OS credential store. The verified remote
//! catalog is the single source
//! of model truth; agent configs are projected
//! from it and restored field by field.

pub mod agents;
pub mod catalog;
pub mod config_doc;
pub mod proxy;
pub mod secrets;
pub mod tokens;
