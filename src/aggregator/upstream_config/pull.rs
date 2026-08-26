//! Authenticated pull client for a control API's upstream config endpoint.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{ACCEPT, AUTHORIZATION};
use serde::Deserialize;

use super::{UpstreamConfig, UpstreamConfigManager};

const MAX_PULL_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpstreamPullConfig {
    pub url: String,
    pub token: String,
    #[serde(default = "default_refresh_seconds")]
    pub refresh_seconds: u64,
    #[serde(default = "default_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
}

impl fmt::Debug for UpstreamPullConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UpstreamPullConfig")
            .field("url", &self.url)
            .field("token", &"[REDACTED]")
            .field("refresh_seconds", &self.refresh_seconds)
            .field("request_timeout_seconds", &self.request_timeout_seconds)
            .finish()
    }
}

const fn default_refresh_seconds() -> u64 {
    300
}

const fn default_request_timeout_seconds() -> u64 {
    90
}

impl UpstreamPullConfig {
    pub fn validate(&self) -> Result<(), String> {
        let url = reqwest::Url::parse(self.url.trim())
            .map_err(|err| format!("upstream_pull.url is invalid: {err}"))?;
        if url.scheme() != "https" {
            return Err("upstream_pull.url must use https".to_string());
        }
        if url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "upstream_pull.url must be an HTTPS URL without credentials or a fragment"
                    .to_string(),
            );
        }
        if !(32..=256).contains(&self.token.len()) {
            return Err("upstream_pull.token must contain 32 to 256 bytes".to_string());
        }
        if self.token.contains(['\r', '\n']) {
            return Err("upstream_pull.token must not contain newline characters".to_string());
        }
        if self.refresh_seconds == 0 {
            return Err("upstream_pull.refresh_seconds must be greater than zero".to_string());
        }
        if self.request_timeout_seconds == 0 {
            return Err(
                "upstream_pull.request_timeout_seconds must be greater than zero".to_string(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullRefreshOutcome {
    Updated {
        upstream_count: usize,
        config_digest: String,
    },
    Unchanged {
        upstream_count: usize,
        config_digest: String,
    },
}

// Unknown top-level fields are tolerated so the control API can extend the
// response without breaking older gateways; `schema_version` is the
// compatibility gate.
#[derive(Debug, Deserialize)]
struct PullResponse {
    schema_version: u8,
    upstreams: Vec<UpstreamConfig>,
}

#[derive(Clone)]
pub struct UpstreamConfigPuller {
    manager: Arc<UpstreamConfigManager>,
    config: UpstreamPullConfig,
    client: reqwest::Client,
}

impl UpstreamConfigPuller {
    pub fn new(
        manager: Arc<UpstreamConfigManager>,
        config: UpstreamPullConfig,
    ) -> Result<Self, String> {
        config.validate()?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.request_timeout_seconds))
            // Never forward the machine Bearer token to a redirect target.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| format!("failed to build upstream pull HTTP client: {err}"))?;
        Ok(Self {
            manager,
            config,
            client,
        })
    }

    pub fn refresh_seconds(&self) -> u64 {
        self.config.refresh_seconds
    }

    pub async fn refresh(&self) -> Result<PullRefreshOutcome, String> {
        let response = self
            .client
            .get(&self.config.url)
            .header(ACCEPT, "application/json")
            .header(AUTHORIZATION, format!("Bearer {}", self.config.token))
            .send()
            .await
            .map_err(|err| format!("gateway config pull request failed: {err}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "gateway config pull returned HTTP {}",
                response.status().as_u16()
            ));
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_PULL_RESPONSE_BYTES as u64)
        {
            return Err("gateway config pull response exceeds 4 MiB".to_string());
        }
        let mut body = Vec::with_capacity(
            response
                .content_length()
                .unwrap_or(0)
                .min(MAX_PULL_RESPONSE_BYTES as u64) as usize,
        );
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(|err| format!("failed to read gateway config pull response: {err}"))?;
            if body.len().saturating_add(chunk.len()) > MAX_PULL_RESPONSE_BYTES {
                return Err("gateway config pull response exceeds 4 MiB".to_string());
            }
            body.extend_from_slice(&chunk);
        }
        self.apply_response(&body)
    }

    fn apply_response(&self, bytes: &[u8]) -> Result<PullRefreshOutcome, String> {
        let payload: PullResponse = serde_json::from_slice(bytes)
            .map_err(|err| format!("invalid gateway config pull response: {err}"))?;
        if payload.schema_version != 1 {
            return Err(format!(
                "unsupported gateway config pull schema_version {}",
                payload.schema_version
            ));
        }
        let upstream_count = payload.upstreams.len();
        match self
            .manager
            .replace_if_changed(payload.upstreams)
            .map_err(|err| format!("rejected pulled gateway config: {err}"))?
        {
            Some(snapshot) => Ok(PullRefreshOutcome::Updated {
                upstream_count,
                config_digest: snapshot.config_digest,
            }),
            None => Ok(PullRefreshOutcome::Unchanged {
                upstream_count,
                config_digest: self.manager.snapshot().config_digest,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::aggregator::upstream_config::{UpstreamRuntimeOptions, UpstreamVerifierMode};

    fn manager(path: PathBuf) -> Arc<UpstreamConfigManager> {
        Arc::new(
            UpstreamConfigManager::load(
                path,
                UpstreamRuntimeOptions {
                    verifier_mode: UpstreamVerifierMode::None,
                    accepted_subjects: Vec::new(),
                    accepted_image_digests: Vec::new(),
                    accepted_dstack_kms_root_public_keys: Vec::new(),
                    pccs_url: None,
                    verifier_cache_seconds: 300,
                    connect_timeout_seconds: 10,
                    read_timeout_seconds: 600,
                    verifier_request_timeout_seconds: 60,
                },
            )
            .unwrap(),
        )
    }

    fn config() -> UpstreamPullConfig {
        UpstreamPullConfig {
            url: "https://control.example/api/admin/gateway-upstreams/config".to_string(),
            token: "0123456789abcdef0123456789abcdef".to_string(),
            refresh_seconds: 300,
            request_timeout_seconds: 90,
        }
    }

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "private-ai-gateway-pull-{label}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn pull_config_requires_https_and_a_dedicated_secret() {
        let base = config();
        assert!(base.validate().is_ok());

        for url in [
            "http://control.example/api/admin/gateway-upstreams/config",
            "https://user@control.example/api/admin/gateway-upstreams/config",
            "https://control.example/api/admin/gateway-upstreams/config#secret",
        ] {
            let mut config = base.clone();
            config.url = url.to_string();
            assert!(config.validate().is_err(), "accepted unsafe URL {url}");
        }

        let mut empty = base.clone();
        empty.token.clear();
        assert!(empty.validate().is_err());
        let mut newline = base;
        newline.token = "0123456789abcdef0123456789abcdef\nheader: injected".to_string();
        assert!(newline.validate().is_err());
    }

    #[test]
    fn valid_pull_updates_once_and_an_identical_pull_does_not_rewrite() {
        let path = temp_path("update");
        let puller = UpstreamConfigPuller::new(manager(path.clone()), config()).unwrap();
        let body = br#"{
          "schema_version": 1,
          "generated_at": "2026-08-26T00:00:00Z",
          "upstreams": [{
            "name": "gpu-a",
            "base_url": "https://gpu-a.example",
            "models": {"public-a": "upstream-a"},
            "bearer_token": "provider-secret"
          }]
        }"#;

        let first = puller.apply_response(body).unwrap();
        assert!(matches!(first, PullRefreshOutcome::Updated { .. }));
        let metadata = std::fs::metadata(&path).unwrap();
        let modified = metadata.modified().unwrap();
        let second = puller.apply_response(body).unwrap();
        assert!(matches!(second, PullRefreshOutcome::Unchanged { .. }));
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("provider-secret"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn invalid_pull_never_replaces_the_last_valid_config() {
        let path = temp_path("retain");
        let initial = r#"[{
          "name":"old",
          "base_url":"https://old.example",
          "models":{"public-old":"upstream-old"},
          "bearer_token":"old-secret"
        }]"#;
        std::fs::write(&path, initial).unwrap();
        let manager = manager(path.clone());
        let original_digest = manager.snapshot().config_digest;
        let puller = UpstreamConfigPuller::new(manager.clone(), config()).unwrap();

        for body in [
            br#"{"schema_version":2,"generated_at":"now","upstreams":[]}"#.as_slice(),
            br#"{"schema_version":1,"generated_at":"now","upstreams":[{"name":"broken"}]}"#
                .as_slice(),
            b"not-json".as_slice(),
        ] {
            assert!(puller.apply_response(body).is_err());
            assert_eq!(manager.snapshot().config_digest, original_digest);
            assert!(std::fs::read_to_string(&path)
                .unwrap()
                .contains("old-secret"));
        }
        let _ = std::fs::remove_file(path);
    }
}
