// The shell's own Tauri commands, the one list for the invoke handler
// (`invoke_handler!` in `src/lib.rs`) and the Tauri app manifest (`build.rs`
// includes this file). The renderer methods are listed in
// `core/src/ui_methods.rs`.

/// The Tauri command names `renderer_methods!` lists.
#[allow(unused_macros)]
macro_rules! renderer_names {
    (
        commands { $($command:ident => $command_variant:ident),+ $(,)? }
        host { $($host:ident => $host_variant:ident),+ $(,)? }
    ) => {
        [$(stringify!($command),)+ $(stringify!($host),)+]
    };
}

/// The Tauri command names `native_commands!` lists: the last segment of each
/// path.
#[allow(unused_macros)]
macro_rules! native_names {
    ($($($segment:ident)::+),+ $(,)?) => {
        [$([$(stringify!($segment)),+].last().copied().unwrap_or_default()),+]
    };
}

/// Invokes `$callback! { $prefix… path, path, … }`.
macro_rules! native_commands {
    ($callback:ident $($prefix:tt)*) => {
        $callback! {
            $($prefix)*
            commands::settings::select_profile_backup,
            commands::settings::export_profiles,
            commands::settings::export_diagnostics,
            commands::settings::get_cli_registration,
            commands::settings::set_cli_registration,
            notifications::request_notification_permission,
            notifications::open_notification_settings,
            updates::prepare_update,
            updates::set_update_channel,
            updates::restart_to_update,
            commands::accounts::open_top_up,
            commands::accounts::open_organization,
            commands::desktop::copy_text,
            commands::desktop::show_edit_menu,
            commands::desktop::open_agent_website,
            commands::desktop::open_api_key_page,
            commands::desktop::open_about_link,
            commands::desktop::open_web_ui,
            commands::desktop::stop_all_and_quit,
        }
    };
}
