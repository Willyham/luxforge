//! Bounded incremental diagnostics. No filesystem work runs in widget callbacks.
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::mpsc,
};

/// How many events one log holds: the launch's first [`HEAD_EVENTS`] and, past them, its newest,
/// with one truncation record between the two once any were let go. At a few hundred bytes an
/// event the file stays within a few megabytes however long the session runs.
const EVENT_CAP: usize = 16_384;
/// The launch's first events, which the log keeps whatever follows: its startup, its modules and
/// its first open, which say what the rest of the log is about.
const HEAD_EVENTS: usize = 256;
/// The newest events the log keeps past its head: the cap less the head and the truncation record.
const TAIL_EVENTS: usize = EVENT_CAP - HEAD_EVENTS - 1;
/// How many of the oldest tail events go at once when the tail is full, so the file is written
/// again once every this many events rather than at every one.
const TAIL_DROP: usize = TAIL_EVENTS / 2;

#[derive(Clone)]
pub struct Diagnostics {
    sender: mpsc::SyncSender<Record>,
}
enum Record {
    Event(Value),
    Finish(mpsc::SyncSender<bool>),
}

/// What a log is written to: appended to as each event arrives, and cut back and written again past
/// its head when its oldest newest events go. The file in production; the tests' own bytes.
trait LogFile: Read + Write + Seek + Send + 'static {
    fn set_len(&mut self, len: u64) -> std::io::Result<()>;
}
impl LogFile for File {
    fn set_len(&mut self, len: u64) -> std::io::Result<()> {
        File::set_len(self, len)
    }
}

/// The log's file and where its parts are. Every event is written and flushed as it arrives, so a
/// stall or a crash leaves the log up to its last event; the tail's events are read back from the
/// file when the oldest go, so the writer holds their offsets and not their text.
struct Log<F> {
    file: F,
    /// Events written into the head.
    head: usize,
    /// Where the head ends: where the truncation record, then the tail, begin.
    head_end: u64,
    /// The byte offset of each tail event in the file, oldest first.
    tail: VecDeque<u64>,
    /// The file's length: where the next event goes.
    end: u64,
    /// The events let go between the head and the tail, which the truncation record counts.
    dropped: u64,
}

impl<F: LogFile> Log<F> {
    fn new(file: F) -> Self {
        Self {
            file,
            head: 0,
            head_end: 0,
            tail: VecDeque::new(),
            end: 0,
            dropped: 0,
        }
    }

    fn write(&mut self, value: &Value) -> std::io::Result<()> {
        if self.head < HEAD_EVENTS {
            self.append(value)?;
            self.head += 1;
            self.head_end = self.end;
            return Ok(());
        }
        if self.tail.len() == TAIL_EVENTS {
            self.drop_oldest(TAIL_DROP)?;
        }
        self.tail.push_back(self.end);
        self.append(value)
    }

    fn append(&mut self, value: &Value) -> std::io::Result<()> {
        let line = format!("{value}\n");
        self.file.seek(SeekFrom::Start(self.end))?;
        self.file.write_all(line.as_bytes())?;
        self.file.flush()?;
        self.end += line.len() as u64;
        Ok(())
    }

    /// Let the oldest `count` tail events go: the file is cut back to its head and written again
    /// with the truncation record, now counting them too, and the tail events it keeps.
    fn drop_oldest(&mut self, count: usize) -> std::io::Result<()> {
        let keep_from = self.tail[count];
        let mut kept = Vec::with_capacity((self.end - keep_from) as usize);
        self.file.seek(SeekFrom::Start(keep_from))?;
        (&mut self.file)
            .take(self.end - keep_from)
            .read_to_end(&mut kept)?;
        self.dropped += count as u64;
        let record = format!("{}\n", truncation(self.dropped));
        self.file.set_len(self.head_end)?;
        self.file.seek(SeekFrom::Start(self.head_end))?;
        self.file.write_all(record.as_bytes())?;
        self.file.write_all(&kept)?;
        self.file.flush()?;
        let start = self.head_end + record.len() as u64;
        self.tail.drain(..count);
        for offset in &mut self.tail {
            *offset = *offset - keep_from + start;
        }
        self.end = start + kept.len() as u64;
        Ok(())
    }
}

/// The record between the head and the tail once events have gone: how many went, all of them from
/// between the two, and what the log keeps.
fn truncation(dropped: u64) -> Value {
    json!({"event":"diagnostics_truncated","cap":EVENT_CAP,"head":HEAD_EVENTS,"dropped":dropped})
}

impl Diagnostics {
    pub fn start(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)?;
        Ok(Self::writer(file))
    }
    fn writer(file: impl LogFile) -> Self {
        let (sender, receiver) = mpsc::sync_channel(256);
        std::thread::spawn(move || {
            let mut log = Log::new(file);
            let mut failed = false;
            while let Ok(record) = receiver.recv() {
                match record {
                    Record::Event(value) if !failed => {
                        if log.write(&value).is_err() {
                            eprintln!(
                                "diagnostics: incremental log write failed; viewing continues"
                            );
                            failed = true;
                        }
                    }
                    Record::Event(_) => {}
                    Record::Finish(done) => {
                        // A log that let events go is incomplete, so it fails the finish as a write
                        // failure does, after saying how much it lost; its record says so in the file.
                        if log.dropped > 0 {
                            eprintln!(
                                "diagnostics: log kept its first {HEAD_EVENTS} and newest events; {} dropped between them",
                                log.dropped
                            );
                            failed = true;
                        }
                        if let Err(error) = log.file.flush() {
                            failed = true;
                            eprintln!("diagnostics: log flush failed: {}", error.kind());
                        }
                        let _ = done.send(!failed);
                        break;
                    }
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
        self.finish_within(std::time::Duration::from_secs(2))
    }
    /// [`Self::finish`] waiting up to `bound` for the writer: a test waits for the hang bound, so
    /// a loaded host cannot turn a slow flush into a failed one.
    pub(crate) fn finish_within(self, bound: std::time::Duration) -> bool {
        let (tx, rx) = mpsc::sync_channel(1);
        if self.sender.send(Record::Finish(tx)).is_ok() {
            return rx.recv_timeout(bound).unwrap_or(false);
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
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

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
        impl Read for Failed {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Ok(0)
            }
        }
        impl Seek for Failed {
            fn seek(&mut self, _: SeekFrom) -> std::io::Result<u64> {
                Ok(0)
            }
        }
        impl LogFile for Failed {
            fn set_len(&mut self, _: u64) -> std::io::Result<()> {
                Ok(())
            }
        }
        let log = Diagnostics::writer(Failed);
        log.event(json!({"event":"startup"}));
        assert!(!log.finish());
    }

    /// A file in memory whose bytes the test reads back once the log has finished.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Cursor<Vec<u8>>>>);
    impl Write for Shared {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Read for Shared {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().read(bytes)
        }
    }
    impl Seek for Shared {
        fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
            self.0.lock().unwrap().seek(to)
        }
    }
    impl LogFile for Shared {
        fn set_len(&mut self, len: u64) -> std::io::Result<()> {
            self.0.lock().unwrap().get_mut().truncate(len as usize);
            Ok(())
        }
    }
    impl Shared {
        fn records(&self) -> Vec<Value> {
            records(&String::from_utf8(self.0.lock().unwrap().get_ref().clone()).unwrap())
        }
    }

    fn records(text: &str) -> Vec<Value> {
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
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

    /// The log of `sent` events past the cap: the launch's first [`HEAD_EVENTS`], one truncation
    /// record counting every event let go, then the newest, in order and ending with the last sent.
    fn assert_head_record_and_newest(records: &[Value], sent: usize) {
        assert!(records.len() <= EVENT_CAP, "{} records", records.len());
        for (index, record) in records[..HEAD_EVENTS].iter().enumerate() {
            assert_eq!(record["index"], json!(index));
        }
        let tail = &records[HEAD_EVENTS + 1..];
        let dropped = sent - HEAD_EVENTS - tail.len();
        assert_eq!(
            records[HEAD_EVENTS],
            json!({"event":"diagnostics_truncated","cap":EVENT_CAP,"head":HEAD_EVENTS,"dropped":dropped})
        );
        assert!(
            tail.len() >= TAIL_EVENTS - TAIL_DROP,
            "{} newest kept",
            tail.len()
        );
        for (offset, record) in tail.iter().enumerate() {
            assert_eq!(record["index"], json!(HEAD_EVENTS + dropped + offset));
        }
        assert_eq!(tail.last().unwrap()["index"], json!(sent - 1));
    }

    #[test]
    fn a_long_log_keeps_its_head_and_its_newest_events_and_counts_the_rest() {
        for sent in [EVENT_CAP, EVENT_CAP + 1, 3 * EVENT_CAP + 17] {
            let file = Shared::default();
            let log = Diagnostics::writer(file.clone());
            send_events(&log, sent);
            assert!(
                !log.finish_within(std::time::Duration::from_secs(60)),
                "a log that let events go is an incomplete one"
            );
            assert_head_record_and_newest(&file.records(), sent);
        }
    }

    #[test]
    fn events_within_the_cap_finish_with_no_truncation_record() {
        let file = Shared::default();
        let log = Diagnostics::writer(file.clone());
        send_events(&log, EVENT_CAP - 1);
        assert!(log.finish_within(std::time::Duration::from_secs(60)));
        let records = file.records();
        assert_eq!(records.len(), EVENT_CAP - 1);
        for (index, record) in records.iter().enumerate() {
            assert_eq!(record["index"], json!(index));
        }
    }

    fn temporary(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "luxforge-log-{name}-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn incremental_records_survive_before_shutdown_and_refuse_reuse() {
        let path = temporary("incremental");
        let log = Diagnostics::start(&path).unwrap();
        log.event(json!({"event":"startup"}));
        luxforge_testbase::wait_until("the startup record reaches the file", || {
            !std::fs::read_to_string(&path).unwrap().is_empty()
        });
        assert!(Diagnostics::start(&path).is_err());
        log.finish();
        assert!(std::fs::read_to_string(&path).unwrap().contains("startup"));
        std::fs::remove_file(path).unwrap();
    }

    /// The file on disk is cut back and written again in place as its tail turns over, and holds
    /// what the writer means it to up to the last event it was sent, before the finish and after.
    #[test]
    fn a_long_log_on_disk_is_rewritten_in_place_within_the_cap() {
        let path = temporary("long");
        let log = Diagnostics::start(&path).unwrap();
        let sent = 2 * EVENT_CAP + 5;
        send_events(&log, sent);
        let last = format!("\"index\":{}}}", sent - 1);
        luxforge_testbase::wait_until("the newest event reaches the file", || {
            std::fs::read_to_string(&path).unwrap().contains(&last)
        });
        assert_head_record_and_newest(&records(&std::fs::read_to_string(&path).unwrap()), sent);
        assert!(!log.finish_within(std::time::Duration::from_secs(60)));
        assert_head_record_and_newest(&records(&std::fs::read_to_string(&path).unwrap()), sent);
        std::fs::remove_file(path).unwrap();
    }
}
