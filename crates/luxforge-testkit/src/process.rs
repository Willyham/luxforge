//! [`JsonProcess`], one `luxforge-json` process driven over its standard input and output the way an
//! agent drives it: each call writes one request line and reads its answer, so a test can wait on
//! jobs between requests. A test names the binary (`env!("CARGO_BIN_EXE_luxforge-json")` is only
//! known to the crate that builds it) and the arguments.
use luxforge_testbase::{HANG, wait_for};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
};

/// One running `luxforge-json` process. Dropping it without [`JsonProcess::finish`] kills it.
pub struct JsonProcess {
    child: Option<Child>,
    input: Option<ChildStdin>,
    output: mpsc::Receiver<std::io::Result<String>>,
    stderr: Option<JoinHandle<Vec<u8>>>,
    tag: String,
    next: u64,
    transcript: Vec<String>,
}

impl JsonProcess {
    /// Start `binary` with `args`. `tag` prefixes every request identity, so a transcript shows
    /// which test wrote it.
    pub fn start(binary: impl AsRef<Path>, args: &[&str], tag: &str) -> Self {
        let mut child = Command::new(binary.as_ref())
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("{} did not start: {error}", binary.as_ref().display()));
        let input = child.stdin.take().expect("a piped standard input");
        let stdout = child.stdout.take().expect("a piped standard output");
        let mut stderr = child.stderr.take().expect("a piped standard error");
        let (sender, output) = mpsc::sync_channel(16);
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        // Drained on its own thread, so a process that logs a lot never blocks on a full pipe.
        let stderr = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            bytes
        });
        Self {
            child: Some(child),
            input: Some(input),
            output,
            stderr: Some(stderr),
            tag: tag.to_owned(),
            next: 0,
            transcript: Vec::new(),
        }
    }

    /// One request, answering the whole response line, error or not. The answer must carry the
    /// request's own identity.
    pub fn call_raw(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = format!("{}-{}", self.tag, self.next);
        let input = self.input.as_mut().expect("the process is running");
        writeln!(
            input,
            "{}",
            json!({"id": id, "method": method, "params": params})
        )
        .expect("write a request");
        input.flush().expect("flush a request");
        // Every request is answered at once; long work is a job the test polls. The hang bound
        // only stops a process that never answers from hanging the test.
        let line = self
            .output
            .recv_timeout(HANG)
            .unwrap_or_else(|_| panic!("no answer to {method} within the {HANG:?} hang bound"))
            .expect("read an answer");
        self.transcript.push(line.clone());
        let response: Value = serde_json::from_str(&line).expect("a JSON answer");
        assert_eq!(
            response["id"],
            json!(id),
            "the answer to {method} names its request"
        );
        response
    }

    /// One request that must succeed, answering its result.
    pub fn call(&mut self, method: &str, params: Value) -> Value {
        let response = self.call_raw(method, params);
        assert!(response.get("error").is_none(), "{method}: {response}");
        response["result"].clone()
    }

    /// One request that must be refused, answering its error.
    pub fn error(&mut self, method: &str, params: Value) -> Value {
        let response = self.call_raw(method, params);
        assert!(response.get("result").is_none(), "{method}: {response}");
        response["error"].clone()
    }

    /// Poll a job with `method` (`job.read`, for a job of any kind) until it leaves the queue,
    /// answering its last status. The client polls; the owner never does.
    pub fn settle(&mut self, method: &str, job_id: &Value) -> Value {
        wait_for(&format!("{method} {job_id} settling"), || {
            let status = self.call(method, json!({"job_id": job_id}));
            (!matches!(status["status"].as_str(), Some("queued" | "running"))).then_some(status)
        })
    }

    /// Open the file at `path` as a client does: develop it at once (`pick.develop` of its path
    /// into the plan's folder, confirming removable media, since the person chose it), prepare the
    /// photograph the Develop answers (`source.prepare`) and adopt it as this client's current
    /// photograph (`job.adopt`), answering the adopted photograph's state. `actor` names the
    /// Develop.
    pub fn open(&mut self, path: &Path, actor: &str) -> Value {
        self.next += 1;
        let request_id = format!("{}-open-{}", self.tag, self.next);
        let started = self.call(
            "pick.develop",
            json!({
                "targets": {"kind": "paths", "paths": [path]},
                "into": [],
                "confirm_removable": true,
                "mutation": {"request_id": request_id, "actor": actor},
            }),
        );
        let developed = self.settle("job.read", &started["job_id"]);
        assert_eq!(developed["status"], "ready", "the Develop: {developed}");
        let report = &developed["result"];
        assert_eq!(report["failed"], json!([]), "the Develop: {report}");
        let asset = report["developed"][0]["asset_id"].clone();
        let prepared = self.call("source.prepare", json!({"asset_id": asset}));
        let ready = self.settle("job.read", &prepared["job_id"]);
        assert_eq!(ready["status"], "ready", "the preparation: {ready}");
        let adopted = self.call("job.adopt", json!({"job_id": prepared["job_id"]}));
        adopted["asset"].clone()
    }

    /// Every answer line so far, in order.
    pub fn transcript(&self) -> String {
        self.transcript.concat()
    }

    /// Close standard input, wait for a clean exit and answer what the process wrote to standard
    /// error.
    pub fn finish(mut self) -> String {
        drop(self.input.take());
        // The child stays in `self` while it is awaited, so a process that never exits is killed
        // when the failed wait drops it.
        let child = self.child.as_mut().expect("the process");
        let status = wait_for("the process exiting at the end of its input", || {
            child.try_wait().expect("the process's exit status")
        });
        self.child = None;
        let stderr = self
            .stderr
            .take()
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        let stderr = String::from_utf8_lossy(&stderr).into_owned();
        assert!(
            status.success(),
            "the process exited with {status}: {stderr}"
        );
        stderr
    }
}

impl Drop for JsonProcess {
    fn drop(&mut self) {
        drop(self.input.take());
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
