use clap::Parser;
use desktop_runtime::controller::{DesktopRuntime, RuntimeOptions};
use private_ai_proxy::serve::managed::InProcessVerifierLauncher;
use std::sync::Arc;
use tracing_appender::rolling::{RollingFileAppender, Rotation};

#[derive(Parser)]
#[command(name = "private-ai-proxy-service", version = desktop_core::protocol::BUILD_VERSION, about = "Run the per-user Private AI Proxy backend in the foreground")]
struct Arguments {}

#[tokio::main]
async fn main() {
    Arguments::parse();
    init_logging();
    tracing::info!(
        "Private AI Proxy backend {} starting (process {})",
        desktop_core::protocol::BUILD_VERSION,
        std::process::id()
    );
    private_ai_proxy::install_crypto_provider();
    if let Err(error) = run().await {
        tracing::error!("Private AI Proxy backend: {error}");
        std::process::exit(1);
    }
}

/// One file per day in the logs directory, the last week kept.
fn init_logging() {
    let file = desktop_core::paths::logs_dir().and_then(|directory| {
        desktop_core::private_fs::create_private_dir(&directory)
            .map_err(|error| error.to_string())?;
        RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix("service")
            .filename_suffix("log")
            .max_log_files(7)
            .build(directory)
            .map_err(|error| error.to_string())
    });
    match file {
        Ok(file) => desktop_core::logging::init_with_file(file),
        Err(error) => {
            desktop_core::logging::init();
            tracing::warn!("Private AI Proxy backend: cannot open the log file: {error}");
        }
    }
}

async fn run() -> Result<(), String> {
    #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
    let service_access = desktop_core::agent_access::acquire_for_service();
    #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
    let (agent_home_access, agent_access_error) = match service_access {
        Ok(access) => (access, None),
        Err(error) => {
            tracing::warn!("Private AI Proxy backend: {error}");
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
            tracing::warn!("System wake monitoring is unavailable: {error}");
            None
        }
    };
    let result = desktop_runtime::server::serve(runtime).await;
    drop(power_monitor);
    #[cfg(all(target_os = "macos", feature = "mac-app-store"))]
    drop(agent_home_access);
    result
}
