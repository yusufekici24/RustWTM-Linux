use std::path::Path;

use tracing::Subscriber;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

/// Daily log files kept; older ones are removed when the service starts and
/// at each new day.
pub const LOG_FILES_KEPT: usize = 14;

/// Logs to a daily rolling file and, in console mode, to stdout.
/// Keep the returned guard alive to flush the file on exit.
pub fn init(logs_dir: &Path, console: bool) -> WorkerGuard {
    let (subscriber, guard) = subscriber(logs_dir, console);
    subscriber.init();
    guard
}

fn subscriber(
    logs_dir: &Path,
    console: bool,
) -> (impl Subscriber + Send + Sync + 'static, WorkerGuard) {
    let _ = std::fs::create_dir_all(logs_dir);
    let appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("wtm-service.log")
        .max_log_files(LOG_FILES_KEPT)
        .build(logs_dir);
    let (writer, guard) = match appender {
        Ok(appender) => tracing_appender::non_blocking(appender),
        // The service runs without its file log rather than not at all.
        Err(e) => {
            eprintln!("cannot open the log folder {}: {e}", logs_dir.display());
            tracing_appender::non_blocking(std::io::sink())
        }
    };

    let subscriber = tracing_subscriber::registry()
        .with(LevelFilter::INFO)
        .with(fmt::layer().with_writer(writer).with_ansi(false))
        .with(console.then(|| fmt::layer().with_writer(std::io::stdout)));
    (subscriber, guard)
}

#[cfg(test)]
mod tests {
    #[test]
    fn writes_to_a_daily_file() {
        let dir = std::env::temp_dir().join(format!("wtm-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Scoped: the global subscriber belongs to the console-mode test.
        let (subscriber, guard) = super::subscriber(&dir, false);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("hello from the logging test");
            tracing::debug!("below the level filter");
        });
        drop(guard); // flushes the non-blocking writer
        let files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert_eq!(files.len(), 1);
        let text = std::fs::read_to_string(files[0].path()).unwrap();
        assert!(text.contains("hello from the logging test"));
        assert!(!text.contains("below the level filter"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_daily_files_are_removed_and_other_files_kept() {
        let dir = std::env::temp_dir().join(format!("wtm-log-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for day in 1..=20 {
            std::fs::write(
                dir.join(format!("wtm-service.log.2026-10-{day:02}")),
                "old
",
            )
            .unwrap();
        }
        std::fs::write(
            dir.join("notes.txt"),
            "kept
",
        )
        .unwrap();
        let (subscriber, guard) = super::subscriber(&dir, false);
        tracing::subscriber::with_default(subscriber, || tracing::info!("today"));
        drop(guard);
        let logs = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("wtm-service.log")
            })
            .count();
        assert!(logs <= super::LOG_FILES_KEPT, "{logs} log files left");
        assert!(dir.join("notes.txt").exists(), "only the service's logs go");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unusable_log_folder_does_not_stop_the_service() {
        // A file where the folder should be.
        let path = std::env::temp_dir().join(format!("wtm-log-file-{}", std::process::id()));
        std::fs::write(&path, "not a folder").unwrap();
        let (subscriber, guard) = super::subscriber(&path, false);
        tracing::subscriber::with_default(subscriber, || tracing::warn!("still running"));
        drop(guard);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not a folder");
        let _ = std::fs::remove_file(&path);
    }
}
