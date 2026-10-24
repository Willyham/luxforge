//! The real `luxforge-mcp` binary as an MCP client sees it, with every message it writes checked
//! against the vendored `schema.json` of the era it answers in (`spec/`, with its source and
//! licence): a modern answer against 2026-07-28's response type for the request's method, a legacy
//! one against 2025-11-25's result type, an error against the era's error response, and each
//! against the era's `JSONRPCMessage` too.
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    ffi::OsStr,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        mpsc::{Receiver, RecvTimeoutError, channel},
    },
    thread::JoinHandle,
    time::Duration,
};

pub const BINARY: &str = env!("CARGO_BIN_EXE_luxforge-mcp");

/// How long a test waits for a message before it fails rather than hangs. Order never comes
/// from it: every wait is for a message the server must write.
const STALL: Duration = Duration::from_secs(120);

/// Which revision a request was sent in, and so which schema its answer is held to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Era {
    Modern,
    Legacy,
}

impl Era {
    fn file(self) -> &'static str {
        match self {
            Self::Modern => "2026-07-28.schema.json",
            Self::Legacy => "2025-11-25.schema.json",
        }
    }
}

/// The era's schema document.
fn document(era: Era) -> &'static Value {
    static MODERN: OnceLock<Value> = OnceLock::new();
    static LEGACY: OnceLock<Value> = OnceLock::new();
    let cell = match era {
        Era::Modern => &MODERN,
        Era::Legacy => &LEGACY,
    };
    cell.get_or_init(|| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/mcp/spec")
            .join(era.file());
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
    })
}

/// Compiled validators by era and definition name.
type Validators = HashMap<(Era, String), Arc<jsonschema::Validator>>;

/// The compiled validator of the era's definition `name`.
fn validator(era: Era, name: &str) -> Arc<jsonschema::Validator> {
    static VALIDATORS: OnceLock<Mutex<Validators>> = OnceLock::new();
    let mut validators = VALIDATORS.get_or_init(Default::default).lock().unwrap();
    validators
        .entry((era, name.to_owned()))
        .or_insert_with(|| {
            let document = document(era);
            assert!(
                document["$defs"].get(name).is_some(),
                "{era:?} defines no {name}"
            );
            let schema = json!({
                "$schema": document["$schema"],
                "$defs": document["$defs"],
                "$ref": format!("#/$defs/{name}"),
            });
            Arc::new(jsonschema::draft202012::new(&schema).unwrap())
        })
        .clone()
}

/// Hold `value` to the era's definition `name`.
pub fn conforms(era: Era, name: &str, value: &Value) {
    let validator = validator(era, name);
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|error| format!("{} at {}", error, error.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "not a {era:?} {name}: {errors:?}\n{value:#}"
    );
}

/// Hold a message the server wrote to the schema of the era of the request it answers (`asked`),
/// or, for a message that answers none, to both. A notification tagged with a subscription is a
/// modern one; an untagged one is a legacy client's, since a modern client hears only from its
/// subscriptions.
fn check(message: &Value, asked: Option<&(Era, String)>) {
    let tagged = message["params"]["_meta"]
        .get("io.modelcontextprotocol/subscriptionId")
        .is_some();
    let eras: Vec<Era> = match asked {
        Some((era, _)) => vec![*era],
        None if message.get("method").is_some() && tagged => vec![Era::Modern],
        None if message.get("method").is_some() => vec![Era::Legacy],
        None => vec![Era::Modern, Era::Legacy],
    };
    for era in eras {
        conforms(era, "JSONRPCMessage", message);
        if message.get("error").is_some() {
            conforms(era, "JSONRPCErrorResponse", message);
            if era == Era::Modern && message["error"]["code"] == -32022 {
                conforms(era, "UnsupportedProtocolVersionError", message);
            }
            continue;
        }
        if let Some(method) = message["method"].as_str() {
            let definition = match method {
                "notifications/subscriptions/acknowledged" => {
                    "SubscriptionsAcknowledgedNotification"
                }
                "notifications/tools/list_changed" => "ToolListChangedNotification",
                other => panic!("the server sent a notification it never should: {other}"),
            };
            conforms(era, definition, message);
            continue;
        }
        let method = asked.map(|(_, method)| method.as_str()).unwrap_or_default();
        match era {
            Era::Modern => {
                let definition = match method {
                    "server/discover" => "DiscoverResultResponse",
                    "tools/list" => "ListToolsResultResponse",
                    "tools/call" => "CallToolResultResponse",
                    other => panic!("no modern result is expected for {other}"),
                };
                conforms(era, definition, message);
                assert_eq!(message["result"]["resultType"], "complete", "{message}");
                assert_eq!(
                    message["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
                    "luxforge-mcp"
                );
            }
            Era::Legacy => {
                conforms(era, "JSONRPCResultResponse", message);
                let definition = match method {
                    "initialize" => "InitializeResult",
                    "tools/list" => "ListToolsResult",
                    "tools/call" => "CallToolResult",
                    "ping" => "EmptyResult",
                    other => panic!("no legacy result is expected for {other}"),
                };
                conforms(era, definition, &message["result"]);
                assert!(
                    message["result"].get("resultType").is_none(),
                    "a legacy result carries no resultType: {message}"
                );
            }
        }
    }
}

/// What the client says it is on every modern request, and in `initialize`.
pub fn client_info() -> Value {
    json!({"name": "claude-code", "title": "Claude Code", "version": "2.0.0"})
}

/// A running `luxforge-mcp`, driven over its standard streams.
pub struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    reader: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<String>>,
    /// Each line the server writes to standard error, as it arrives.
    diagnostics: Receiver<String>,
    /// Each request sent and not yet answered, by identity: its era and method.
    asked: HashMap<String, (Era, String)>,
    next: i64,
    /// Every message read, for a test to look back at.
    pub read: Vec<Value>,
}

impl Mcp {
    /// Start the binary with `args`, as `prepare` adjusts its command.
    pub fn start(args: &[&OsStr], prepare: impl FnOnce(&mut Command)) -> Self {
        let mut command = Command::new(BINARY);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        prepare(&mut command);
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let (send, lines) = channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if send.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        // Each diagnostic is echoed as it arrives, so a failing test shows what the server said,
        // and passed on for a test that waits for one.
        let (diagnose, diagnostics) = channel();
        let stderr = std::thread::spawn(move || {
            let mut text = String::new();
            for line in BufReader::new(&mut stderr).lines() {
                let line = line.unwrap();
                eprintln!("[luxforge-mcp] {line}");
                text.push_str(&line);
                text.push('\n');
                let _ = diagnose.send(line);
            }
            text
        });
        Self {
            stdin: child.stdin.take(),
            child,
            lines,
            reader: Some(reader),
            stderr: Some(stderr),
            diagnostics,
            asked: HashMap::new(),
            next: 0,
            read: Vec::new(),
        }
    }

    /// The next diagnostic line starting with `prefix`, skipping the others: one the server must
    /// write.
    pub fn diagnostic(&mut self, prefix: &str) -> String {
        loop {
            match self.diagnostics.recv_timeout(STALL) {
                Ok(line) if line.starts_with(prefix) => return line,
                Ok(_) => {}
                Err(error) => panic!("luxforge-mcp wrote no diagnostic {prefix:?}: {error}"),
            }
        }
    }

    /// Write one line as it is.
    pub fn send_line(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    /// Send a request in `era`, a modern one with its `_meta`, and answer its identity.
    pub fn request(&mut self, era: Era, method: &str, mut params: Value) -> Value {
        if era == Era::Modern {
            params["_meta"] = json!({
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
                "io.modelcontextprotocol/clientInfo": client_info(),
            });
        }
        self.next += 1;
        let id = json!(self.next);
        self.send_request(era, &id, method, params);
        id
    }

    /// Send a request under `id` exactly as given.
    pub fn send_request(&mut self, era: Era, id: &Value, method: &str, params: Value) {
        self.asked.insert(id.to_string(), (era, method.to_owned()));
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.send_line(&message.to_string());
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.send_line(&message.to_string());
    }

    /// The next message the server writes, held to its era's schema.
    pub fn next_message(&mut self) -> Value {
        let line = match self.lines.recv_timeout(STALL) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => panic!("luxforge-mcp wrote nothing for {STALL:?}"),
            Err(RecvTimeoutError::Disconnected) => panic!("luxforge-mcp closed its output"),
        };
        let message: Value = serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("not JSON on standard output ({error}): {line}"));
        let asked = message
            .get("id")
            .and_then(|id| self.asked.remove(&id.to_string()));
        check(&message, asked.as_ref());
        self.read.push(message.clone());
        message
    }

    /// The next message, which must answer `id`.
    pub fn answer(&mut self, id: &Value) -> Value {
        let message = self.next_message();
        assert_eq!(&message["id"], id, "{message}");
        message
    }

    /// Send a request and read its answer.
    pub fn ask(&mut self, era: Era, method: &str, params: Value) -> Value {
        let id = self.request(era, method, params);
        self.answer(&id)
    }

    /// Call `tool` and answer its `CallToolResult`.
    pub fn call(&mut self, era: Era, tool: &str, arguments: Value) -> Value {
        let answer = self.ask(
            era,
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
        );
        answer["result"].clone()
    }

    /// `luxforge_call` a method, answering its result, which must not be an error.
    pub fn luxforge(&mut self, era: Era, method: &str, params: Value) -> Value {
        let result = self.call(
            era,
            "luxforge_call",
            json!({"method": method, "params": params}),
        );
        assert_eq!(result["isError"], false, "{method}: {result}");
        result
    }

    /// `initialize` as a legacy client asking for `version`, then `notifications/initialized`.
    pub fn initialize(&mut self, version: &str) -> Value {
        let answer = self.ask(
            Era::Legacy,
            "initialize",
            json!({"protocolVersion": version, "capabilities": {}, "clientInfo": client_info()}),
        );
        self.notify("notifications/initialized", json!({}));
        answer
    }

    /// Close standard input and wait for the server to end: its exit status and standard error.
    /// Nothing more may be written.
    pub fn finish(mut self) -> (Option<i32>, String) {
        drop(self.stdin.take());
        let status = self.child.wait().unwrap();
        self.reader.take().unwrap().join().unwrap();
        let extra: Vec<String> = self.lines.try_iter().collect();
        assert!(extra.is_empty(), "written after the last answer: {extra:?}");
        let stderr = self.stderr.take().unwrap().join().unwrap();
        (status.code(), stderr)
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        if self.stdin.is_some() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// A tool result's structured content and its warnings' codes.
pub fn structured(result: &Value) -> &Value {
    &result["structuredContent"]
}

/// The codes of the warnings a tool result carries.
pub fn warnings(result: &Value) -> Vec<String> {
    result["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block["text"].as_str())
        .filter_map(|text| serde_json::from_str::<Value>(text).ok())
        .filter_map(|value| value.get("warnings").cloned())
        .flat_map(|warnings| warnings.as_array().unwrap().clone())
        .map(|warning| warning["code"].as_str().unwrap().to_owned())
        .collect()
}

/// The decoded pictures a tool result attaches.
pub fn images(result: &Value) -> Vec<Vec<u8>> {
    use base64::Engine;
    result["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["type"] == "image")
        .map(|block| {
            assert_eq!(block["mimeType"], "image/jpeg");
            base64::engine::general_purpose::STANDARD
                .decode(block["data"].as_str().unwrap())
                .unwrap()
        })
        .collect()
}
