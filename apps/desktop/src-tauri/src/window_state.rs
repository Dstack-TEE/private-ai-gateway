use std::collections::HashMap;

use serde::Deserialize;
use tauri::{Manager, WebviewWindow};

const LEGACY_DEFAULT_WIDTH: u32 = 1052;
const LEGACY_DEFAULT_HEIGHTS: [u32; 2] = [720, 840];
const DISPLAY_SCALE_FACTORS: [f64; 8] = [1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0];

#[derive(Deserialize)]
struct SavedWindowState {
    width: u32,
    height: u32,
    maximized: bool,
}

pub fn migrate_legacy_default(
    app: &tauri::App,
    window: &WebviewWindow,
    width: f64,
    height: f64,
) -> tauri::Result<()> {
    if has_legacy_default(app) {
        window.set_size(tauri::LogicalSize::new(width, height))?;
    }
    Ok(())
}

fn has_legacy_default(app: &tauri::App) -> bool {
    let Ok(directory) = app.path().app_config_dir() else {
        return false;
    };
    let Ok(bytes) = std::fs::read(directory.join(tauri_plugin_window_state::DEFAULT_FILENAME))
    else {
        return false;
    };
    let Ok(state) = serde_json::from_slice::<HashMap<String, SavedWindowState>>(&bytes) else {
        return false;
    };
    let Some(main) = state.get("main") else {
        return false;
    };
    saved_size_is_legacy_default(main.width, main.height, main.maximized)
}

fn saved_size_is_legacy_default(width: u32, height: u32, maximized: bool) -> bool {
    if maximized {
        return false;
    }
    DISPLAY_SCALE_FACTORS.iter().any(|scale| {
        width == (f64::from(LEGACY_DEFAULT_WIDTH) * scale).round() as u32
            && LEGACY_DEFAULT_HEIGHTS
                .iter()
                .any(|legacy_height| height == (f64::from(*legacy_height) * scale).round() as u32)
    })
}

#[cfg(test)]
mod tests {
    use super::saved_size_is_legacy_default;

    #[test]
    fn recognizes_only_the_previous_default_window_size() {
        assert!(saved_size_is_legacy_default(1052, 840, false));
        assert!(saved_size_is_legacy_default(2104, 1680, false));
        assert!(!saved_size_is_legacy_default(1368, 1092, false));
        assert!(saved_size_is_legacy_default(1052, 720, false));
        assert!(!saved_size_is_legacy_default(1052, 840, true));
    }
}
