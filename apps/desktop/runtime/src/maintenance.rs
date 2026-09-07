use crate::{
    contracts::{ConfidentialProfile, ConfidentialProfileInput, GatewayState, ServiceProvider},
    service_config::{self, ServiceSettings},
};
use serde::{Deserialize, Serialize};
use std::{fs::File, io::Read, path::Path};

const MAX_BACKUP_BYTES: usize = 256 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProfileConfiguration {
    pub name: String,
    pub provider: ServiceProvider,
    pub remote_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileBackup {
    pub version: u8,
    pub profiles: Vec<ProfileConfiguration>,
}

#[derive(Serialize)]
pub struct ImportResult {
    pub imported: usize,
    pub skipped: usize,
}

impl ProfileBackup {
    pub fn from_profiles(profiles: &[ConfidentialProfile]) -> Self {
        Self {
            version: 1,
            profiles: profiles
                .iter()
                .map(|profile| ProfileConfiguration {
                    name: profile.name.clone(),
                    provider: profile.provider.clone(),
                    remote_url: profile.remote_url.clone(),
                })
                .collect(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.profiles.len() > 50 {
            return Err("Unsupported or oversized profile configuration file".into());
        }
        for profile in &self.profiles {
            resolve(profile)?;
        }
        Ok(())
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        let metadata = std::fs::metadata(path)
            .map_err(|_| "Could not inspect the profile configuration file")?;
        if !metadata.is_file() {
            return Err("Select a regular JSON file".into());
        }
        if metadata.len() > MAX_BACKUP_BYTES as u64 {
            return Err("Profile configuration file is too large".into());
        }
        let file = File::open(path).map_err(|_| "Could not open the profile configuration file")?;
        let mut bytes = Vec::new();
        file.take((MAX_BACKUP_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "Could not read the profile configuration file")?;
        if bytes.len() > MAX_BACKUP_BYTES {
            return Err("Profile configuration file is too large".into());
        }
        let backup: Self = serde_json::from_slice(&bytes).map_err(|_| "Invalid profile configuration file. Credentials and verification state must not be included.")?;
        backup.validate()?;
        Ok(backup)
    }

    pub fn merge(&self, settings: &mut ServiceSettings) -> Result<ImportResult, String> {
        self.validate()?;
        let mut candidate = settings.clone();
        let mut result = ImportResult {
            imported: 0,
            skipped: 0,
        };
        for profile in &self.profiles {
            let mut resolved = resolve(profile)?;
            if candidate.profiles.iter().any(|existing| {
                existing.name == resolved.name
                    && existing.provider == resolved.provider
                    && existing.remote_url == resolved.remote_url
            }) {
                result.skipped += 1;
                continue;
            }
            resolved.id = format!("profile-{}", uuid::Uuid::new_v4());
            resolved.credential_saved = Some(false);
            if candidate.profiles.is_empty() {
                candidate.active_profile_id = resolved.id.clone();
            }
            candidate.upsert(resolved)?;
            result.imported += 1;
        }
        *settings = candidate;
        Ok(result)
    }
}

fn resolve(profile: &ProfileConfiguration) -> Result<ConfidentialProfile, String> {
    service_config::resolve_profile(
        ConfidentialProfileInput {
            id: "import-validation".into(),
            name: profile.name.clone(),
            provider: profile.provider.clone(),
            remote_url: profile.remote_url.clone(),
        },
        None,
    )
}

pub fn write_json(path: &Path, data: &impl Serialize) -> Result<(), String> {
    let text = serde_json::to_string_pretty(data).map_err(|_| "Could not encode the export")?;
    desktop_gateway::agents::write_atomic(path, &text, None)
        .map_err(|_| "Could not save the export file".to_string())
}

/// An allowlist of typed fields; never serialize state, raw errors or stderr.
pub fn diagnostics(state: &GatewayState, version: &str) -> serde_json::Value {
    let status = match state.status.as_str() {
        "verified" | "verifying" | "blocked" | "stopped" | "error" => state.status.as_str(),
        _ => "unknown",
    };
    serde_json::json!({
        "formatVersion": 1, "appVersion": version, "os": std::env::consts::OS, "architecture": std::env::consts::ARCH,
        "gateway": { "status": status, "hasError": state.error.is_some(), "configurationVerification": state.configuration_verification, "productionOsRequired": state.config.require_production_os },
        "localApi": { "bound": state.proxy_url.is_some(), "hasError": state.endpoint_error.is_some(), "networkAccessAllowed": state.local_api.allow_network_access },
        "profiles": { "count": state.profiles.len(), "activeCredentialAvailable": state.api_key_saved },
        "verification": { "identityPresent": state.identity.is_some(), "passedChecks": state.checks.iter().filter(|check| check.status == "pass").count(), "failedChecks": state.checks.iter().filter(|check| check.status == "fail").count() },
        "catalog": { "modelCount": state.catalog.as_ref().map_or(0, |catalog| catalog.models.len()) },
        "usage": { "requestsThisSession": state.session_usage.requests, "failedProofsThisSession": state.session_usage.failed_proof }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn backup() -> ProfileBackup {
        ProfileBackup {
            version: 1,
            profiles: vec![ProfileConfiguration {
                name: "Phala".into(),
                provider: ServiceProvider::Phala,
                remote_url: "https://inference.phala.com".into(),
            }],
        }
    }
    #[test]
    fn imports_are_untrusted_additive_and_idempotent() {
        let mut settings = ServiceSettings::default();
        assert_eq!(backup().merge(&mut settings).unwrap().imported, 1);
        assert_eq!(settings.profiles[0].verified_at, None);
        assert_eq!(settings.profiles[0].credential_saved, Some(false));
        let id = settings.active_profile_id.clone();
        assert_eq!(backup().merge(&mut settings).unwrap().skipped, 1);
        assert_eq!(settings.active_profile_id, id);
        let mut invalid = backup();
        invalid.profiles[0].remote_url = "http://untrusted.example".into();
        assert!(invalid.merge(&mut settings).is_err());
        assert_eq!(settings.profiles.len(), 1);
    }
    #[test]
    fn import_capacity_and_file_limits_fail_without_partial_changes() {
        let mut settings = ServiceSettings::default();
        for index in 0..49 {
            let mut item = backup();
            item.profiles[0].name = format!("Profile {index}");
            item.merge(&mut settings).unwrap();
        }
        let mut incoming = backup();
        incoming.profiles.push(ProfileConfiguration {
            name: "Another profile".into(),
            ..incoming.profiles[0].clone()
        });
        assert!(incoming.merge(&mut settings).is_err());
        assert_eq!(settings.profiles.len(), 49);
        let directory = tempfile::tempdir().unwrap();
        assert!(ProfileBackup::read(directory.path()).is_err());
        let path = directory.path().join("profiles.json");
        std::fs::write(&path, vec![b' '; MAX_BACKUP_BYTES + 1]).unwrap();
        assert!(ProfileBackup::read(&path)
            .unwrap_err()
            .contains("too large"));
        let exported = ProfileBackup::from_profiles(&settings.profiles);
        let text = serde_json::to_string(&exported).unwrap();
        for excluded in ["credential", "verified", "auth", "activeProfileId"] {
            assert!(!text.contains(excluded));
        }
        std::fs::write(&path, text).unwrap();
        assert_eq!(ProfileBackup::read(&path).unwrap().profiles.len(), 49);
    }
    #[test]
    fn diagnostic_allowlist_omits_untrusted_strings() {
        let mut state = GatewayState {
            error: Some("SECRET".into()),
            remote_url: Some("https://SECRET".into()),
            endpoint_error: Some("/home/SECRET".into()),
            ..Default::default()
        };
        state.config.remote_url = "https://SECRET".into();
        assert!(!diagnostics(&state, "test").to_string().contains("SECRET"));
        assert!(serde_json::from_str::<ProfileBackup>(
            r#"{"version":1,"profiles":[],"apiKey":"SECRET"}"#
        )
        .is_err());
    }
}
