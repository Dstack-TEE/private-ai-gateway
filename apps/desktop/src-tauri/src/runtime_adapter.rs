use desktop_runtime::gateway::{SidecarChild, SidecarEvent, SidecarLauncher};
use std::sync::Mutex;
use tauri::AppHandle;
use tauri_plugin_shell::{
    process::{CommandChild, CommandEvent},
    ShellExt,
};
use tokio::sync::mpsc::{self, Receiver};

#[derive(Default)]
pub struct TauriSidecarLauncher(Mutex<Option<AppHandle>>);

impl TauriSidecarLauncher {
    pub fn initialize(&self, app: AppHandle) -> Result<(), String> {
        *self
            .0
            .lock()
            .map_err(|_| "The ACI launcher is unavailable".to_string())? = Some(app);
        Ok(())
    }
}

impl SidecarLauncher for TauriSidecarLauncher {
    fn spawn(
        &self,
        args: Vec<String>,
    ) -> Result<(Receiver<SidecarEvent>, Box<dyn SidecarChild>), String> {
        let app = self
            .0
            .lock()
            .map_err(|_| "The ACI launcher is unavailable".to_string())?
            .clone()
            .ok_or_else(|| "The ACI launcher is not initialized".to_string())?;
        let command = app
            .shell()
            .sidecar("aci")
            .map_err(|error| format!("Cannot locate bundled ACI executable: {error}"))?
            .args(args)
            .set_raw_out(true);
        let (mut source, child) = command
            .spawn()
            .map_err(|error| format!("Cannot start bundled ACI executable: {error}"))?;
        let (events, receiver) = mpsc::channel(256);
        tauri::async_runtime::spawn(async move {
            while let Some(event) = source.recv().await {
                let event = match event {
                    CommandEvent::Stdout(bytes) => SidecarEvent::Stdout(bytes),
                    CommandEvent::Stderr(bytes) => SidecarEvent::Stderr(bytes),
                    CommandEvent::Error(error) => SidecarEvent::Error(error),
                    CommandEvent::Terminated(_) => SidecarEvent::Terminated,
                    _ => continue,
                };
                if events.send(event).await.is_err() {
                    break;
                }
            }
        });
        Ok((receiver, Box::new(TauriSidecarChild(Some(child)))))
    }
}

struct TauriSidecarChild(Option<CommandChild>);

impl SidecarChild for TauriSidecarChild {
    fn kill(&mut self) -> Result<(), String> {
        let Some(child) = self.0.take() else {
            return Ok(());
        };
        child
            .kill()
            .map_err(|error| format!("Cannot stop ACI executable: {error}"))
    }
}
