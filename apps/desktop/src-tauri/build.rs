// The renderer methods, as `desktop_core::renderer_methods!` lists them.
include!("../core/src/ui_methods.rs");

macro_rules! names {
    (
        commands { $($command:ident => $command_variant:ident),+ $(,)? }
        host { $($host:ident => $host_variant:ident),+ $(,)? }
    ) => {
        [$(stringify!($command),)+ $(stringify!($host),)+]
    };
}

/// The shell's own commands; see `invoke_handler!` in `src/lib.rs`.
const NATIVE_COMMANDS: &[&str] = &[
    "read_profile_backup",
    "export_profiles",
    "export_diagnostics",
    "request_notification_permission",
    "open_notification_settings",
    "prepare_update",
    "set_update_channel",
    "restart_to_update",
    "open_top_up",
    "open_organization",
    "copy_text",
    "show_edit_menu",
    "main_window_ready",
    "open_agent_website",
    "open_api_key_page",
    "open_about_link",
    "open_web_ui",
    "get_cli_registration",
    "set_cli_registration",
    "stop_all_and_quit",
];

fn main() {
    let commands: Vec<&'static str> = renderer_methods!(names)
        .into_iter()
        .chain(NATIVE_COMMANDS.iter().copied())
        .collect();
    let manifest = tauri_build::AppManifest::new().commands(commands.leak());
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("failed to build the Tauri application");
}
