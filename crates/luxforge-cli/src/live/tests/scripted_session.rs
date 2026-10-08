//! A scripted live session for these tests: a loopback listener that writes the session file beside
//! a scratch catalog, accepts one connection and answers each request line through a script,
//! recording every request it read, so a test controls exactly what the client is told. The
//! handshake's `schema.list` is answered with the built-in registry's own schema unless a test
//! scripts it ([`Endpoint::serve_all`]).
use luxforge_core::{LocalSessionInfo, ModuleRegistry, PROTOCOL, live_session_file};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::PathBuf,
    sync::OnceLock,
    thread::JoinHandle,
};

pub(super) const TOKEN: &str = "endpoint-token";

/// `schema.list` as an ordinary run's registry publishes it, built once: the session the client
/// is tested against lists exactly the methods, envelopes and markers the real one does.
pub(super) fn schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| luxforge_core::schemas(&ModuleRegistry::builtin()))
}

/// What the script answers a request with.
pub(super) enum Reply {
    /// A result for the request.
    Result(Value),
    /// An API failure, `{code, message, ...}`, for the request.
    Error(Value),
    /// This exact line, without its newline.
    Raw(Vec<u8>),
    /// Close the connection without answering.
    Close,
    /// Read nothing more and answer nothing, holding the connection open until the client leaves.
    Silence,
}

pub(super) struct Endpoint {
    pub catalog: PathBuf,
    join: Option<JoinHandle<Vec<Value>>>,
}

impl Endpoint {
    /// Serve one connection, answering its handshake with [`schema`] and each request after it,
    /// `n` from 0, with `script(n, request)`.
    pub fn serve(mut script: impl FnMut(usize, &Value) -> Reply + Send + 'static) -> Self {
        Self::serve_all(move |n, request| match n {
            0 => {
                assert_eq!(
                    request["method"], "schema.list",
                    "the handshake comes first"
                );
                Reply::Result(schema().clone())
            }
            _ => script(n - 1, request),
        })
    }

    /// Serve one connection, answering every request, the handshake's included, `n` from 0, with
    /// `script(n, request)`.
    pub fn serve_all(mut script: impl FnMut(usize, &Value) -> Reply + Send + 'static) -> Self {
        let catalog = luxforge_testbase::paths::temp_catalog("live-endpoint");
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let info = LocalSessionInfo {
            protocol: PROTOCOL.into(),
            address: listener.local_addr().unwrap(),
            token: TOKEN.into(),
        };
        std::fs::write(
            live_session_file(&catalog),
            serde_json::to_vec(&info).unwrap(),
        )
        .unwrap();
        let join = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut requests = Vec::new();
            let mut lines = BufReader::new(stream).lines();
            while let Some(Ok(line)) = lines.next() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let reply = script(requests.len(), &request);
                let id = request["id"].clone();
                requests.push(request);
                let mut line = match reply {
                    Reply::Result(result) => {
                        serde_json::to_vec(&json!({"id": id, "sequence": 0, "result": result}))
                            .unwrap()
                    }
                    Reply::Error(error) => {
                        serde_json::to_vec(&json!({"id": id, "sequence": 0, "error": error}))
                            .unwrap()
                    }
                    Reply::Raw(line) => line,
                    Reply::Close => break,
                    Reply::Silence => {
                        // Held until the client closes its end.
                        for _ in lines.by_ref() {}
                        break;
                    }
                };
                line.push(b'\n');
                if writer.write_all(&line).is_err() {
                    break;
                }
            }
            requests
        });
        Self {
            catalog,
            join: Some(join),
        }
    }

    /// Every request the endpoint read, the handshake's included, once the client has gone.
    pub fn requests(mut self) -> Vec<Value> {
        self.join.take().unwrap().join().unwrap()
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(live_session_file(&self.catalog));
    }
}
