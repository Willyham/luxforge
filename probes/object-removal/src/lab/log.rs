//! The lab's log: one JSON object per line, written to stdout and to a file in the output
//! directory, and kept in memory for the script summary and tests.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

pub struct Log {
    start: Instant,
    path: Option<PathBuf>,
    file: Mutex<Option<BufWriter<File>>>,
    echo: bool,
    records: Mutex<Vec<Value>>,
}

impl Log {
    /// A new `lab-<unix ms>.jsonl` in `dir`, never an existing file, echoed to stdout.
    pub fn create(dir: &Path) -> Result<Log, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let path = dir.join(format!("lab-{stamp}.jsonl"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Log {
            start: Instant::now(),
            path: Some(path),
            file: Mutex::new(Some(BufWriter::new(file))),
            echo: true,
            records: Mutex::new(Vec::new()),
        })
    }

    /// A log kept in memory only, for tests.
    pub fn memory() -> Log {
        Log {
            start: Instant::now(),
            path: None,
            file: Mutex::new(None),
            echo: false,
            records: Mutex::new(Vec::new()),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Milliseconds since the log was opened.
    pub fn now_ms(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1e3
    }

    /// Records `event` with `fields` (an object) and the time since the log opened. A write
    /// failure is reported once on stderr and the file is dropped; the in-memory record stays.
    pub fn event(&self, event: &str, fields: Value) {
        let mut record = Map::new();
        record.insert("t_ms".into(), Value::from(round3(self.now_ms())));
        record.insert("event".into(), Value::from(event));
        if let Value::Object(fields) = fields {
            record.extend(fields);
        }
        let record = Value::Object(record);
        let line = record.to_string();
        if self.echo {
            println!("{line}");
        }
        if let Ok(mut file) = self.file.lock()
            && let Some(f) = file.as_mut()
            && let Err(e) = writeln!(f, "{line}").and_then(|()| f.flush())
        {
            eprintln!("lab log: {e}; the rest of this session is logged to stdout only");
            *file = None;
        }
        if let Ok(mut records) = self.records.lock() {
            records.push(record);
        }
    }

    /// Every record so far with this event name.
    pub fn records(&self, event: &str) -> Vec<Value> {
        self.records.lock().map_or_else(
            |_| Vec::new(),
            |r| r.iter().filter(|v| v["event"] == event).cloned().collect(),
        )
    }
}

/// Milliseconds to three decimals, so the log stays readable.
pub fn round3(ms: f64) -> f64 {
    (ms * 1e3).round() / 1e3
}

pub fn ms(d: std::time::Duration) -> f64 {
    round3(d.as_secs_f64() * 1e3)
}
