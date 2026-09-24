//! Diagnostics are `tracing` events at `INFO` and above. Every process prints
//! them to stderr as bare lines; the detached service also appends them to a
//! daily log file under [`crate::paths::logs_dir`], because its stderr goes
//! away with the client that started it.

use tracing_subscriber::{
    filter::LevelFilter,
    fmt::{self, MakeWriter},
    layer::SubscriberExt,
    registry::LookupSpan,
    util::SubscriberInitExt,
    Layer,
};

/// Log to stderr: the CLI and the desktop shell.
pub fn init() {
    let _ = tracing_subscriber::registry()
        .with(LevelFilter::INFO)
        .with(stderr())
        .try_init();
}

/// Log to stderr and to `file`, with timestamps and levels: the service.
pub fn init_with_file<W>(file: W)
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let _ = tracing_subscriber::registry()
        .with(LevelFilter::INFO)
        .with(stderr())
        .with(
            fmt::layer()
                .with_writer(file)
                .with_ansi(false)
                .log_internal_errors(false),
        )
        .try_init();
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
