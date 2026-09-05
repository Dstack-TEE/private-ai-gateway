use desktop_gateway::{
    agents::{app_data_dir, write_atomic},
    tokens,
};
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    #[serde(default)]
    pub connect_on_launch: bool,
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

#[cfg(test)]
mod tests {
    use super::Preferences;

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
        })
        .unwrap();
        assert!(
            serde_json::from_str::<Preferences>(&saved)
                .unwrap()
                .connect_on_launch
        );
    }
}
