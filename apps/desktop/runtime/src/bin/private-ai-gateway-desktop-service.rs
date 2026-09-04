use std::{path::PathBuf, sync::Arc};

use desktop_runtime::{
    controller::{DesktopRuntime, RuntimeOptions},
    process::TokioSidecarLauncher,
    server,
};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Desktop runtime failed: {}", error.message);
        std::process::exit(1);
    }
}

async fn run() -> Result<(), desktop_runtime::protocol::Error> {
    let executable = std::env::current_exe().map_err(|error| {
        desktop_runtime::protocol::Error::new(
            "startup_failed",
            format!("Cannot locate the desktop runtime executable: {error}"),
        )
    })?;
    let directory = executable.parent().ok_or_else(|| {
        desktop_runtime::protocol::Error::new(
            "startup_failed",
            "Cannot locate the desktop runtime directory",
        )
    })?;
    let aci = sibling(directory, "aci");
    let helper = sibling(directory, "private-ai-gateway-helper");
    let launcher = Arc::new(TokioSidecarLauncher::new(aci).map_err(startup_error)?);
    let runtime = DesktopRuntime::launch(RuntimeOptions {
        launcher,
        helper_path: helper,
    })
    .map_err(startup_error)?;
    server::run_stdio(runtime).await
}

fn sibling(directory: &std::path::Path, name: &str) -> PathBuf {
    if cfg!(windows) {
        directory.join(format!("{name}.exe"))
    } else {
        directory.join(name)
    }
}

fn startup_error(message: String) -> desktop_runtime::protocol::Error {
    desktop_runtime::protocol::Error::new("startup_failed", message)
}
