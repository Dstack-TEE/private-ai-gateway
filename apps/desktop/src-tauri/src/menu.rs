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

/// The menu bar, which `tauri::Builder::menu` sets on macOS.
#[cfg(any(target_os = "macos", test))]
pub fn menu_bar<R: tauri::Runtime>(app: &AppHandle<R>) -> tauri::Result<tauri::menu::Menu<R>> {
    use desktop_core::brand::{ORGANIZATION_NAME, PRODUCT_NAME};
    use tauri::menu::{
        AboutMetadata, MenuBuilder, MenuItem, SubmenuBuilder, HELP_SUBMENU_ID, WINDOW_SUBMENU_ID,
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
    let application = SubmenuBuilder::new(app, PRODUCT_NAME)
        .about_with_text(format!("About {PRODUCT_NAME}"), Some(about))
        .separator()
        .item(&settings)
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    let view = SubmenuBuilder::new(app, "View").fullscreen().build()?;
    let window = SubmenuBuilder::with_id(app, WINDOW_SUBMENU_ID, "Window")
        .minimize()
        .maximize()
        .separator()
        .close_window()
        .separator()
        .bring_all_to_front()
        .build()?;
    let help = SubmenuBuilder::with_id(app, HELP_SUBMENU_ID, "Help")
        .text("documentation", format!("{PRODUCT_NAME} Help"))
        .text("github", "GitHub")
        .build()?;
    MenuBuilder::new(app)
        .items(&[&application, &edit, &view, &window, &help])
        .build()
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

/// Each item of a menu as `id: text`, check items with their state, disabled
/// items marked, and predefined items by their label; submenu items follow
/// their submenu, indented.
#[cfg(test)]
pub(crate) fn describe<R: tauri::Runtime>(items: Vec<tauri::menu::MenuItemKind<R>>) -> Vec<String> {
    use tauri::menu::MenuItemKind;
    let disabled = |enabled: tauri::Result<bool>| if enabled.unwrap() { "" } else { " (disabled)" };
    let mut lines = Vec::new();
    for item in items {
        match item {
            MenuItemKind::MenuItem(item) => lines.push(format!(
                "{}: {}{}",
                item.id().0,
                item.text().unwrap(),
                disabled(item.is_enabled())
            )),
            MenuItemKind::Check(item) => lines.push(format!(
                "{}: [{}] {}{}",
                item.id().0,
                if item.is_checked().unwrap() { "x" } else { " " },
                item.text().unwrap(),
                disabled(item.is_enabled())
            )),
            MenuItemKind::Predefined(item) => {
                let text = item.text().unwrap().replace('&', "");
                lines.push(if text.is_empty() { "---".into() } else { text });
            }
            MenuItemKind::Submenu(submenu) => {
                lines.push(format!(
                    "{}:{}",
                    submenu.text().unwrap(),
                    disabled(submenu.is_enabled())
                ));
                let items = describe(submenu.items().unwrap());
                lines.extend(items.into_iter().map(|line| format!("  {line}")));
            }
            MenuItemKind::Icon(item) => {
                lines.push(format!("{}: {}", item.id().0, item.text().unwrap()))
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use tauri::menu::{HELP_SUBMENU_ID, WINDOW_SUBMENU_ID};

    #[test]
    fn the_menu_bar_keeps_the_standard_layout() {
        let app = tauri::test::mock_app();
        let menu = super::menu_bar(app.handle()).unwrap();
        let (maximize, close, quit) = if cfg!(target_os = "macos") {
            ("Zoom", "Close Window", "Quit")
        } else if cfg!(windows) {
            ("Maximize", "Close", "Exit")
        } else {
            ("Maximize", "Close Window", "Quit")
        };
        assert_eq!(
            super::describe(menu.items().unwrap()),
            [
                "Private AI Proxy:",
                "  About Private AI Proxy",
                "  ---",
                "  settings: Settings…",
                "  ---",
                "  Services",
                "  ---",
                "  Hide",
                "  Hide Others",
                "  Show All",
                "  ---",
                &format!("  {quit}"),
                "Edit:",
                "  Undo",
                "  Redo",
                "  ---",
                "  Cut",
                "  Copy",
                "  Paste",
                "  Select All",
                "View:",
                "  Toggle Full Screen",
                "Window:",
                "  Minimize",
                &format!("  {maximize}"),
                "  ---",
                &format!("  {close}"),
                "  ---",
                "  Bring All to Front",
                "Help:",
                "  documentation: Private AI Proxy Help",
                "  github: GitHub",
            ]
        );
        let ids: Vec<_> = menu
            .items()
            .unwrap()
            .iter()
            .map(|item| item.id().0.clone())
            .collect();
        assert_eq!(ids[3], WINDOW_SUBMENU_ID);
        assert_eq!(ids[4], HELP_SUBMENU_ID);
    }
}
