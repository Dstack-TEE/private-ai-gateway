use tauri::{
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, PhysicalPosition, PhysicalSize, Position, Rect, WebviewWindow,
};

use crate::gateway::GatewayManager;

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let menu = MenuBuilder::new(app)
        .text("open", "Open Private AI Gateway")
        .separator()
        .text("quit", "Quit Private AI Gateway")
        .build()?;
    let icon = tauri::image::Image::from_bytes(include_bytes!(
        "../../assets/tray/trayTemplate@2x.png"
    ))?;

    TrayIconBuilder::with_id("gateway")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("Private AI Gateway - Stopped")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_popup(app, None),
            "quit" => {
                let manager = app.state::<GatewayManager>();
                let _ = manager.stop(app);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                toggle_popup(tray.app_handle(), rect);
            }
        })
        .build(app)?;
    Ok(())
}

pub fn show_popup(app: &AppHandle, tray_rect: Option<Rect>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if let Some(rect) = tray_rect {
        position_below_tray(&window, rect);
    } else {
        position_at_top_right(&window);
    }
    let _ = window.show();
    let _ = window.set_focus();
}

fn position_at_top_right(window: &WebviewWindow) {
    let Ok(Some(monitor)) = window.primary_monitor() else {
        return;
    };
    let Ok(window_size) = window.outer_size() else {
        return;
    };
    let monitor_position = monitor.position();
    let monitor_size = monitor.size();
    let x = monitor_position.x + monitor_size.width as i32 - window_size.width as i32 - 12;
    let y = monitor_position.y + 28;
    let _ = window.set_position(Position::Physical(PhysicalPosition::new(x, y)));
}

fn toggle_popup(app: &AppHandle, tray_rect: Rect) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
    } else {
        position_below_tray(&window, tray_rect);
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn position_below_tray(window: &WebviewWindow, rect: Rect) {
    let Ok(scale_factor) = window.scale_factor() else {
        return;
    };
    let tray_position: PhysicalPosition<i32> = rect.position.to_physical(scale_factor);
    let tray_size: PhysicalSize<u32> = rect.size.to_physical(scale_factor);
    let Ok(window_size) = window.outer_size() else {
        return;
    };
    let x = tray_position.x + (tray_size.width as i32 / 2) - (window_size.width as i32 / 2);
    let y = tray_position.y + tray_size.height as i32 + 6;
    let _ = window.set_position(Position::Physical(PhysicalPosition::new(x, y)));
}
