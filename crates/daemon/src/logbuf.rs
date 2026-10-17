//! Logging: a small in-memory ring the GUI can show, mirrored to stderr
//! (captured by journald on Linux) and to a size-capped file on Windows.
//!
//! Key codes, clipboard contents and file names are never logged.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

const KEEP_LINES: usize = 300;
const MAX_LOG_FILE: u64 = 1024 * 1024;

#[derive(Clone, Default)]
pub struct LogBuffer(Arc<Mutex<VecDeque<String>>>);

impl LogBuffer {
    pub fn lines(&self) -> Vec<String> {
        self.0
            .lock()
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn push(&self, line: String) {
        if let Ok(mut q) = self.0.lock() {
            if q.len() == KEEP_LINES {
                q.pop_front();
            }
            q.push_back(line);
        }
    }
}

struct Sink {
    buffer: LogBuffer,
    file: Option<Mutex<std::fs::File>>,
}

#[derive(Default)]
struct Line(String);

impl Visit for Line {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for Sink {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut body = Line::default();
        event.record(&mut body);
        let (h, m, s) = crate::platform::local_clock();
        let level = match *event.metadata().level() {
            Level::ERROR => "ERROR",
            Level::WARN => "WARN ",
            Level::INFO => "INFO ",
            _ => "DEBUG",
        };
        let line = format!("{h:02}:{m:02}:{s:02} {level} {}", body.0);
        eprintln!("{line}");
        if let Some(file) = &self.file {
            if let Ok(mut f) = file.lock() {
                let _ = writeln!(f, "{line}");
            }
        }
        self.buffer.push(line);
    }
}

/// Install the global logger. `to_file` additionally appends to the log file,
/// starting it afresh once it grows past a megabyte.
pub fn init(to_file: bool) -> LogBuffer {
    let buffer = LogBuffer::default();
    let file = to_file.then(open_log_file).flatten().map(Mutex::new);
    let level = match std::env::var("YZENDRIS_LOG").as_deref() {
        Ok("debug") | Ok("trace") => Level::DEBUG,
        Ok("warn") => Level::WARN,
        _ => Level::INFO,
    };
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::from_level(level))
        .with(Sink {
            buffer: buffer.clone(),
            file,
        })
        .try_init();
    buffer
}

fn open_log_file() -> Option<std::fs::File> {
    let path = yzendris_core::paths::log_file();
    yzendris_core::config::create_private_dir(path.parent()?).ok()?;
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_FILE) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}
