//! Product identity; see "Branding" in apps/desktop/README.md.

use serde::{Deserialize, Serialize};

/// `productName` in src-tauri/tauri.conf.json.
pub const PRODUCT_NAME: &str = "Private AI Proxy";
/// `bundle.publisher` in src-tauri/tauri.conf.json.
pub const ORGANIZATION_NAME: &str = "Dstack TEE";
/// `byline` in src/renderer/brand/brand.ts.
pub const BYLINE: &str = "by dstack TEE";
pub const SERVICE_NAME: &str = "RedPill";
/// `service.defaultUrl` in src/renderer/brand/brand.ts.
pub const SERVICE_DEFAULT_URL: &str = "https://tee.redpill.ai";
/// `identifier` in src-tauri/tauri.conf.json. It names the per-user app data
/// directory and credential namespace on every platform.
pub const APP_IDENTIFIER: &str = "org.dstack.private-ai-proxy";

/// A project resource Settings, the Help menu and the tray link to. The
/// renderer receives the URLs as generated constants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum AboutLink {
    Documentation,
    Github,
    Aci,
}

impl AboutLink {
    pub const ALL: [Self; 3] = [Self::Documentation, Self::Github, Self::Aci];

    pub const fn url(self) -> &'static str {
        match self {
            Self::Documentation => {
                "https://github.com/Dstack-TEE/private-ai-gateway/blob/main/docs/quickstart.md"
            }
            Self::Github => "https://github.com/Dstack-TEE/private-ai-gateway",
            Self::Aci => "https://github.com/Dstack-TEE/private-ai-gateway/blob/main/docs/attested-confidential-inference.md",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tauri.conf.json is the source of the identity it shares with Rust.
    #[test]
    fn identity_matches_the_tauri_config() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../src-tauri/tauri.conf.json"))
                .expect("tauri.conf.json is valid JSON");
        assert_eq!(config["productName"], PRODUCT_NAME);
        assert_eq!(config["app"]["windows"][0]["title"], PRODUCT_NAME);
        assert_eq!(config["identifier"], APP_IDENTIFIER);
        assert_eq!(
            config["plugins"]["deep-link"]["desktop"]["schemes"],
            serde_json::json!([APP_IDENTIFIER])
        );
        assert_eq!(config["bundle"]["publisher"], ORGANIZATION_NAME);
    }

    /// The renderer repeats the byline and service default as literals.
    #[test]
    fn renderer_brand_matches() {
        let renderer = include_str!("../../src/renderer/brand/brand.ts");
        let quoted = |value: &str| serde_json::to_string(value).expect("string");
        assert!(renderer.contains(&format!("byline: {},", quoted(BYLINE))));
        assert!(renderer.contains(&format!("defaultUrl: {} ", quoted(SERVICE_DEFAULT_URL))));
    }
}
