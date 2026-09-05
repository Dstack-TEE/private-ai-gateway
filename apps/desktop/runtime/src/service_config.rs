use std::{
    collections::HashSet,
    fs,
    net::IpAddr,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use desktop_gateway::{
    agents::{app_data_dir, write_atomic},
    tokens,
};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::contracts::{
    ConfidentialProfile, ConfidentialProfileInput, ProfileAuth, ServiceProvider, StartGatewayConfig,
};

const CONFIG_FILE: &str = "confidential-ai.json";
const CONFIG_VERSION: u8 = 1;
const LEGACY_DEFAULT_PROFILE_ID: &str = "default";
const MAX_PROFILES: usize = 50;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSettings {
    pub version: u8,
    pub active_profile_id: String,
    pub profiles: Vec<ConfidentialProfile>,
    pub require_production_os: bool,
}

pub struct LoadedSettings {
    pub settings: ServiceSettings,
    pub migrated_legacy: bool,
}

impl Default for ServiceSettings {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            active_profile_id: String::new(),
            profiles: Vec::new(),
            require_production_os: true,
        }
    }
}

impl ServiceSettings {
    pub fn active_profile(&self) -> Result<&ConfidentialProfile, String> {
        self.profiles
            .iter()
            .find(|profile| profile.id == self.active_profile_id)
            .ok_or_else(|| "The active Confidential AI profile does not exist".to_string())
    }

    pub fn runtime_config(&self) -> Result<StartGatewayConfig, String> {
        Ok(StartGatewayConfig {
            remote_url: self
                .profiles
                .iter()
                .find(|profile| profile.id == self.active_profile_id)
                .map(|profile| profile.remote_url.clone())
                .unwrap_or_else(|| desktop_gateway::brand::SERVICE_DEFAULT_URL.to_string()),
            require_production_os: self.require_production_os,
        })
    }

    pub fn upsert(&mut self, profile: ConfidentialProfile) -> Result<(), String> {
        if let Some(existing) = self
            .profiles
            .iter_mut()
            .find(|entry| entry.id == profile.id)
        {
            *existing = profile;
        } else {
            if self.profiles.len() >= MAX_PROFILES {
                return Err(format!(
                    "At most {MAX_PROFILES} Confidential AI profiles are allowed"
                ));
            }
            self.profiles.push(profile);
        }
        Ok(())
    }
}

pub fn profile_has_credential(profile: &ConfidentialProfile) -> bool {
    profile
        .credential_saved
        .unwrap_or(profile.verified_at.is_some())
}

pub fn set_profile_credential_saved(
    settings: &mut ServiceSettings,
    profile_id: &str,
    saved: bool,
) -> Result<bool, String> {
    let profile = settings
        .profiles
        .iter_mut()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| "Confidential AI profile not found".to_string())?;
    if profile.credential_saved == Some(saved) {
        return Ok(false);
    }
    profile.credential_saved = Some(saved);
    Ok(true)
}

pub fn load() -> Result<LoadedSettings, String> {
    let path = config_path()?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoadedSettings {
                settings: ServiceSettings::default(),
                migrated_legacy: false,
            });
        }
        Err(error) => return Err(format!("Cannot read Confidential AI settings: {error}")),
    };
    if let Ok(settings) = serde_json::from_str::<ServiceSettings>(&text) {
        return Ok(LoadedSettings {
            settings: resolve_settings(settings)?,
            migrated_legacy: false,
        });
    }
    let legacy: StartGatewayConfig = serde_json::from_str(&text)
        .map_err(|_| "The saved Confidential AI settings are invalid".to_string())?;
    let legacy = resolve_runtime_config(legacy)?;
    let provider = provider_for_url(&legacy.remote_url);
    let settings = ServiceSettings {
        version: CONFIG_VERSION,
        active_profile_id: LEGACY_DEFAULT_PROFILE_ID.to_string(),
        profiles: vec![ConfidentialProfile {
            id: LEGACY_DEFAULT_PROFILE_ID.to_string(),
            name: provider_name(&provider).to_string(),
            provider,
            remote_url: legacy.remote_url,
            auth: ProfileAuth::ApiKey,
            credential_saved: None,
            verified_at: None,
        }],
        require_production_os: legacy.require_production_os,
    };
    Ok(LoadedSettings {
        settings: resolve_settings(settings)?,
        migrated_legacy: true,
    })
}

pub fn save(settings: ServiceSettings) -> Result<ServiceSettings, String> {
    let settings = resolve_settings(settings)?;
    let path = config_path()?;
    let text = serde_json::to_string_pretty(&settings)
        .map_err(|error| format!("Cannot encode Confidential AI settings: {error}"))?;
    let parent = path
        .parent()
        .ok_or_else(|| "The Confidential AI settings path has no parent".to_string())?;
    tokens::create_private_dir(parent)
        .map_err(|error| format!("Cannot create the app data directory: {error}"))?;
    write_atomic(&path, &text, None)
        .map_err(|error| format!("Cannot save Confidential AI settings: {error}"))?;
    Ok(settings)
}

pub fn resolve_profile(
    input: ConfidentialProfileInput,
    verified_at: Option<u64>,
) -> Result<ConfidentialProfile, String> {
    validate_profile_id(&input.id)?;
    let name = input.name.trim();
    if name.is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
        return Err("Profile name must be between 1 and 80 characters".to_string());
    }
    let remote_url = normalize_url(&input.remote_url)?;
    match input.provider {
        ServiceProvider::Phala if remote_url != "https://inference.phala.com" => {
            return Err("The Phala preset must use https://inference.phala.com".to_string());
        }
        ServiceProvider::Redpill if remote_url != "https://tee.redpill.ai" => {
            return Err("The RedPill preset must use https://tee.redpill.ai".to_string());
        }
        _ => {}
    }
    Ok(ConfidentialProfile {
        id: input.id,
        name: name.to_string(),
        provider: input.provider,
        remote_url,
        auth: ProfileAuth::ApiKey,
        credential_saved: None,
        verified_at,
    })
}

pub fn resolve_runtime_config(
    mut config: StartGatewayConfig,
) -> Result<StartGatewayConfig, String> {
    config.remote_url = normalize_url(&config.remote_url)?;
    Ok(config)
}

pub fn settings_from_state(
    profiles: Vec<ConfidentialProfile>,
    active_profile_id: String,
    require_production_os: bool,
) -> Result<ServiceSettings, String> {
    resolve_settings(ServiceSettings {
        version: CONFIG_VERSION,
        active_profile_id,
        profiles,
        require_production_os,
    })
}

pub fn credential_entry(profile_id: &str) -> Result<String, String> {
    validate_profile_id(profile_id)?;
    Ok(format!("service-profile-{profile_id}-api-key"))
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn resolve_settings(mut settings: ServiceSettings) -> Result<ServiceSettings, String> {
    if settings.version != CONFIG_VERSION {
        return Err("The saved Confidential AI settings use an unsupported version".to_string());
    }
    if settings.profiles.len() > MAX_PROFILES {
        return Err(format!(
            "Confidential AI settings may contain at most {MAX_PROFILES} profiles"
        ));
    }
    let mut ids = HashSet::new();
    for profile in &mut settings.profiles {
        let resolved = resolve_profile(
            ConfidentialProfileInput {
                id: profile.id.clone(),
                name: profile.name.clone(),
                provider: profile.provider.clone(),
                remote_url: profile.remote_url.clone(),
            },
            profile.verified_at,
        )?;
        if !ids.insert(resolved.id.clone()) {
            return Err("Confidential AI profile IDs must be unique".to_string());
        }
        match &profile.auth {
            ProfileAuth::ApiKey => {}
            ProfileAuth::OAuth { account_id, .. } if !account_id.trim().is_empty() => {}
            ProfileAuth::OAuth { .. } => {
                return Err("OAuth profiles must identify an account".to_string());
            }
        }
        profile.id = resolved.id;
        profile.name = resolved.name;
        profile.provider = resolved.provider;
        profile.remote_url = resolved.remote_url;
    }
    if settings.profiles.is_empty() {
        settings.active_profile_id.clear();
    } else if !ids.contains(&settings.active_profile_id) {
        return Err("The active Confidential AI profile does not exist".to_string());
    }
    Ok(settings)
}

fn normalize_url(value: &str) -> Result<String, String> {
    let mut url = Url::parse(value.trim())
        .map_err(|_| "Gateway URL must be a valid HTTP or HTTPS URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(
            "Gateway URL must use HTTPS (HTTP is allowed only for loopback development)"
                .to_string(),
        );
    }
    let loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if url.scheme() != "https" && !loopback {
        return Err(
            "Gateway URL must use HTTPS unless it points to localhost or a loopback address"
                .to_string(),
        );
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Gateway URL must not contain credentials".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("Gateway URL must not contain a query or fragment".to_string());
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(&path);
    Ok(url.to_string().trim_end_matches('/').to_string())
}

fn validate_profile_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            "Profile ID must contain only letters, numbers, hyphens, or underscores".to_string(),
        );
    }
    Ok(())
}

fn provider_for_url(url: &str) -> ServiceProvider {
    match url {
        "https://inference.phala.com" => ServiceProvider::Phala,
        "https://tee.redpill.ai" => ServiceProvider::Redpill,
        _ => ServiceProvider::Custom,
    }
}

fn provider_name(provider: &ServiceProvider) -> &'static str {
    match provider {
        ServiceProvider::Phala => "Phala",
        ServiceProvider::Redpill => "RedPill",
        ServiceProvider::Custom => "Custom service",
    }
}

fn config_path() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join(CONFIG_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(remote_url: &str) -> ConfidentialProfileInput {
        ConfidentialProfileInput {
            id: "work-profile".to_string(),
            name: "Work".to_string(),
            provider: ServiceProvider::Custom,
            remote_url: remote_url.to_string(),
        }
    }

    #[test]
    fn normalizes_remote_url_and_rejects_credentials() {
        assert_eq!(
            resolve_profile(input(" https://private.example.com/ "), None)
                .unwrap()
                .remote_url,
            "https://private.example.com"
        );
        assert!(resolve_profile(input("https://token@private.example.com"), None).is_err());
        assert!(resolve_profile(input("file:///tmp/gateway"), None).is_err());
        assert!(resolve_profile(input("http://private.example.com"), None).is_err());
        assert_eq!(
            resolve_profile(input("http://127.0.0.1:8090/"), None)
                .unwrap()
                .remote_url,
            "http://127.0.0.1:8090"
        );
    }

    #[test]
    fn validates_profiles_and_provider_endpoints() {
        let mut phala = input("https://tee.redpill.ai");
        phala.provider = ServiceProvider::Phala;
        assert!(resolve_profile(phala, None).is_err());
        let mut invalid_id = input("https://private.example.com");
        invalid_id.id = "../../key".to_string();
        assert!(credential_entry(&invalid_id.id).is_err());
        assert!(resolve_profile(invalid_id, None).is_err());
    }

    #[test]
    fn default_settings_start_without_a_profile() {
        let settings = resolve_settings(ServiceSettings::default()).unwrap();
        assert!(settings.profiles.is_empty());
        assert!(settings.active_profile_id.is_empty());
        assert_eq!(
            settings.runtime_config().unwrap().remote_url,
            "https://tee.redpill.ai"
        );
    }

    #[test]
    fn credential_presence_is_explicit_and_profile_scoped() {
        let mut profile = resolve_profile(input("https://private.example.com"), Some(42)).unwrap();
        assert!(profile_has_credential(&profile));
        profile.credential_saved = Some(false);
        let mut settings = ServiceSettings {
            active_profile_id: profile.id.clone(),
            profiles: vec![profile],
            ..ServiceSettings::default()
        };

        assert!(!profile_has_credential(settings.active_profile().unwrap()));
        assert!(set_profile_credential_saved(&mut settings, "work-profile", true).unwrap());
        assert!(profile_has_credential(settings.active_profile().unwrap()));
        assert!(!set_profile_credential_saved(&mut settings, "work-profile", true).unwrap());
        assert!(set_profile_credential_saved(&mut settings, "missing", true).is_err());
    }
}
