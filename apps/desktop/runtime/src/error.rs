//! Operation errors inside the service. An error authored for the caller
//! carries its API code; any other failure keeps its detail for the state and
//! the service log, and callers receive only `operation_failed` (Docker
//! `errdefs.System`, gRPC `INTERNAL`).

use std::fmt;

use desktop_core::protocol::{self, ErrorCode};

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// Authored for the caller, answered as it is.
    Api(protocol::Error),
    /// Its detail never leaves the service.
    Internal(String),
}

impl Error {
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::Api(protocol::Error::invalid_state(message))
    }

    /// An account operation failed; the message says why, without provider details.
    pub fn account(message: impl Into<String>) -> Self {
        Self::Api(protocol::Error::account(message))
    }

    /// Another operation holds what this one needs.
    pub fn busy() -> Self {
        Self::Api(protocol::Error::busy())
    }

    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Api(error) => error.code,
            Self::Internal(_) => ErrorCode::OperationFailed,
        }
    }
}

/// The message the state and the service log show.
impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api(error) => formatter.write_str(&error.message),
            Self::Internal(detail) => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(detail: String) -> Self {
        Self::Internal(detail)
    }
}

impl From<&str> for Error {
    fn from(detail: &str) -> Self {
        Self::Internal(detail.into())
    }
}

impl From<protocol::Error> for Error {
    fn from(error: protocol::Error) -> Self {
        Self::Api(error)
    }
}

impl From<agent_bridge::agents::AgentError> for Error {
    fn from(error: agent_bridge::agents::AgentError) -> Self {
        Self::Api(error.into())
    }
}

impl From<Error> for protocol::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Api(error) => error,
            Error::Internal(_) => protocol::Error::internal(),
        }
    }
}
