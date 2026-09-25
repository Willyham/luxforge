//! Bounded incremental diagnostics. No filesystem work runs in widget callbacks.
use serde_json::{Value, json};
use std::{fs::OpenOptions, io::Write, path::Path, sync::mpsc};

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
            let mut failed = false;
            while let Ok(record) = receiver.recv() {
                match record {
                    Record::Event(value) if count < 4096 && !failed => {
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
