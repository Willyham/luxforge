//! The adapter's own headless catalog owner ([headless](../../../../docs/design/mcp.md#attaching-to-luxforge)):
//! a `luxforge-json --catalog PATH` child, started from beside the adapter's executable with edit
//! authority (never `--permission-authority`), spoken to in JSON lines over its standard streams
//! as one more [`Target`] and [`Connection`].
//!
//! - **Refusal first.** Before any child starts, a catalog the registry of running live sessions
//!   names is refused [`CATALOG_IN_USE`]: a desktop has it open.
//! - **Starting.** A child that cannot be started, or that ends or does not answer during its
//!   handshake (`schema.list` naming this protocol, then `session.identify`), is reported once as
//!   [`HEADLESS_FAILED`] with what it wrote to standard error; nothing is retried. The catalog's
//!   exclusive lock refusing a second owner is such a failure.
//! - **Diagnostics.** The child's standard error is forwarded, line by line, to the adapter's own
//!   by a thread that blocks on it, and its last [`STDERR_TAIL`] bytes are kept for a failure.
//! - **Answers.** A second thread reads the child's standard output, one bounded answer line at a
//!   time; its end is the child's end, which [`Connection::closed`] learns without sending.
//! - **Lifetime.** Dropping the connection, as the adapter does when its input ends or its session
//!   is replaced, closes the child's standard input, waits up to [`SHUTDOWN_GRACE`] for it to end,
//!   then kills it, and waits for it either way, so no child outlives its adapter.
use super::owner::{Attached, Connection, ListChanges, Target};
use crate::{
    Paths,
    live::{
        CONNECTION_LOST, RESPONSE_LIMIT, UNSUPPORTED_PROTOCOL, failure, read_answer, request_line,
        too_long,
    },
};
use luxforge_core::{ApiFailure, PROTOCOL, running_sessions};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, sync_channel},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// The failure code for a headless catalog a running desktop's registry entry names: it has the
/// catalog open, so a headless owner is not started for it.
pub const CATALOG_IN_USE: &str = "catalog-in-use";
/// The failure code for a headless owner that could not be started, or ended or stopped answering
/// during its handshake; its standard error is in the message and in `data.stderr`.
pub const HEADLESS_FAILED: &str = "headless-failed";
/// How long the child's handshake may take, from its start: opening the catalog, then answering
/// `schema.list` and `session.identify`.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a child whose standard input was closed may take to end before it is killed: it
/// finishes the request it is serving, stops its owner and closes the catalog.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
/// How much of the child's standard error is kept for a failure that reports it.
pub const STDERR_TAIL: usize = 8 * 1024;
/// The longest standard-error line forwarded whole; a longer one is forwarded in pieces.
const STDERR_LINE: usize = 64 * 1024;

/// The `luxforge-json` beside `executable`, as a build and its bundle place them.
pub fn beside(executable: &Path) -> PathBuf {
    executable.with_file_name(format!("luxforge-json{}", std::env::consts::EXE_SUFFIX))
}

/// A `luxforge-json` child of the adapter's own, over `catalog`.
pub struct HeadlessTarget {
    pub catalog: PathBuf,
    /// The `luxforge-json` to start ([`beside`] the adapter).
    pub binary: PathBuf,
    /// Where the desktop's configuration is, for the registry of running live sessions; with none,
    /// only the catalog's own lock refuses a second owner.
    pub paths: Option<Paths>,
}

impl Target for HeadlessTarget {
    fn attach(&mut self, label: &str) -> Result<Attached, ApiFailure> {
        let catalog = std::path::absolute(&self.catalog).unwrap_or_else(|_| self.catalog.clone());
        refuse_registered(&catalog, self.paths.as_ref())?;
        let mut child = HeadlessChild::start(&self.binary, &catalog)?;
        match child.handshake(label) {
            Ok(schema) => Ok(Attached {
                connection: Box::new(child),
                schema,
                catalog,
            }),
            Err(cause) => Err(child.failed(&cause)),
        }
    }

    /// The child serves the adapter alone, on one connection.
    fn list_changes(&self) -> ListChanges {
        ListChanges::OwnCalls
    }
}

/// Refuse `catalog` when a running desktop's registry entry names it. A registry that cannot be
/// read refuses nothing: the catalog's own lock still refuses a second owner, less clearly.
fn refuse_registered(catalog: &Path, paths: Option<&Paths>) -> Result<(), ApiFailure> {
    let Some(paths) = paths else {
        return Ok(());
    };
    let registry = paths.live_sessions();
    let running = match running_sessions(&registry) {
        Ok(running) => running,
        Err(error) => {
            eprintln!(
                "luxforge-mcp: the registry of running live sessions cannot be read ({}); the \
                 catalog's own lock still refuses a second owner",
                error.detail
            );
            return Ok(());
        }
    };
    for running in &running {
        if let Ok(entry) = &running.entry
            && same_catalog(&entry.catalog, catalog)
        {
            return Err(failure(
                CATALOG_IN_USE,
                format!(
                    "Luxforge (process {}) has the catalog {} open, so luxforge-mcp --headless \
                     does not open it too; quit Luxforge or open another catalog there, or set \
                     luxforge-mcp up without --headless to work in the running Luxforge",
                    entry.pid,
                    catalog.display()
                ),
                Some(json!({"catalog": catalog, "pid": entry.pid, "entry": running.file})),
            ));
        }
    }
    Ok(())
}

/// Whether two paths name the same catalog: the same absolute path, or the same file once both
/// are resolved.
fn same_catalog(registered: &Path, catalog: &Path) -> bool {
    registered == catalog
        || matches!(
            (std::fs::canonicalize(registered), std::fs::canonicalize(catalog)),
            (Ok(registered), Ok(catalog)) if registered == catalog
        )
}

/// One answer line from the child, or the mark of one too long to keep.
enum Answer {
    Line(Vec<u8>),
    TooLong,
}

/// A running `luxforge-json` child and the two threads reading its output.
struct HeadlessChild {
    child: Child,
    stdin: Option<ChildStdin>,
    answers: Receiver<Answer>,
    stdout: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<()>>,
    /// The last [`STDERR_TAIL`] bytes the child wrote to standard error.
    tail: Arc<Mutex<Vec<u8>>>,
    binary: PathBuf,
    catalog: PathBuf,
    stopped: bool,
}

impl HeadlessChild {
    /// Start `binary` over `catalog` and the threads reading its output. A child that cannot be
    /// started is [`HEADLESS_FAILED`].
    fn start(binary: &Path, catalog: &Path) -> Result<Self, ApiFailure> {
        let mut child = Command::new(binary)
            .arg("--catalog")
            .arg(catalog)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                failure(
                    HEADLESS_FAILED,
                    format!(
                        "cannot start the headless owner {} for {}: {error}",
                        binary.display(),
                        catalog.display()
                    ),
                    Some(json!({
                        "catalog": catalog,
                        "binary": binary,
                        "status": null,
                        "stderr": "",
                    })),
                )
            })?;
        eprintln!(
            "luxforge-mcp: started luxforge-json (process {}) for {}",
            child.id(),
            catalog.display()
        );
        let stdin = child.stdin.take().expect("a piped standard input");
        let stdout = child.stdout.take().expect("a piped standard output");
        let stderr = child.stderr.take().expect("a piped standard error");
        let tail = Arc::new(Mutex::new(Vec::new()));
        let stderr = {
            let tail = tail.clone();
            std::thread::Builder::new()
                .name("luxforge-mcp-headless-stderr".into())
                .spawn(move || forward(stderr, &tail))
                .expect("the headless owner's standard-error thread starts")
        };
        // One answer waits at most while the next is read: the owner answers one request at a
        // time, so at most one is ever outstanding.
        let (send, answers) = sync_channel(1);
        let stdout = std::thread::Builder::new()
            .name("luxforge-mcp-headless-stdout".into())
            .spawn(move || read_answers(stdout, &send))
            .expect("the headless owner's output thread starts");
        Ok(Self {
            child,
            stdin: Some(stdin),
            answers,
            stdout: Some(stdout),
            stderr: Some(stderr),
            tail,
            binary: binary.to_path_buf(),
            catalog: catalog.to_path_buf(),
            stopped: false,
        })
    }

    /// `schema.list`, naming this protocol, then `session.identify {label}`, both within
    /// [`HANDSHAKE_TIMEOUT`]: the registry the child serves.
    fn handshake(&mut self, label: &str) -> Result<Value, ApiFailure> {
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let schema = self.exchange("schema.list", json!({}), Some(deadline))?;
        if schema["protocol"] != PROTOCOL {
            return Err(failure(
                UNSUPPORTED_PROTOCOL,
                format!(
                    "the headless owner speaks {}; this luxforge-mcp speaks {PROTOCOL}",
                    schema["protocol"]
                ),
                None,
            ));
        }
        self.exchange("session.identify", json!({"label": label}), Some(deadline))?;
        Ok(schema)
    }

    /// Send one request and read its answer, by `deadline` when there is one. A child that ends,
    /// or whose input is closed, once the request is on its way is [`CONNECTION_LOST`].
    fn exchange(
        &mut self,
        method: &str,
        params: Value,
        deadline: Option<Instant>,
    ) -> Result<Value, ApiFailure> {
        let (id, line) = request_line(method, params, None)?;
        let lost = |detail: &str| {
            failure(
                CONNECTION_LOST,
                format!("the headless luxforge-json failed during {method}: {detail}"),
                None,
            )
        };
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| lost("its input is closed"))?;
        stdin
            .write_all(&line)
            .and_then(|()| stdin.flush())
            .map_err(|error| lost(&error.to_string()))?;
        let answer = match deadline {
            None => self.answers.recv().map_err(|_| lost("it ended"))?,
            Some(deadline) => match self
                .answers
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(answer) => answer,
                Err(RecvTimeoutError::Disconnected) => return Err(lost("it ended")),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(lost(&format!(
                        "it did not answer within {} s of starting",
                        HANDSHAKE_TIMEOUT.as_secs()
                    )));
                }
            },
        };
        match answer {
            Answer::TooLong => Err(too_long(method)),
            Answer::Line(line) => read_answer(method, &id, &line),
        }
    }

    /// Stop the child after a failed handshake and report it, with what it wrote to standard
    /// error, as [`HEADLESS_FAILED`].
    fn failed(mut self, cause: &ApiFailure) -> ApiFailure {
        let status = self.stop();
        let stderr = String::from_utf8_lossy(&self.tail.lock().expect("the tail")).into_owned();
        let stderr = stderr.trim().to_owned();
        let mut message = format!(
            "the headless owner {} could not serve {}: {}",
            self.binary.display(),
            self.catalog.display(),
            cause.message
        );
        if !stderr.is_empty() {
            message.push_str("; it wrote: ");
            message.push_str(&stderr);
        }
        failure(
            HEADLESS_FAILED,
            message,
            Some(json!({
                "catalog": &self.catalog,
                "binary": &self.binary,
                "status": status.map(|status| status.to_string()),
                "stderr": stderr,
                "cause": {"code": cause.code, "message": cause.message},
            })),
        )
    }

    /// Close the child's input, wait up to [`SHUTDOWN_GRACE`] for it to end, kill it if it has not,
    /// and wait for it and both its threads. Answers how it ended; once only.
    fn stop(&mut self) -> Option<ExitStatus> {
        if self.stopped {
            return None;
        }
        self.stopped = true;
        drop(self.stdin.take());
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        loop {
            match self
                .answers
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                // An answer no call waits for.
                Ok(_) => {}
                // Its output closed: it has ended.
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    eprintln!(
                        "luxforge-mcp: luxforge-json (process {}) did not end within {} s of its \
                         input closing; stopping it",
                        self.child.id(),
                        SHUTDOWN_GRACE.as_secs()
                    );
                    let _ = self.child.kill();
                    break;
                }
            }
        }
        let status = self.child.wait().ok();
        for thread in [self.stdout.take(), self.stderr.take()]
            .into_iter()
            .flatten()
        {
            let _ = thread.join();
        }
        if let Some(status) = status {
            eprintln!(
                "luxforge-mcp: luxforge-json (process {}) ended: {status}",
                self.child.id()
            );
        }
        status
    }
}

impl Connection for HeadlessChild {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, ApiFailure> {
        self.exchange(method, params, None)
    }

    /// Whether the child has ended: its output thread has finished. An answer that arrives with
    /// no call waiting means the exchange is out of step, which ends the connection too.
    fn closed(&self) -> bool {
        !matches!(self.answers.try_recv(), Err(TryRecvError::Empty))
    }
}

impl Drop for HeadlessChild {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Read the child's answers, one line each, until its output ends. A line longer than
/// [`RESPONSE_LIMIT`] is passed on as [`Answer::TooLong`] and the rest of it skipped, so the next
/// answer is read whole.
fn read_answers(stdout: ChildStdout, answers: &SyncSender<Answer>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = Vec::new();
        match reader
            .by_ref()
            .take(RESPONSE_LIMIT as u64 + 1)
            .read_until(b'\n', &mut line)
        {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let answer = if line.len() > RESPONSE_LIMIT {
            if reader.skip_until(b'\n').is_err() {
                return;
            }
            Answer::TooLong
        } else if line.last() == Some(&b'\n') {
            Answer::Line(line)
        } else {
            // The output ended mid-answer.
            return;
        };
        if answers.send(answer).is_err() {
            return;
        }
    }
}

/// Forward the child's standard error to the adapter's, line by line, keeping its tail.
fn forward(stderr: ChildStderr, tail: &Mutex<Vec<u8>>) {
    let mut reader = BufReader::new(stderr);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader
            .by_ref()
            .take(STDERR_LINE as u64)
            .read_until(b'\n', &mut line)
        {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&line);
        eprintln!("luxforge-json: {}", text.trim_end());
        let mut tail = tail.lock().expect("the tail");
        tail.extend_from_slice(&line);
        let excess = tail.len().saturating_sub(STDERR_TAIL);
        tail.drain(..excess);
    }
}

#[cfg(test)]
mod tests;
