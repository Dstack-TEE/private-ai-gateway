use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

const PROFILES_LABEL: &str = "profiles";
const PRIVACY_LABEL: &str = "privacy";
const LOCAL_API_LABEL: &str = "local-api";
const USAGE_PROOF_LABEL: &str = "usage-proof";
const PROFILE_REPAIR_EVENT: &str = "gateway://profile-repair";
const USAGE_PROOF_EVENT: &str = "gateway://usage-proof";

struct DialogSpec {
    label: &'static str,
    title: &'static str,
    width: f64,
    height: f64,
    min_width: f64,
    min_height: f64,
    query: String,
}

pub fn open(
    app: &AppHandle,
    kind: &str,
    repair: bool,
    record_id: Option<&str>,
) -> Result<(), String> {
    let spec = match kind {
        "profiles" => DialogSpec {
            label: PROFILES_LABEL,
            title: "Profiles",
            width: 620.0,
            height: 560.0,
            min_width: 520.0,
            min_height: 460.0,
            query: if repair {
                "index.html?native-dialog=profiles&repair=1".to_string()
            } else {
                "index.html?native-dialog=profiles".to_string()
            },
        },
        "privacy" => DialogSpec {
            label: PRIVACY_LABEL,
            title: "Privacy Verification",
            width: 700.0,
            height: 680.0,
            min_width: 600.0,
            min_height: 520.0,
            query: "index.html?native-dialog=privacy".to_string(),
        },
        "local-api" => DialogSpec {
            label: LOCAL_API_LABEL,
            title: "Local API Settings",
            width: 600.0,
            height: 680.0,
            min_width: 540.0,
            min_height: 580.0,
            query: "index.html?native-dialog=local-api".to_string(),
        },
        "usage-proof" => {
            let record_id = record_id
                .filter(|value| !value.is_empty() && value.len() <= 128)
                .ok_or_else(|| "A usage record is required".to_string())?;
            if record_id.chars().any(char::is_control) {
                return Err("Invalid usage record".to_string());
            }
            DialogSpec {
                label: USAGE_PROOF_LABEL,
                title: "Usage Proof",
                width: 560.0,
                height: 500.0,
                min_width: 500.0,
                min_height: 420.0,
                query: format!(
                    "index.html?native-dialog=usage-proof&record={}",
                    encode_query_component(record_id)
                ),
            }
        }
        _ => return Err("Unknown native dialog".to_string()),
    };

    if let Some(window) = app.get_webview_window(spec.label) {
        if spec.label == PROFILES_LABEL && repair {
            window
                .emit(PROFILE_REPAIR_EVENT, ())
                .map_err(window_error)?;
        } else if let Some(record_id) = record_id.filter(|_| spec.label == USAGE_PROOF_LABEL) {
            window
                .emit(USAGE_PROOF_EVENT, record_id)
                .map_err(window_error)?;
        }
        window.show().map_err(window_error)?;
        window.set_focus().map_err(window_error)?;
        return Ok(());
    }

    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "The main window is unavailable".to_string())?;
    let mut builder =
        WebviewWindowBuilder::new(app, spec.label, WebviewUrl::App(spec.query.into()))
            .title(spec.title)
            .inner_size(spec.width, spec.height)
            .min_inner_size(spec.min_width, spec.min_height)
            .prevent_overflow()
            .resizable(true)
            .maximizable(false)
            .minimizable(false)
            .skip_taskbar(true);
    builder = match centered_position(&main, spec.width, spec.height) {
        Some((x, y)) => builder.position(x, y),
        None => builder.center(),
    };
    let window = builder
        .parent(&main)
        .map_err(window_error)?
        .build()
        .map_err(window_error)?;
    window.set_focus().map_err(window_error)
}

pub fn open_profiles(app: &AppHandle, repair: bool) -> Result<(), String> {
    open(app, "profiles", repair, None)
}

pub fn close(window: &tauri::WebviewWindow) -> Result<(), String> {
    if ![
        PROFILES_LABEL,
        PRIVACY_LABEL,
        LOCAL_API_LABEL,
        USAGE_PROOF_LABEL,
    ]
    .contains(&window.label())
    {
        return Err("Only native dialog windows can close themselves".to_string());
    }
    window.close().map_err(window_error)
}

fn centered_position(parent: &tauri::WebviewWindow, width: f64, height: f64) -> Option<(f64, f64)> {
    let Ok(scale) = parent.scale_factor() else {
        return None;
    };
    let (Ok(position), Ok(size)) = (parent.outer_position(), parent.outer_size()) else {
        return None;
    };
    Some((
        position.x as f64 / scale + (size.width as f64 / scale - width) / 2.0,
        position.y as f64 / scale + (size.height as f64 / scale - height) / 2.0,
    ))
}

fn encode_query_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

fn window_error(error: impl std::fmt::Display) -> String {
    format!("Cannot manage the native dialog: {error}")
}

#[cfg(test)]
mod tests {
    use super::encode_query_component;

    #[test]
    fn usage_record_ids_are_url_encoded() {
        assert_eq!(
            encode_query_component("tag:legacy/agent@example"),
            "tag%3Alegacy%2Fagent%40example"
        );
    }
}
