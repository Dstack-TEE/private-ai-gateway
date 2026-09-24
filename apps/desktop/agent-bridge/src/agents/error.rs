use std::fmt;

use desktop_core::protocol::{Error, ErrorCode};

/// Only authored, credential-free diagnostics cross the management boundary.
#[derive(Clone, Debug)]
pub enum AgentError {
    ConfigurationRead,
    InvalidConfiguration(String),
    ConfigurationConflict(String),
    AuthenticationConflict(String),
    CredentialStore,
    ConfigurationWrite,
    ConfigurationLock,
    RecordUnavailable,
    RestorationFailed,
    HelperUnavailable,
    MetadataUnavailable(String),
    NoCompatibleModels,
    IncompatibleModel,
    RevisionConflict,
    InvalidState,
    Internal,
}

impl AgentError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::ConfigurationRead => ErrorCode::ConfigurationReadFailed,
            Self::InvalidConfiguration(_) => ErrorCode::InvalidConfiguration,
            Self::ConfigurationConflict(_) => ErrorCode::ConfigurationConflict,
            Self::AuthenticationConflict(_) => ErrorCode::AuthenticationConflict,
            Self::CredentialStore => ErrorCode::CredentialStoreUnavailable,
            Self::ConfigurationWrite => ErrorCode::ConfigurationWriteFailed,
            Self::ConfigurationLock => ErrorCode::ConfigurationLockFailed,
            Self::RecordUnavailable => ErrorCode::ConnectionRecordUnavailable,
            Self::RestorationFailed => ErrorCode::ConfigurationRestoreFailed,
            Self::HelperUnavailable => ErrorCode::HelperUnavailable,
            Self::MetadataUnavailable(_) => ErrorCode::CodexMetadataUnavailable,
            Self::NoCompatibleModels => ErrorCode::NoCompatibleModels,
            Self::IncompatibleModel => ErrorCode::IncompatibleModel,
            Self::RevisionConflict => ErrorCode::RevisionConflict,
            Self::InvalidState => ErrorCode::InvalidState,
            Self::Internal => ErrorCode::OperationFailed,
        }
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ConfigurationRead => "The agent configuration could not be read. Check its file permissions and config location, then retry.",
            Self::InvalidConfiguration(message)
            | Self::ConfigurationConflict(message)
            | Self::AuthenticationConflict(message)
            | Self::MetadataUnavailable(message) => message,
            Self::ConfigurationWrite => "The agent settings could not be saved. Check file permissions and available disk space, then retry.",
            Self::ConfigurationLock => "The agent configuration transaction could not start. Check write permissions on the app data directory and retry.",
            Self::RecordUnavailable => "The agent connection record could not be read or secured. Check the app data directory permissions; do not delete the record because it contains recovery information.",
            Self::CredentialStore => "The agent credential could not be backed up or restored. Check that local-state.json in the app data directory is valid and writable, then retry.",
            Self::RestorationFailed => "Agent configuration restoration is incomplete. Stop protection, check file permissions and local-state.json in the app data directory, then retry disconnecting.",
            Self::HelperUnavailable => super::HELPER_MISSING,
            Self::NoCompatibleModels => "This profile has no models confirmed available on the agent's API. Choose another profile and retry.",
            Self::IncompatibleModel => "The selected model is unavailable on this agent's API for the current profile. Choose a supported model in the agent and retry.",
            Self::RevisionConflict => "The agent settings changed while applying this connection. Try again.",
            Self::InvalidState => "The agent connection is not ready. Check protection and the selected profile, then retry.",
            Self::Internal => "The agent operation could not complete. Check the protection status and agent configuration, then retry.",
        })
    }
}

impl std::error::Error for AgentError {}

impl From<desktop_core::lock::ApplyLockError> for AgentError {
    fn from(_: desktop_core::lock::ApplyLockError) -> Self {
        Self::ConfigurationLock
    }
}

impl From<AgentError> for Error {
    fn from(error: AgentError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_failures_keep_actionable_causes_without_internal_details() {
        for (error, code) in [
            (AgentError::NoCompatibleModels, "no_compatible_models"),
            (AgentError::IncompatibleModel, "incompatible_model"),
            (AgentError::ConfigurationRead, "configuration_read_failed"),
            (AgentError::ConfigurationWrite, "configuration_write_failed"),
            (AgentError::Internal, "operation_failed"),
        ] {
            let public = Error::from(error);
            assert!(public.to_string().starts_with(&format!("{code}: ")));
            assert!(!public.message.contains("PRIVATE_OS_DETAIL"));
            assert!(!public.message.contains("sk-hidden"));
        }
        let diagnostic =
            "The app-owned Codex model catalog is invalid. Reinstall Private AI Proxy.";
        assert_eq!(
            Error::from(AgentError::MetadataUnavailable(diagnostic.to_string())).message,
            diagnostic
        );
    }
}
