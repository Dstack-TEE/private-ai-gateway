use clap::Parser;
use desktop_runtime::{
    controller::{DesktopRuntime, RuntimeOptions},
    process::TokioSidecarLauncher,
};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "private-ai-proxy-service", version = desktop_runtime::protocol::BUILD_VERSION, about = "Run the per-user Private AI Proxy backend in the foreground")]
struct Arguments {}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Private AI Proxy backend: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    if let Some(status) = desktop_runtime::process::run_sidecar_supervisor_if_requested()? {
        if status.success() {
            return Ok(());
        }
        return Err("ACI supervisor exited unsuccessfully".into());
    }
    Arguments::parse();
    let executable = std::env::current_exe().map_err(|_| "Cannot locate backend")?;
    let directory = executable
        .parent()
        .ok_or("Cannot locate backend directory")?;
    let name = |name: &str| {
        if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        }
    };
    let launcher = Arc::new(TokioSidecarLauncher::new(
        directory.join(name("private-ai-proxy")),
    )?);
    let options = RuntimeOptions {
        launcher,
        helper_path: directory.join(name("private-ai-proxy-helper")),
        task_runtime: tokio::runtime::Handle::current(),
    };
    // Runtime initialization uses synchronous persistence APIs outside executor workers.
    let runtime = tokio::task::spawn_blocking(move || DesktopRuntime::launch(options))
        .await
        .map_err(|_| "Backend initialization failed")??;
    let power_monitor = match desktop_runtime::power::Monitor::start(
        Arc::downgrade(&runtime),
        tokio::runtime::Handle::current(),
    ) {
        Ok(monitor) => Some(monitor),
        Err(error) => {
            eprintln!("System wake monitoring is unavailable: {error}");
            None
        }
    };
    let result = desktop_runtime::server::serve(runtime).await;
    drop(power_monitor);
    result
}
