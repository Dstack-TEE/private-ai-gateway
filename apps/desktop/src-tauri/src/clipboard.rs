//! Copies text to the clipboard. A secret, such as the Local API key, is
//! marked so clipboard history leaves it out: on Windows the formats that
//! exclude it from clipboard monitoring, history and the cloud clipboard; on
//! macOS `org.nspasteboard.ConcealedType`, which clipboard managers honor; on
//! Linux, which has no standard, KDE's `x-kde-passwordManagerHint`, the most
//! widely honored convention.

use std::sync::{Mutex, PoisonError};

/// The app's clipboard: on X11 the app serves what it copied while it holds
/// one, and `release` drops it on exit.
static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

pub fn write_text(text: String, secret: bool) -> Result<(), String> {
    write(text, secret).map_err(|error| {
        tracing::warn!("Cannot copy text: {error}");
        "The clipboard is unavailable".into()
    })
}

/// Drops the clipboard as the app exits: on X11 the last arboard clipboard
/// dropped hands what the app copied to the clipboard manager, or, for a
/// secret, gives up its ownership so no manager keeps it.
pub fn release() {
    CLIPBOARD
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
}

fn write(text: String, secret: bool) -> Result<(), arboard::Error> {
    let mut clipboard = CLIPBOARD.lock().unwrap_or_else(PoisonError::into_inner);
    let clipboard = match &mut *clipboard {
        Some(clipboard) => clipboard,
        none => none.insert(arboard::Clipboard::new()?),
    };
    let set = clipboard.set();
    if !secret {
        return set.text(text);
    }
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
