use crate::{
    contracts::ListenConfig,
    paths::app_data_dir,
    private_fs::{self, write_atomic},
};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// Clear of the Local API (4180) and the account callback (4181).
pub const WEB_UI_DEFAULT_PORT: u16 = 4182;

static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    Beta,
    Stable,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    #[serde(default)]
    pub auto_cli_registration: Option<bool>,
    #[serde(default)]
    pub notifications: NotificationPreferences,
    #[serde(default)]
    pub connect_on_launch: bool,
    #[serde(default)]
    pub update_channel: Option<UpdateChannel>,
    #[serde(default)]
    pub appearance: Appearance,
    #[serde(default)]
    pub web_ui: WebUiConfig,
    /// Argon2id hash of the web UI sign-in password. Management reads omit it; see [`Preferences::shared`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_ui_password_hash: Option<String>,
}

impl Preferences {
    /// These preferences as management clients may read them: without the password hash.
    pub fn shared(self) -> Self {
        Self {
            web_ui_password_hash: None,
            ..self
        }
    }
}

/// The service-hosted browser UI. It is off until the user enables it and
/// listens on loopback unless network access is explicitly allowed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
#[ts(optional_fields)]
pub struct WebUiConfig {
    pub enabled: bool,
    pub listen_address: String,
    pub allow_network_access: bool,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_host: Option<String>,
}

impl WebUiConfig {
    pub fn listen(&self) -> ListenConfig {
        ListenConfig {
            listen_address: self.listen_address.clone(),
            allow_network_access: self.allow_network_access,
            port: self.port,
            client_host: self.client_host.clone(),
        }
    }
}

impl Default for WebUiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen_address: "127.0.0.1".into(),
            allow_network_access: false,
            port: WEB_UI_DEFAULT_PORT,
            client_host: None,
        }
    }
}

pub fn load() -> Result<Preferences, String> {
    let path = app_data_dir()?.join("preferences.json");
    match private_fs::read_private_text(&path)
        .map_err(|error| format!("Cannot read startup preferences: {error}"))?
    {
        Some(text) => {
            serde_json::from_str(&text).map_err(|_| "Startup preferences are invalid".to_string())
        }
        None => Ok(Preferences::default()),
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct NotificationPreferences {
    pub enabled: bool,
    pub gateway: bool,
    pub local_api: bool,
    pub verification: bool,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            enabled: true,
            gateway: true,
            local_api: true,
            verification: true,
        }
    }
}

pub fn save(preferences: Preferences) -> Result<(), String> {
    let dir = app_data_dir()?;
    private_fs::create_private_dir(&dir)
        .map_err(|error| format!("Cannot save startup preferences: {error}"))?;
    let text = serde_json::to_string_pretty(&preferences).map_err(|error| error.to_string())?;
    write_atomic(&dir.join("preferences.json"), &text, None)
        .map_err(|error| format!("Cannot save startup preferences: {error}"))
}

pub fn update(change: impl FnOnce(&mut Preferences)) -> Result<(), String> {
    let _guard = WRITE_LOCK
        .lock()
        .map_err(|_| "Preferences are unavailable")?;
    let mut preferences = load()?;
    change(&mut preferences);
    save(preferences)
}

pub fn reset() -> Result<(), String> {
    let _guard = WRITE_LOCK
        .lock()
        .map_err(|_| "Preferences are unavailable")?;
    save(Preferences::default())
}

#[cfg(test)]
mod tests {
    use super::{Appearance, Preferences, UpdateChannel, WebUiConfig};

    #[test]
    fn update_channel_is_optional_and_preserves_startup_preference() {
        let mut preferences: Preferences =
            serde_json::from_str(r#"{"connectOnLaunch":true}"#).unwrap();
        assert_eq!(preferences.update_channel, None);
        assert_eq!(preferences.appearance, Appearance::System);
        assert!(preferences.notifications.enabled);
        preferences.notifications.enabled = false;
        preferences.appearance = Appearance::Dark;
        preferences.update_channel = Some(UpdateChannel::Beta);
        let restored: Preferences =
            serde_json::from_str(&serde_json::to_string(&preferences).unwrap()).unwrap();
        assert!(restored.connect_on_launch);
        assert!(!restored.notifications.enabled);
        assert_eq!(restored.appearance, Appearance::Dark);
        assert!(serde_json::from_str::<Preferences>(r#"{"appearance":"invalid"}"#).is_err());
        assert_eq!(restored.update_channel, Some(UpdateChannel::Beta));
        assert!(serde_json::from_str::<Preferences>(r#"{"updateChannel":"nightly"}"#).is_err());
    }

    #[test]
    fn web_ui_settings_saved_before_network_listening_stay_on_loopback() {
        let preferences: Preferences =
            serde_json::from_str(r#"{"webUi":{"enabled":true,"port":4190}}"#).unwrap();
        assert_eq!(
            preferences.web_ui,
            WebUiConfig {
                enabled: true,
                port: 4190,
                ..WebUiConfig::default()
            }
        );
        assert!(serde_json::from_str::<Preferences>(r#"{"webUi":{"listen":"0.0.0.0"}}"#).is_err());
    }

    #[test]
    fn startup_connection_is_opt_in_and_requires_a_boolean() {
        let mut preferences: Preferences = serde_json::from_str("{}").unwrap();
        assert!(preferences.auto_cli_registration.unwrap_or(true));
        preferences.auto_cli_registration = Some(false);
        let saved = serde_json::to_string(&preferences).unwrap();
        assert_eq!(
            serde_json::from_str::<Preferences>(&saved)
                .unwrap()
                .auto_cli_registration,
            Some(false)
        );
        assert!(!Preferences::default().connect_on_launch);
        assert!(
            !serde_json::from_str::<Preferences>("{}")
                .unwrap()
                .connect_on_launch
        );
        assert!(serde_json::from_str::<Preferences>(r#"{"connectOnLaunch":"true"}"#).is_err());
        let saved = serde_json::to_string(&Preferences {
            connect_on_launch: true,
            ..Preferences::default()
        })
        .unwrap();
        assert!(
            serde_json::from_str::<Preferences>(&saved)
                .unwrap()
                .connect_on_launch
        );
    }
}
