//! Desktop login supervision and the renderer's first-start notice.

use std::{io, path::Path};

use desktop_core::{client::Client, paths, supervisor};
use tauri::State;
use tokio::sync::{oneshot, Mutex};

pub(crate) struct StartupNotice(pub(crate) Mutex<Option<oneshot::Receiver<Option<String>>>>);

pub(crate) fn at_startup(client: &Client) -> Option<String> {
    let overrides = paths::agent_environment::environment_overrides();
    match supervisor::automatic_blockers(&overrides) {
        supervisor::AutomaticBlockers::Eligible => {}
        blockers @ supervisor::AutomaticBlockers::Environment(_) if !supervisor::enabled() => {
            return supervisor::automatic_skip_notice(blockers);
        }
        _ => return None,
    }
    match supervisor::enable().and_then(|enabled| client.handoff_to_supervisor().map(|_| enabled)) {
        Ok(true) => notice_once(
            "login-service-auto-enable-notice-shown",
            "Private AI Proxy now runs in the background so protection survives restarts; turn off in Settings.",
        ),
        Ok(false) => None,
        Err(error) => {
            tracing::warn!("Cannot enable PAP backend supervision: {error}");
            None
        }
    }
}

fn notice_once(marker: &str, message: &str) -> Option<String> {
    let data = paths::app_data_dir().ok()?;
    match record_notice(&data, marker) {
        Ok(first) => first.then(|| message.into()),
        Err(error) => {
            tracing::warn!("Cannot record the backend startup notice: {error}");
            Some(message.into())
        }
    }
}

fn record_notice(data: &Path, marker: &str) -> io::Result<bool> {
    desktop_core::private_fs::create_private_dir(data)?;
    match desktop_core::private_fs::write_private(&data.join(marker), "") {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

#[tauri::command]
pub(crate) async fn take_supervision_notice(
    notice: State<'_, StartupNotice>,
) -> Result<Option<String>, String> {
    let Some(pending) = notice.0.lock().await.take() else {
        return Ok(None);
    };
    Ok(pending.await.ok().flatten())
}

#[cfg(test)]
mod tests {
    #[test]
    fn automatic_enrollment_notice_is_claimed_only_once() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("app-data");
        let marker = "login-service-auto-enable-notice-shown";
        assert!(super::record_notice(&data, marker).unwrap());
        assert!(!super::record_notice(&data, marker).unwrap());
    }
}
