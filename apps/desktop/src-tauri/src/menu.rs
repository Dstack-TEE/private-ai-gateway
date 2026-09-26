//! The macOS menu bar, laid out like Tauri's default menu so every standard
//! item keeps its system role and shortcut: the application menu (About,
//! Settings…, Services, Hide, Hide Others, Show All, Quit), Edit (Undo, Redo,
//! Cut, Copy, Paste, Select All, so text fields in the window get the system
//! editing commands), View (Full Screen), Window (Minimize, Zoom, Close
//! Window, Bring All to Front), and Help, which opens with "<App> Help" (the
//! documentation) and then the source link. Every label comes from the brand
//! module. Other platforms are tray-only and get no menu bar.

use desktop_core::brand::AboutLink;
use tauri::{menu::MenuEvent, AppHandle};

#[cfg(target_os = "macos")]
pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    use desktop_core::brand::{ORGANIZATION_NAME, PRODUCT_NAME};
    use tauri::menu::{
        AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu, HELP_SUBMENU_ID,
        WINDOW_SUBMENU_ID,
    };

    let about = AboutMetadata {
        name: Some(PRODUCT_NAME.to_string()),
        version: Some(app.package_info().version.to_string()),
        authors: Some(vec![ORGANIZATION_NAME.to_string()]),
        ..AboutMetadata::default()
    };
    // The accelerator sends the same navigate request as the renderer's
    // shortcut elsewhere; the window shows the page once a modal dialog closes.
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, Some("CmdOrCtrl+,"))?;
    let application = Submenu::with_items(
        app,
        PRODUCT_NAME,
        true,
        &[
            &PredefinedMenuItem::about(app, Some(&format!("About {PRODUCT_NAME}")), Some(about))?,
            &PredefinedMenuItem::separator(app)?,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, None)?,
        ],
    )?;
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    let view = Submenu::with_items(
        app,
        "View",
        true,
        &[&PredefinedMenuItem::fullscreen(app, None)?],
    )?;
    let window = Submenu::with_id_and_items(
        app,
        WINDOW_SUBMENU_ID,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::bring_all_to_front(app, None)?,
        ],
    )?;
    let documentation = MenuItem::with_id(
        app,
        "documentation",
        format!("{PRODUCT_NAME} Help"),
        true,
        None::<&str>,
    )?;
    let github = MenuItem::with_id(app, "github", "GitHub", true, None::<&str>)?;
    let help = Submenu::with_id_and_items(
        app,
        HELP_SUBMENU_ID,
        "Help",
        true,
        &[&documentation, &github],
    )?;
    app.set_menu(Menu::with_items(
        app,
        &[&application, &edit, &view, &window, &help],
    )?)?;
    Ok(())
}

/// The one handler of every native menu item. Tauri calls each global menu
/// listener for every item, `TrayIconBuilder::on_menu_event` registering one
/// too, so the tray menu and the menu bar share this handler and each item
/// runs once; Settings… has the same id and action in both.
pub fn handle_event(app: &AppHandle, event: MenuEvent) {
    match event.id().as_ref() {
        "documentation" => open_link(app, AboutLink::Documentation),
        "github" => open_link(app, AboutLink::Github),
        id => crate::tray::handle_menu_event(app, id),
    }
}

fn open_link(app: &AppHandle, link: AboutLink) {
    use tauri_plugin_opener::OpenerExt;
    if app.opener().open_url(link.url(), None::<&str>).is_err() {
        crate::notifications::show_failure(
            app,
            "Could not open the link",
            "Your default browser did not open it.",
        );
    }
}

#[cfg(not(target_os = "macos"))]
pub fn setup(_app: &AppHandle) -> tauri::Result<()> {
    Ok(())
}
