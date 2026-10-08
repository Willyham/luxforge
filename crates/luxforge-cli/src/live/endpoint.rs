//! A scripted live session for this module's tests: a loopback listener that writes the session file
//! beside a scratch catalog, accepts one connection and answers each request line through a script,
//! recording every request it read, so a test controls exactly what the client is told.
use luxforge_core::{LocalSessionInfo, PROTOCOL, live_session_file};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::PathBuf,
    thread::JoinHandle,
};

pub(super) const TOKEN: &str = "endpoint-token";

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
}

pub(super) struct Endpoint {
    pub catalog: PathBuf,
    join: Option<JoinHandle<Vec<Value>>>,
}

impl Endpoint {
    /// Serve one connection, answering request `n` (from 0) with `script(n, request)`.
    pub fn serve(mut script: impl FnMut(usize, &Value) -> Reply + Send + 'static) -> Self {
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
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
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

    /// Every request the endpoint read, once the client has gone.
    pub fn requests(mut self) -> Vec<Value> {
        self.join.take().unwrap().join().unwrap()
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(live_session_file(&self.catalog));
    }
}
