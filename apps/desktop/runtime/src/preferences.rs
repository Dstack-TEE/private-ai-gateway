use desktop_gateway::{
    agents::{app_data_dir, write_atomic},
    tokens,
};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    Beta,
    Stable,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    #[serde(default)]
    pub connect_on_launch: bool,
    #[serde(default)]
    pub update_channel: Option<UpdateChannel>,
}

pub fn load() -> Result<Preferences, String> {
    let path = app_data_dir()?.join("preferences.json");
    match tokens::read_private_text(&path)
        .map_err(|error| format!("Cannot read startup preferences: {error}"))?
    {
        Some(text) => {
            serde_json::from_str(&text).map_err(|_| "Startup preferences are invalid".to_string())
        }
        None => Ok(Preferences::default()),
    }
}

pub fn save(preferences: Preferences) -> Result<(), String> {
    let dir = app_data_dir()?;
    tokens::create_private_dir(&dir)
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

#[cfg(test)]
mod tests {
    use super::{Preferences, UpdateChannel};

    #[test]
    fn update_channel_is_optional_and_preserves_startup_preference() {
        let mut preferences: Preferences =
            serde_json::from_str(r#"{"connectOnLaunch":true}"#).unwrap();
        assert_eq!(preferences.update_channel, None);
        preferences.update_channel = Some(UpdateChannel::Beta);
        let restored: Preferences =
            serde_json::from_str(&serde_json::to_string(&preferences).unwrap()).unwrap();
        assert!(restored.connect_on_launch);
        assert_eq!(restored.update_channel, Some(UpdateChannel::Beta));
        assert!(serde_json::from_str::<Preferences>(r#"{"updateChannel":"nightly"}"#).is_err());
    }

    #[test]
    fn startup_connection_is_opt_in_and_requires_a_boolean() {
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
