//! Diagnostics are `tracing` events; what the CLI tells its user (results,
//! prompts, hints such as deprecation warnings) is printed directly and never
//! goes through this. Every process prints this app's own
//! events at `INFO` and above to stderr as bare lines. The detached service
//! also appends them, plus its libraries' warnings and errors, to a daily log
//! file under [`crate::paths::logs_dir`], because its stderr goes away with
//! the client that started it.

use tracing_subscriber::{
    filter::{LevelFilter, Targets},
    fmt::{self, MakeWriter},
    layer::SubscriberExt,
    registry::LookupSpan,
    util::SubscriberInitExt,
    Layer,
};

/// Log to stderr: the CLI and the desktop shell.
pub fn init() {
    let _ = tracing_subscriber::registry().with(stderr()).try_init();
}

/// Log to stderr and to `file`, with timestamps and levels: the service.
pub fn init_with_file<W>(file: W)
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let _ = tracing_subscriber::registry()
        .with(stderr())
        .with(
            fmt::layer()
                .with_writer(file)
                .with_ansi(false)
                .log_internal_errors(false)
                .with_filter(file_events()),
        )
        .try_init();
}

/// This workspace's crates (targets are module paths, matched by prefix:
/// `private_ai_proxy` also covers the service and desktop binaries).
fn app_events() -> Targets {
    Targets::new().with_targets(
        ["desktop_core", "desktop_runtime", "private_ai_proxy"]
            .map(|target| (target, LevelFilter::INFO)),
    )
}

/// The service log also keeps other crates' warnings and errors.
fn file_events() -> Targets {
    app_events().with_default(LevelFilter::WARN)
}

/// Bare lines like `eprintln!`, except that a closed or detached stderr (a GUI
/// process, a service whose client exited) never fails or panics the process.
fn stderr<S>() -> impl Layer<S>
where
    S: tracing::Subscriber + for<'span> LookupSpan<'span>,
{
    fmt::layer()
        .with_writer(std::io::stderr)
        .without_time()
        .with_level(false)
        .with_target(false)
        .with_ansi(false)
        .log_internal_errors(false)
        .with_filter(app_events())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn logged(filter: Targets) -> String {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let buffer = lines.clone();
        let writer = move || Capture(buffer.clone());
        let subscriber = tracing_subscriber::registry()
            .with(fmt::layer().with_writer(writer).with_filter(filter));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("app info");
            tracing::debug!("app debug");
            tracing::info!(target: "private_ai_proxy_service", "service info");
            tracing::info!(target: "hyper_util::client", "library info");
            tracing::warn!(target: "hyper_util::client", "library warning");
        });
        let text = String::from_utf8(lines.lock().unwrap().clone()).unwrap();
        text
    }

    #[test]
    fn stderr_keeps_app_events_and_the_file_adds_library_warnings() {
        let stderr = logged(app_events());
        assert!(stderr.contains("app info") && stderr.contains("service info"));
        assert!(!stderr.contains("app debug") && !stderr.contains("library"));
        let file = logged(file_events());
        assert!(file.contains("app info") && file.contains("library warning"));
        assert!(!file.contains("app debug") && !file.contains("library info"));
    }

    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
