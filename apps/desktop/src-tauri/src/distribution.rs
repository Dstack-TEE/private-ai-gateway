use desktop_core::contracts::{DistributionCapabilities, DistributionChannel};

#[cfg(feature = "mac-app-store")]
pub(crate) const CAPABILITIES: DistributionCapabilities = DistributionCapabilities {
    channel: DistributionChannel::MacAppStore,
    native_updates: false,
    cli_registration: false,
    account_portal_links: false,
    sandbox_home_access: cfg!(target_os = "macos"),
    launch_at_login: true,
    notifications: true,
    web_ui: false,
};

#[cfg(not(feature = "mac-app-store"))]
pub(crate) const CAPABILITIES: DistributionCapabilities = DistributionCapabilities {
    channel: DistributionChannel::Direct,
    native_updates: true,
    cli_registration: true,
    account_portal_links: true,
    sandbox_home_access: false,
    launch_at_login: true,
    notifications: true,
    web_ui: true,
};

pub(crate) fn initialization_script() -> String {
    let capabilities = serde_json::to_string(&CAPABILITIES)
        .expect("distribution capabilities must be serializable");
    format!("window.__PAP_DISTRIBUTION__ = {capabilities};")
}

pub(crate) fn require(available: bool, message: &str) -> Result<(), String> {
    available.then_some(()).ok_or_else(|| message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_capabilities_enforce_the_channel_contract() {
        let script = initialization_script();
        let json = script
            .strip_prefix("window.__PAP_DISTRIBUTION__ = ")
            .unwrap()
            .strip_suffix(';')
            .unwrap();
        let actual: serde_json::Value = serde_json::from_str(json).unwrap();
        let app_store = cfg!(feature = "mac-app-store");
        assert_eq!(
            actual,
            serde_json::json!({
                "channel": if app_store { "macAppStore" } else { "direct" },
                "nativeUpdates": !app_store,
                "cliRegistration": !app_store,
                "accountPortalLinks": !app_store,
                "sandboxHomeAccess": app_store && cfg!(target_os = "macos"),
                "launchAtLogin": true,
                "notifications": true,
                "webUi": !app_store,
            })
        );
        assert_eq!(
            require(CAPABILITIES.native_updates, "App Store").is_err(),
            app_store
        );
        assert_eq!(
            require(CAPABILITIES.account_portal_links, "App Store").is_err(),
            app_store
        );
    }
}
