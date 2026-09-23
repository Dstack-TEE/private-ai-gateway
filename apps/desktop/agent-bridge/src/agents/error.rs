use std::fmt;

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
    pub fn code(&self) -> &'static str {
        match self {
            Self::ConfigurationRead => "configuration_read_failed",
            Self::InvalidConfiguration(_) => "invalid_configuration",
            Self::ConfigurationConflict(_) => "configuration_conflict",
            Self::AuthenticationConflict(_) => "authentication_conflict",
            Self::CredentialStore => "credential_store_unavailable",
            Self::ConfigurationWrite => "configuration_write_failed",
            Self::ConfigurationLock => "configuration_lock_failed",
            Self::RecordUnavailable => "connection_record_unavailable",
            Self::RestorationFailed => "configuration_restore_failed",
            Self::HelperUnavailable => "helper_unavailable",
            Self::MetadataUnavailable(_) => "codex_metadata_unavailable",
            Self::NoCompatibleModels => "no_compatible_models",
            Self::IncompatibleModel => "incompatible_model",
            Self::RevisionConflict => "revision_conflict",
            Self::InvalidState => "invalid_state",
            Self::Internal => "operation_failed",
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
            Self::CredentialStore => "The OS credential store could not back up or restore the agent credential. Allow access or unlock it in your user session, then retry.",
            Self::RestorationFailed => "Agent configuration restoration is incomplete. Stop protection, check file permissions and unlock the OS credential store, then retry disconnecting.",
            Self::HelperUnavailable => super::HELPER_MISSING,
            Self::NoCompatibleModels => "This profile has no models confirmed available on the agent's API. Choose another profile and retry.",
            Self::IncompatibleModel => "The selected model is unavailable on this agent's API for the current profile. Choose a supported model in the agent and retry.",
            Self::RevisionConflict => "The agent settings changed while applying this connection. Try again.",
            Self::InvalidState => "The agent connection is not ready. Check protection and the selected profile, then retry.",
            Self::Internal => "The agent operation could not complete. Check the gateway state and agent configuration, then retry.",
        })
    }
}

impl std::error::Error for AgentError {}

impl From<desktop_core::lock::ApplyLockError> for AgentError {
    fn from(_: desktop_core::lock::ApplyLockError) -> Self {
        Self::ConfigurationLock
    }
}

impl From<AgentError> for desktop_core::protocol::RpcError {
    fn from(error: AgentError) -> Self {
        Self::new(error.code(), &error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use desktop_core::protocol::RpcError;

    #[test]
    fn agent_failures_keep_actionable_causes_without_internal_details() {
        for (error, code) in [
            (AgentError::NoCompatibleModels, "no_compatible_models"),
            (AgentError::IncompatibleModel, "incompatible_model"),
            (AgentError::ConfigurationRead, "configuration_read_failed"),
            (AgentError::ConfigurationWrite, "configuration_write_failed"),
            (AgentError::Internal, "operation_failed"),
        ] {
            let public = RpcError::from(error);
            assert_eq!(public.code, code);
            assert!(!public.message.contains("PRIVATE_OS_DETAIL"));
            assert!(!public.message.contains("sk-hidden"));
        }
        let diagnostic =
            "The app-owned Codex model catalog is invalid. Reinstall Private AI Proxy.";
        assert_eq!(
            RpcError::from(AgentError::MetadataUnavailable(diagnostic.to_string())).message,
            diagnostic
        );
    }
}
