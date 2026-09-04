use std::{path::PathBuf, process::Stdio};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::{mpsc, oneshot},
};

use crate::gateway::{SidecarChild, SidecarEvent, SidecarLauncher};

pub struct TokioSidecarLauncher {
    executable: PathBuf,
}

impl TokioSidecarLauncher {
    pub fn new(executable: PathBuf) -> Result<Self, String> {
        if !executable.is_absolute() {
            return Err("The ACI executable path must be absolute".to_string());
        }
        Ok(Self { executable })
    }
}

impl SidecarLauncher for TokioSidecarLauncher {
    fn spawn(
        &self,
        args: Vec<String>,
    ) -> Result<(mpsc::Receiver<SidecarEvent>, Box<dyn SidecarChild>), String> {
        let mut child = Command::new(&self.executable)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("Cannot start bundled ACI executable: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Cannot read ACI standard output".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "Cannot read ACI standard error".to_string())?;
        let (events, receiver) = mpsc::channel(256);
        let stdout_task = tokio::spawn(read_stream(stdout, events.clone(), StreamKind::Stdout));
        let stderr_task = tokio::spawn(read_stream(stderr, events.clone(), StreamKind::Stderr));
        let (stop, stopped) = oneshot::channel();
        tokio::spawn(async move {
            let wait = tokio::select! {
                result = child.wait() => result,
                _ = stopped => {
                    let _ = child.kill().await;
                    child.wait().await
                }
            };
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            if let Err(error) = wait {
                let _ = events
                    .send(SidecarEvent::Error(format!("Cannot wait for ACI: {error}")))
                    .await;
            }
            let _ = events.send(SidecarEvent::Terminated).await;
        });
        Ok((receiver, Box::new(TokioSidecarChild(Some(stop)))))
    }
}

struct TokioSidecarChild(Option<oneshot::Sender<()>>);

impl SidecarChild for TokioSidecarChild {
    fn kill(&mut self) -> Result<(), String> {
        if let Some(stop) = self.0.take() {
            let _ = stop.send(());
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum StreamKind {
    Stdout,
    Stderr,
}

async fn read_stream(
    mut reader: impl AsyncRead + Unpin,
    events: mpsc::Sender<SidecarEvent>,
    kind: StreamKind,
) {
    let mut buffer = vec![0; 16 * 1024];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                let _ = events
                    .send(SidecarEvent::Error(format!(
                        "Cannot read ACI process output: {error}"
                    )))
                    .await;
                break;
            }
        };
        let event = match kind {
            StreamKind::Stdout => SidecarEvent::Stdout(buffer[..read].to_vec()),
            StreamKind::Stderr => SidecarEvent::Stderr(buffer[..read].to_vec()),
        };
        if events.send(event).await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_an_absolute_executable_path() {
        assert!(TokioSidecarLauncher::new(PathBuf::from("aci")).is_err());
        let absolute = if cfg!(windows) {
            PathBuf::from(r"C:\Program Files\Private AI Gateway\aci.exe")
        } else {
            PathBuf::from("/Applications/Private AI Gateway.app/Contents/MacOS/aci")
        };
        assert!(TokioSidecarLauncher::new(absolute).is_ok());
    }
}
