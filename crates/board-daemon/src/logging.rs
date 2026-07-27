//! Daemon logging setup: an append-mode file under the board data dir, plus
//! stderr when running in the foreground.

use std::fs::OpenOptions;
use std::sync::Arc;

use board_core::paths;

pub(crate) fn init_logging(foreground: bool) {
    use tracing_subscriber::fmt::writer::MakeWriterExt;
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let log_path = paths::log_path();

    // Anything that goes wrong *before* a subscriber exists cannot be
    // `tracing!`-ed. Collect it and replay it once the outcome is known.
    let mut deferred = Vec::<String>::new();
    if let Some(parent) = log_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            deferred.push(format!(
                "cannot create log directory {}: {e}",
                parent.display()
            ));
        }
    }
    let file = OpenOptions::new().create(true).append(true).open(&log_path);

    let installed = match file {
        Ok(f) => {
            let f = Arc::new(f);
            if foreground {
                tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_writer(FileWriter(f).and(std::io::stderr))
                    .try_init()
            } else {
                tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_writer(FileWriter(f))
                    .try_init()
            }
        }
        Err(e) => {
            deferred.push(format!(
                "cannot open log file {}: {e}; falling back to stderr",
                log_path.display()
            ));
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .try_init()
        }
    };

    match installed {
        Ok(()) => {
            for message in deferred {
                tracing::warn!("logging setup: {message}");
            }
        }
        // A swallowed failure here leaves the daemon permanently blind — every
        // later diagnostic silently goes nowhere. Say so on the one channel
        // that is still guaranteed to work.
        Err(e) => {
            for message in deferred {
                eprintln!("boardd: logging setup: {message}");
            }
            eprintln!(
                "boardd: no tracing subscriber could be installed ({e}); \
                 this process will produce no logs"
            );
        }
    }
}

/// A `MakeWriter` over a shared append-mode log file.
struct FileWriter(Arc<std::fs::File>);
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for FileWriter {
    type Writer = &'a std::fs::File;
    fn make_writer(&'a self) -> Self::Writer {
        &self.0
    }
}
