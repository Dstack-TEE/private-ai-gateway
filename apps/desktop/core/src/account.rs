//! Account links and the login presentation that cross IPC. Authorization
//! itself stays in the backend's `account_login`.

use serde::{Deserialize, Serialize};

use crate::contracts::ServiceProvider;

/// The loopback port of the account connection callback listener.
pub const CALLBACK_PORT: u16 = 4181;

#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LoginPresentation {
    pub id: String,
    pub url: String,
    pub user_code: Option<String>,
}

/// The deep link that brings the desktop app forward after browser sign-in.
pub fn account_return_url() -> String {
    format!("{}://oauth/return", crate::brand::APP_IDENTIFIER)
}

pub fn top_up_url(provider: &ServiceProvider, scope_slug: Option<&str>) -> Result<String, String> {
    let slug = validated_scope_slug(scope_slug)?;
    match provider {
        ServiceProvider::Phala => Ok(format!("https://cloud.phala.com/{slug}/billing")),
        ServiceProvider::Redpill => Ok(format!("https://redpill.ai/{slug}/credits")),
        ServiceProvider::Custom => Err("Billing is only available for Phala and RedPill".into()),
    }
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
    Ok(format!(
        "https://redpill.ai/{}",
        validated_scope_slug(organization_slug)?
    ))
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
