//! The main window's capability grants exactly the commands the desktop
//! renderer calls: `removeUnusedCommands` strips every command no capability
//! grants, and a grant without a command is a stale permission.

use std::collections::BTreeSet;

/// Renderer methods only the web UI calls: the desktop shell writes exports,
/// opens account pages and installs updates natively.
const WEB_ONLY: &[&str] = &[
    "export_profiles_content",
    "export_diagnostics_content",
    "get_organization_url",
    "get_top_up_url",
    "get_update_notice",
];

#[test]
fn the_main_window_is_granted_exactly_the_desktop_commands() {
    let renderer = desktop_core::renderer_methods!(renderer_names);
    for name in WEB_ONLY {
        assert!(renderer.contains(name), "{name} is not a renderer method");
    }
    let expected: BTreeSet<String> = renderer
        .into_iter()
        .filter(|name| !WEB_ONLY.contains(name))
        .chain(native_commands!(native_names))
        .map(|name| format!("allow-{}", name.replace('_', "-")))
        .collect();
    let capability: serde_json::Value =
        serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
    let granted: BTreeSet<String> = capability["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(serde_json::Value::as_str)
        // Plugin and core permissions are namespaced (`core:event:allow-listen`).
        .filter(|permission| !permission.contains(':'))
        .map(str::to_owned)
        .collect();
    assert_eq!(granted, expected);
}
