use clap::Parser;
use desktop_runtime::controller::{DesktopRuntime, RuntimeOptions};
use private_ai_proxy::serve::managed::InProcessVerifierLauncher;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "private-ai-proxy-service", version = desktop_core::protocol::BUILD_VERSION, about = "Run the per-user Private AI Proxy backend in the foreground")]
struct Arguments {}

#[tokio::main]
async fn main() {
    private_ai_proxy::install_crypto_provider();
    if let Err(error) = run().await {
        desktop_core::diagnostic!("Private AI Proxy backend: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    Arguments::parse();
    #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
    let service_access = desktop_core::agent_access::acquire_for_service();
    #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
    let (agent_home_access, agent_access_error) = match service_access {
        Ok(access) => (access, None),
        Err(error) => {
            desktop_core::diagnostic!("Private AI Proxy backend: {error}");
            (None, Some(error))
        }
    };
    #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
    let agent_configuration = agent_home_access.is_some();
    #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
    let agent_home = agent_home_access
        .as_ref()
        .map(|access| access.home().to_path_buf());
    #[cfg(not(all(target_os = "macos", feature = "mac-app-store")))]
    let agent_configuration = true;
    #[cfg(not(all(target_os = "macos", feature = "mac-app-store")))]
    let agent_access_error = None;
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
    let launcher = Arc::new(InProcessVerifierLauncher::new(
        tokio::runtime::Handle::current(),
    ));
    let options = RuntimeOptions {
        launcher,
        helper_path: directory.join(name("private-ai-proxy-helper")),
        task_runtime: tokio::runtime::Handle::current(),
        agent_configuration,
        agent_access_error,
        #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
        agent_home,
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
            desktop_core::diagnostic!("System wake monitoring is unavailable: {error}");
            None
        }
    };
    let result = desktop_runtime::server::serve(runtime).await;
    drop(power_monitor);
    #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
    drop(agent_home_access);
    result
}
