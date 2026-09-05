//! The macOS menu bar, laid out like Tauri's default menu so every standard
//! item keeps its system role and shortcut: the application menu (About,
//! Settings…, Services, Hide, Hide Others, Show All, Quit), Edit (Undo, Redo,
//! Cut, Copy, Paste, Select All, so text fields in the window get the system
//! editing commands), View (Full Screen), Window (Minimize, Zoom, Close
//! Window), and Help with documentation and source links. Every label comes from
//! the brand module. Other platforms are tray-only and get no menu bar.

use tauri::AppHandle;

/// Emitted to the window when a menu item asks it to show a section.
pub const NAVIGATE_EVENT: &str = "gateway://navigate";

#[cfg(target_os = "macos")]
pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    use desktop_gateway::brand::{ORGANIZATION_NAME, PRODUCT_NAME};
    use tauri::{
        menu::{
            AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu, HELP_SUBMENU_ID,
            WINDOW_SUBMENU_ID,
        },
        Emitter,
    };

    let about = AboutMetadata {
        name: Some(PRODUCT_NAME.to_string()),
        version: Some(app.package_info().version.to_string()),
        authors: Some(vec![ORGANIZATION_NAME.to_string()]),
        ..AboutMetadata::default()
    };
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
        ],
    )?;
    let documentation =
        MenuItem::with_id(app, "documentation", "Documentation", true, None::<&str>)?;
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
    app.on_menu_event(|app, event| match event.id().as_ref() {
        "settings" => {
            crate::tray::show_window(app);
            let _ = app.emit(NAVIGATE_EVENT, "settings");
        }
        "documentation" | "github" => {
            if let Err(error) = crate::open_about_link(app.clone(), event.id().as_ref().to_string())
            {
                use tauri::Manager;
                app.state::<std::sync::Arc<desktop_runtime::controller::DesktopRuntime>>()
                    .report_error(error);
            }
        }
        _ => {}
    });
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn setup(_app: &AppHandle) -> tauri::Result<()> {
    Ok(())
}
