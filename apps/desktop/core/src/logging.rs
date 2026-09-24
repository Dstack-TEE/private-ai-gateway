//! Diagnostics are this app's `tracing` events at `INFO` and above; other
//! crates' events are left out. Every process prints them to stderr as bare
//! lines; the detached service also appends them to a
//! daily log file under [`crate::paths::logs_dir`], because its stderr goes
//! away with the client that started it.

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
    let _ = tracing_subscriber::registry()
        .with(app_events())
        .with(stderr())
        .try_init();
}

/// Log to stderr and to `file`, with timestamps and levels: the service.
pub fn init_with_file<W>(file: W)
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let _ = tracing_subscriber::registry()
        .with(app_events())
        .with(stderr())
        .with(
            fmt::layer()
                .with_writer(file)
                .with_ansi(false)
                .log_internal_errors(false),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn only_this_apps_events_are_logged() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let buffer = lines.clone();
        let writer = move || Capture(buffer.clone());
        let subscriber = tracing_subscriber::registry()
            .with(app_events())
            .with(fmt::layer().with_writer(writer).without_time());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("kept");
            tracing::debug!("too verbose");
            tracing::warn!(target: "hyper_util::client", "dependency noise");
            tracing::info!(target: "private_ai_proxy_service", "service kept");
        });
        let text = String::from_utf8(lines.lock().unwrap().clone()).unwrap();
        assert!(
            text.contains("kept") && text.contains("service kept"),
            "{text}"
        );
        assert!(
            !text.contains("too verbose") && !text.contains("noise"),
            "{text}"
        );
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
