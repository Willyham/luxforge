//! Bounded incremental diagnostics. No filesystem work runs in widget callbacks.
use serde_json::{Value, json};
use std::{fs::OpenOptions, io::Write, path::Path, sync::mpsc};

/// How many events one log holds. Past it events are counted rather than written, and the count is
/// the one truncation record written when the log finishes.
const EVENT_CAP: usize = 4096;

#[derive(Clone)]
pub struct Diagnostics {
    sender: mpsc::SyncSender<Record>,
}
enum Record {
    Event(Value),
    Finish(mpsc::SyncSender<bool>),
}
impl Diagnostics {
    pub fn start(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create_new(true).write(true).open(path)?;
        Ok(Self::writer(file))
    }
    fn writer(mut file: impl Write + Send + 'static) -> Self {
        let (sender, receiver) = mpsc::sync_channel(256);
        std::thread::spawn(move || {
            let mut count = 0;
            let mut dropped = 0u64;
            let mut failed = false;
            while let Ok(record) = receiver.recv() {
                match record {
                    Record::Event(_) if count >= EVENT_CAP => dropped += 1,
                    Record::Event(value) if !failed => {
                        count += 1;
                        if writeln!(file, "{value}")
                            .and_then(|_| file.flush())
                            .is_err()
                        {
                            eprintln!(
                                "diagnostics: incremental log write failed; viewing continues"
                            );
                            failed = true;
                        }
                    }
                    Record::Finish(done) => {
                        // A truncated log is incomplete, so it fails the finish as a write
                        // failure does, after saying how much it lost.
                        if dropped > 0 {
                            eprintln!(
                                "diagnostics: log truncated at {EVENT_CAP} events; {dropped} dropped"
                            );
                            let record = json!({"event":"diagnostics_truncated","cap":EVENT_CAP,"dropped":dropped});
                            if !failed && writeln!(file, "{record}").is_err() {
                                eprintln!("diagnostics: truncation record write failed");
                            }
                            failed = true;
                        }
                        if let Err(error) = file.flush() {
                            failed = true;
                            eprintln!("diagnostics: log flush failed: {}", error.kind());
                        }
                        let _ = done.send(!failed);
                        break;
                    }
                    _ => {}
                }
            }
        });
        Self { sender }
    }
    pub fn event(&self, value: Value) {
        if self.sender.try_send(Record::Event(value)).is_err() {
            eprintln!("diagnostics: log queue unavailable/full; event dropped");
        }
    }
    /// Call on the task executor, never the UI thread.
    pub fn finish(self) -> bool {
        let (tx, rx) = mpsc::sync_channel(1);
        if self.sender.send(Record::Finish(tx)).is_ok() {
            return rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap_or(false);
        }
        false
    }
    pub fn panic_hook(&self, run: String) {
        let log = self.clone();
        std::panic::set_hook(Box::new(move |_| {
            // Panic payloads can contain private paths; deliberately omit them.
            eprintln!("internal: Luxforge panicked; inspect retained diagnostics");
            log.event(json!({"event":"panic","run_id":run}));
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn write_failure_is_reported_without_panicking() {
        struct Failed;
        impl Write for Failed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::PermissionDenied.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let log = Diagnostics::writer(Failed);
        log.event(json!({"event":"startup"}));
        assert!(!log.finish());
    }
    /// A writer whose bytes the test reads back once the log has finished.
    #[derive(Clone, Default)]
    struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl Write for Shared {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Shared {
        fn records(&self) -> Vec<Value> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }
    }

    /// Send `count` events the way a busy run does, every one reaching the writer: the queue is
    /// bounded, so the test waits for room instead of dropping at the queue.
    fn send_events(log: &Diagnostics, count: usize) {
        for index in 0..count {
            log.sender
                .send(Record::Event(json!({"event":"input","index":index})))
                .unwrap();
        }
    }

    #[test]
    fn events_past_the_cap_are_counted_in_one_record_and_fail_the_finish() {
        let file = Shared::default();
        let log = Diagnostics::writer(file.clone());
        send_events(&log, EVENT_CAP + 1);
        assert!(!log.finish(), "a truncated log is an incomplete one");
        let records = file.records();
        assert_eq!(records.len(), EVENT_CAP + 1);
        assert_eq!(records[EVENT_CAP - 1]["index"], json!(EVENT_CAP - 1));
        assert_eq!(
            records[EVENT_CAP],
            json!({"event":"diagnostics_truncated","cap":4096,"dropped":1})
        );
    }

    #[test]
    fn events_under_the_cap_finish_with_no_truncation_record() {
        let file = Shared::default();
        let log = Diagnostics::writer(file.clone());
        send_events(&log, EVENT_CAP - 1);
        assert!(log.finish());
        let records = file.records();
        assert_eq!(records.len(), EVENT_CAP - 1);
        assert!(
            records
                .iter()
                .all(|record| record["event"] != "diagnostics_truncated")
        );
    }

    #[test]
    fn incremental_records_survive_before_shutdown_and_refuse_reuse() {
        let path = std::env::temp_dir().join(format!(
            "luxforge-log-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let log = Diagnostics::start(&path).unwrap();
        log.event(json!({"event":"startup"}));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::fs::read_to_string(&path).unwrap().is_empty() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(Diagnostics::start(&path).is_err());
        log.finish();
        assert!(std::fs::read_to_string(&path).unwrap().contains("startup"));
        std::fs::remove_file(path).unwrap();
    }
}
