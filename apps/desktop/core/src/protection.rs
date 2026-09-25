//! How protection presents itself. The window, the web UI and the tray all
//! show the one presentation [`AppState::protection`] carries.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::contracts::AppState;

/// The verifier's state. `Verifying` lasts until the service identity and
/// the catalog are both in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum VerificationStatus {
    #[default]
    Stopped,
    Verifying,
    Verified,
    Blocked,
    Error,
}

/// The serialized name, as logs show it.
impl std::fmt::Display for VerificationStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.serialize(formatter)
    }
}

/// What protection is doing, as the user sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ProtectionPhase {
    /// The desktop shell is still starting the backend; profiles and
    /// protection are unknown until it answers.
    Starting,
    Reconnecting,
    LocalApiUnavailable,
    VerifyingConfiguration,
    Verifying,
    Blocked,
    Interrupted,
    ProfileRequired,
    #[default]
    NotProtected,
    ConfigurationVerified,
    ApiKeyRequired,
    Protected,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum Tone {
    Success,
    Warning,
    Danger,
    #[default]
    Neutral,
}

/// The protection switch in the window and the tray's protection item.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProtectionAction {
    pub label: String,
    pub enabled: bool,
    /// Whether the action stops protection (the switch is on) rather than
    /// starting it.
    pub stops: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Protection {
    pub phase: ProtectionPhase,
    pub title: String,
    pub tone: Tone,
    pub action: ProtectionAction,
}

impl Protection {
    pub fn of(state: &AppState) -> Self {
        let phase = phase(state);
        let (title, tone) = match phase {
            ProtectionPhase::Starting => ("Starting…", Tone::Neutral),
            ProtectionPhase::Reconnecting => ("Reconnecting…", Tone::Neutral),
            ProtectionPhase::LocalApiUnavailable => ("Local API unavailable", Tone::Danger),
            ProtectionPhase::VerifyingConfiguration => ("Verifying configuration…", Tone::Neutral),
            ProtectionPhase::Verifying => ("Verifying…", Tone::Neutral),
            ProtectionPhase::Blocked => ("Protection blocked", Tone::Danger),
            ProtectionPhase::Interrupted => ("Protection interrupted", Tone::Danger),
            ProtectionPhase::ProfileRequired | ProtectionPhase::NotProtected => {
                ("Not protected", Tone::Neutral)
            }
            ProtectionPhase::ConfigurationVerified => ("Configuration verified", Tone::Neutral),
            ProtectionPhase::ApiKeyRequired => ("API key needed", Tone::Warning),
            ProtectionPhase::Protected if !state.config.require_production_os => {
                ("Protected (Dev mode)", Tone::Warning)
            }
            ProtectionPhase::Protected => ("Protected", Tone::Success),
        };
        let stops = state.should_stop_protection();
        let label = match phase {
            ProtectionPhase::Reconnecting if stops => "Cancel Reconnection",
            ProtectionPhase::Verifying => "Cancel Verification",
            _ if stops => "Stop Protection",
            ProtectionPhase::ProfileRequired => "Set Up Profile…",
            _ => "Start Protection",
        };
        let enabled = phase != ProtectionPhase::Starting
            && !(state.status == VerificationStatus::Verifying && state.configuration_verification)
            && (stops || state.endpoint_error.is_none());
        Self {
            phase,
            title: title.into(),
            tone,
            action: ProtectionAction {
                label: label.into(),
                enabled,
                stops,
            },
        }
    }
}

fn phase(state: &AppState) -> ProtectionPhase {
    if state.backend_connected == Some(false) && state.error.is_none() {
        return ProtectionPhase::Starting;
    }
    if state.reconnecting {
        return ProtectionPhase::Reconnecting;
    }
    if state.endpoint_error.is_some() {
        return ProtectionPhase::LocalApiUnavailable;
    }
    match state.status {
        VerificationStatus::Verifying if state.configuration_verification => {
            ProtectionPhase::VerifyingConfiguration
        }
        VerificationStatus::Verifying => ProtectionPhase::Verifying,
        VerificationStatus::Blocked => ProtectionPhase::Blocked,
        VerificationStatus::Error => ProtectionPhase::Interrupted,
        VerificationStatus::Verified if state.configuration_verification => {
            ProtectionPhase::ConfigurationVerified
        }
        VerificationStatus::Verified if !state.api_key_saved => ProtectionPhase::ApiKeyRequired,
        VerificationStatus::Verified => ProtectionPhase::Protected,
        VerificationStatus::Stopped if active_profile_ready(state) => ProtectionPhase::NotProtected,
        VerificationStatus::Stopped => ProtectionPhase::ProfileRequired,
    }
}

/// The active profile has a saved credential, so protection can start.
fn active_profile_ready(state: &AppState) -> bool {
    state.api_key_saved
        && state
            .profiles
            .iter()
            .any(|profile| profile.id == state.active_profile_id && profile.credential_saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{ConfidentialProfile, ProfileAuth, ServiceProvider};

    fn state(status: VerificationStatus, api_key_saved: bool) -> AppState {
        AppState {
            status,
            api_key_saved,
            ..AppState::default()
        }
    }

    fn action(state: &AppState) -> ProtectionAction {
        Protection::of(state).action
    }

    #[test]
    fn the_action_matches_the_runtime_operation() {
        assert_eq!(
            Protection::of(&AppState::default()).phase,
            ProtectionPhase::ProfileRequired
        );
        assert_eq!(action(&AppState::default()).label, "Set Up Profile…");
        let protected = Protection::of(&state(VerificationStatus::Verified, true));
        assert_eq!(protected.title, "Protected");
        assert_eq!(protected.action.label, "Stop Protection");
        assert_eq!(
            Protection::of(&state(VerificationStatus::Verified, false)).phase,
            ProtectionPhase::ApiKeyRequired
        );
        assert_eq!(
            action(&state(VerificationStatus::Verifying, false)).label,
            "Cancel Verification"
        );
        assert!(action(&state(VerificationStatus::Blocked, true)).stops);
        assert!(!action(&state(VerificationStatus::Error, true)).stops);
        for status in [VerificationStatus::Stopped, VerificationStatus::Error] {
            let mut reconnecting = state(status, true);
            reconnecting.reconnecting = true;
            assert!(action(&reconnecting).stops);
            assert_eq!(action(&reconnecting).label, "Cancel Reconnection");
            reconnecting.configuration_verification = true;
            assert!(!action(&reconnecting).stops);
        }
    }

    #[test]
    fn a_starting_backend_offers_no_protection_action() {
        let starting = AppState {
            backend_connected: Some(false),
            ..AppState::default()
        };
        assert_eq!(Protection::of(&starting).phase, ProtectionPhase::Starting);
        assert!(!action(&starting).enabled);
        let failed = AppState {
            error: Some("Backend exited during startup".into()),
            ..starting
        };
        assert_ne!(Protection::of(&failed).phase, ProtectionPhase::Starting);
    }

    #[test]
    fn a_stopped_state_requires_a_saved_active_credential() {
        let mut ready = state(VerificationStatus::Stopped, true);
        ready.active_profile_id = "profile-1".to_string();
        ready.profiles.push(ConfidentialProfile {
            credential_ref: None,
            id: "profile-1".to_string(),
            name: "Private AI".to_string(),
            provider: ServiceProvider::Custom,
            remote_url: "https://private.example.com".to_string(),
            auth: ProfileAuth::ApiKey,
            credential_saved: true,
            verified_at: None,
        });
        assert_eq!(Protection::of(&ready).phase, ProtectionPhase::NotProtected);
        assert_eq!(action(&ready).label, "Start Protection");

        ready.status = VerificationStatus::Verified;
        ready.configuration_verification = true;
        assert_eq!(
            Protection::of(&ready).phase,
            ProtectionPhase::ConfigurationVerified
        );
        assert_eq!(action(&ready).label, "Start Protection");
        assert!(action(&ready).enabled);

        ready.status = VerificationStatus::Verifying;
        assert!(!action(&ready).stops);
        assert!(!action(&ready).enabled);

        ready.status = VerificationStatus::Stopped;
        ready.configuration_verification = false;
        ready.profiles[0].credential_saved = false;
        assert_eq!(
            Protection::of(&ready).phase,
            ProtectionPhase::ProfileRequired
        );
        assert_eq!(action(&ready).label, "Set Up Profile…");
    }

    #[test]
    fn development_os_protection_says_so() {
        let mut state = state(VerificationStatus::Verified, true);
        state.config.require_production_os = false;
        let protection = Protection::of(&state);
        assert_eq!(protection.title, "Protected (Dev mode)");
        assert_eq!(protection.tone, Tone::Warning);
    }
}
