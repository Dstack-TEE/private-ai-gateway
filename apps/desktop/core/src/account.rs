//! Account links and the login presentation that cross IPC. Authorization
//! itself stays in the backend's `account_login`.

use serde::{Deserialize, Serialize};

use crate::contracts::ServiceProvider;

/// Where account avatars load from: the `img-src` sources the desktop CSP
/// (`src-tauri/tauri.conf.json`) and the web UI's CSP allow besides `'self'`.
pub const IMAGE_SOURCES: [&str; 3] = [
    "https://img.clerk.com",
    "https://images.clerk.dev",
    "https://clerk.redpill.ai",
];

#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LoginPresentation {
    pub id: String,
    pub url: String,
    pub user_code: Option<String>,
}

/// The deep link that brings the desktop app forward after browser sign-in.
pub const ACCOUNT_RETURN_URL: &str = concat!(crate::app_identifier!(), "://oauth/return");

pub fn account_return_url() -> String {
    ACCOUNT_RETURN_URL.to_string()
}

pub fn top_up_url(provider: &ServiceProvider, scope_slug: Option<&str>) -> Result<String, String> {
    let slug = validated_scope_slug(scope_slug)?;
    let (base, page) = match provider {
        ServiceProvider::Phala => ("https://cloud.phala.com", "billing"),
        ServiceProvider::Redpill => ("https://redpill.ai", "credits"),
        ServiceProvider::Custom => {
            return Err("Billing is only available for Phala and RedPill".into())
        }
    };
    Ok(crate::endpoint(base, &[slug, page])?.into())
}

/// Where a provider's API keys are managed.
pub const fn api_key_page(provider: ServiceProvider) -> Option<&'static str> {
    match provider {
        ServiceProvider::Phala => Some("https://cloud.phala.com/dashboard"),
        ServiceProvider::Redpill => Some("https://www.redpill.ai/dashboard"),
        ServiceProvider::Custom => None,
    }
}

pub fn organization_url(organization_slug: Option<&str>) -> Result<String, String> {
    let slug = validated_scope_slug(organization_slug)?;
    Ok(crate::endpoint("https://redpill.ai", &[slug])?.into())
}

fn validated_scope_slug(slug: Option<&str>) -> Result<&str, String> {
    slug.filter(|slug| {
        !slug.is_empty()
            && slug.len() <= 255
            && slug.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            })
    })
    .ok_or("Refresh account details or reconnect the account to open billing.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_csp_allows_exactly_the_account_image_sources() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../src-tauri/tauri.conf.json"))
                .expect("tauri.conf.json is valid JSON");
        let expected: Vec<&str> = std::iter::once("'self'").chain(IMAGE_SOURCES).collect();
        for key in ["csp", "devCsp"] {
            let policy = config["app"]["security"][key]
                .as_str()
                .expect("a CSP string");
            let sources: Vec<&str> = policy
                .split(';')
                .map(str::trim)
                .find_map(|directive| directive.strip_prefix("img-src "))
                .expect("an img-src directive")
                .split_whitespace()
                .collect();
            assert_eq!(sources, expected, "{key}");
        }
    }
}
