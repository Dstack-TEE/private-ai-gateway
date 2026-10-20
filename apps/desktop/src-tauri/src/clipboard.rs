//! Copies text to the clipboard. A secret, such as the Local API key, is
//! marked so clipboard history leaves it out: on Windows the formats that
//! exclude it from clipboard monitoring, history and the cloud clipboard; on
//! macOS `org.nspasteboard.ConcealedType`, which clipboard managers honor; on
//! Linux, which has no standard, KDE's `x-kde-passwordManagerHint`, the most
//! widely honored convention. The clipboard plugin's arboard cannot mark data,
//! so secrets use one of their own with the same crate.

use std::sync::{Mutex, PoisonError};

use tauri::AppHandle;
use tauri_plugin_clipboard_manager::ClipboardExt;

const UNAVAILABLE: &str = "The clipboard is unavailable";

/// The clipboard for secrets, kept like the plugin's: on X11 the app serves
/// what it copied while it holds one, and `release` drops it on exit.
static SECRET_CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

pub fn write_text(app: &AppHandle, text: String, secret: bool) -> Result<(), String> {
    if !secret {
        return app
            .clipboard()
            .write_text(text)
            .map_err(|_| UNAVAILABLE.into());
    }
    write_secret(text).map_err(|error| {
        tracing::warn!("Cannot copy a secret: {error}");
        UNAVAILABLE.into()
    })
}

/// Drops the clipboard for secrets as the app exits, as the plugin drops its
/// own: on X11 the last arboard clipboard dropped hands what the app copied
/// to the clipboard manager, or, for a secret, gives up its ownership so no
/// manager keeps it.
pub fn release() {
    SECRET_CLIPBOARD
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
}

fn write_secret(text: String) -> Result<(), arboard::Error> {
    let mut clipboard = SECRET_CLIPBOARD
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let clipboard = match &mut *clipboard {
        Some(clipboard) => clipboard,
        none => none.insert(arboard::Clipboard::new()?),
    };
    let set = clipboard.set();
    #[cfg(target_os = "windows")]
    let set = {
        use arboard::SetExtWindows;
        set.exclude_from_monitoring()
            .exclude_from_history()
            .exclude_from_cloud()
    };
    #[cfg(target_os = "macos")]
    let set = {
        use arboard::SetExtApple;
        set.exclude_from_history()
    };
    #[cfg(target_os = "linux")]
    let set = {
        use arboard::SetExtLinux;
        set.exclude_from_history()
    };
    set.text(text)
}
