//! The HTTP front: one route, `POST /v1/usage/batches`, and nothing else.
//!
//! Order of refusal, cheapest first: path (`404`), method (`405`), content type (`415`), a declared
//! length over the cap (`413`), global admission (`429`), the body read under the cap (`413`), the
//! strict decode (`400`), the optional per-ID limit (`429`), the forwarding queue (`429`). The peer
//! address is never read: the accept loop discards it, and no function here is handed a socket
//! after that. No request header is read except `Content-Type` and `Content-Length`, and none is
//! passed on.

use crate::config::Config;
use crate::diag::{Counters, Diagnostics, Sink};
use crate::forward::{self, Collector, Queue};
use crate::limits::{Clock, Global, IdLimiter, IdVerdict};
use crate::registry::Registry;
use crate::rows;
use crate::wire::{self, MAX_BODY};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{ALLOW, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

/// The only route.
pub const ROUTE: &str = "/v1/usage/batches";

/// How many refusals may be answered at once, and for how long each is held.
const MAX_REFUSALS: usize = 64;
const REFUSAL_TIME: Duration = Duration::from_millis(250);

/// The canned reply to a connection refused for lack of capacity.
const BUSY: &[u8] = b"HTTP/1.1 429 Too Many Requests\r\nconnection: close\r\ncontent-length: 17\r\n\r\ntoo many requests";

struct Shared {
    global: Global,
    ids: Option<IdLimiter>,
    queue: Queue,
    diag: Arc<Diagnostics>,
    clock: Arc<dyn Clock>,
    retry_after_secs: u64,
}

/// A running service. Dropping it stops the listener; call [`Server::shutdown`] to wait for it.
pub struct Server {
    addr: SocketAddr,
    diag: Arc<Diagnostics>,
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl Server {
    pub async fn start(
        config: Config,
        clock: Arc<dyn Clock>,
        sink: Sink,
    ) -> Result<Server, String> {
        Registry::get();
        let collector = Collector::parse(&config.collector_url).map_err(str::to_owned)?;
        let listener = TcpListener::bind(config.listen)
            .await
            .map_err(|_| "cannot listen on the configured address".to_owned())?;
        let addr = listener
            .local_addr()
            .map_err(|_| "no local address".to_owned())?;
        let diag = Arc::new(Diagnostics::new(sink));
        let (queue, forwarder) = forward::spawn(
            collector,
            config.queue_batches,
            config.queue_bytes,
            config.forward_timeout,
            diag.clone(),
        );
        let now = clock.now_ms();
        let shared = Arc::new(Shared {
            global: Global::new(config.global_per_second, config.global_burst, now),
            ids: (config.id_interval_secs > 0).then(|| {
                IdLimiter::new(
                    config.id_interval_secs,
                    u64::from(config.id_burst),
                    config.id_ttl_secs,
                    config.id_max_entries,
                )
            }),
            queue,
            diag: diag.clone(),
            clock,
            retry_after_secs: config.retry_after_secs,
        });
        let (stop, stopped) = watch::channel(false);
        let accept = tokio::spawn(accept_loop(
            listener,
            shared.clone(),
            config.max_connections.max(1),
            config.request_timeout,
            stopped.clone(),
        ));
        let stats = tokio::spawn(stats_loop(shared, config.stats_interval, stopped));
        diag.note("usage-ingest listening");
        Ok(Server {
            addr,
            diag,
            stop,
            tasks: vec![accept, stats, forwarder],
        })
    }

    /// The listener's own address (not a peer's).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn diagnostics(&self) -> Arc<Diagnostics> {
        self.diag.clone()
    }

    /// Stop accepting, end open connections and drop whatever is still queued.
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        for task in &self.tasks[..2] {
            while !task.is_finished() {
                sleep(Duration::from_millis(5)).await;
            }
        }
        self.tasks[2].abort();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.tasks[2].abort();
    }
}

async fn stats_loop(shared: Arc<Shared>, every: Duration, mut stopped: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = sleep(every) => {
                shared.diag.emit();
                if let Some(ids) = &shared.ids {
                    ids.sweep(shared.clock.now_ms());
                }
            }
            _ = stopped.changed() => return,
        }
    }
}

async fn accept_loop(
    listener: TcpListener,
    shared: Arc<Shared>,
    max_connections: usize,
    lifetime: Duration,
    mut stopped: watch::Receiver<bool>,
) {
    let capacity = Arc::new(Semaphore::new(max_connections));
    let refusals = Arc::new(Semaphore::new(MAX_REFUSALS));
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = stopped.changed() => return,
        };
        // The peer's address is dropped here, unread.
        let Ok((stream, _)) = accepted else {
            sleep(Duration::from_millis(10)).await;
            continue;
        };
        let Ok(permit) = capacity.clone().try_acquire_owned() else {
            Counters::bump(&shared.diag.counters.connections_refused);
            // Past the cap, answer briefly if a small second budget allows, else just close.
            if let Ok(turn) = refusals.clone().try_acquire_owned() {
                tokio::spawn(refuse(stream, turn));
            }
            continue;
        };
        let shared = shared.clone();
        let mut stopped = stopped.clone();
        tokio::spawn(async move {
            let service = service_fn({
                let shared = shared.clone();
                move |request| {
                    let shared = shared.clone();
                    async move { Ok::<_, Infallible>(route(&shared, request).await) }
                }
            });
            let connection = http1::Builder::new()
                .keep_alive(false)
                .max_buf_size(16 * 1024)
                .serve_connection(TokioIo::new(stream), service);
            tokio::select! {
                finished = timeout(lifetime, connection) => {
                    if finished.is_err() {
                        Counters::bump(&shared.diag.counters.connections_timed_out);
                    }
                }
                _ = stopped.changed() => {}
            }
            drop(permit);
        });
    }
}

/// Say `429`, close our side, and read what the client sent for a moment: closing a socket with
/// unread data resets the connection, which can discard the reply on the client.
async fn refuse(mut stream: TcpStream, _turn: OwnedSemaphorePermit) {
    let _ = timeout(REFUSAL_TIME, async {
        stream.write_all(BUSY).await.ok()?;
        stream.shutdown().await.ok()?;
        let mut sink = [0u8; 1024];
        while stream.read(&mut sink).await.ok()? > 0 {}
        Some(())
    })
    .await;
}

fn reply(
    status: StatusCode,
    word: &'static str,
    retry_after: Option<u64>,
) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from_static(word.as_bytes())));
    *response.status_mut() = status;
    if let Some(seconds) = retry_after {
        response.headers_mut().insert(RETRY_AFTER, seconds.into());
    }
    response
}

fn accepted() -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = StatusCode::ACCEPTED;
    response
}

/// `application/json`, optionally with `; charset=utf-8`, and exactly one such header.
fn json_content_type(request: &Request<Incoming>) -> bool {
    let mut values = request.headers().get_all(CONTENT_TYPE).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let compact: String = value.chars().filter(|c| *c != ' ' && *c != '\t').collect();
    matches!(
        compact.to_ascii_lowercase().as_str(),
        "application/json" | "application/json;charset=utf-8"
    )
}

/// Read a body, stopping as soon as it passes `limit` bytes. `None` means it did.
async fn read_capped(body: Incoming, limit: usize) -> Result<Option<Vec<u8>>, ()> {
    let mut body = body;
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| ())?;
        if let Some(data) = frame.data_ref() {
            if bytes.len() + data.len() > limit {
                return Ok(None);
            }
            bytes.extend_from_slice(data);
        }
    }
    Ok(Some(bytes))
}

async fn route(shared: &Shared, request: Request<Incoming>) -> Response<Full<Bytes>> {
    let counters = &shared.diag.counters;
    if request.uri().path() != ROUTE || request.uri().query().is_some() {
        Counters::bump(&counters.not_found);
        return reply(StatusCode::NOT_FOUND, "not found", None);
    }
    if request.method() != Method::POST {
        Counters::bump(&counters.method_not_allowed);
        let mut response = reply(StatusCode::METHOD_NOT_ALLOWED, "method not allowed", None);
        response
            .headers_mut()
            .insert(ALLOW, "POST".parse().expect("static header value"));
        return response;
    }
    if !json_content_type(&request) {
        Counters::bump(&counters.unsupported_media_type);
        return reply(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported media type",
            None,
        );
    }
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|length| length > MAX_BODY as u64) {
        Counters::bump(&counters.payload_too_large);
        return reply(StatusCode::PAYLOAD_TOO_LARGE, "payload too large", None);
    }
    if let Err(retry) = shared.global.admit(shared.clock.now_ms()) {
        Counters::bump(&counters.limited_global);
        return reply(
            StatusCode::TOO_MANY_REQUESTS,
            "too many requests",
            Some(retry),
        );
    }
    let body = match read_capped(request.into_body(), MAX_BODY).await {
        Ok(Some(body)) => body,
        Ok(None) => {
            Counters::bump(&counters.payload_too_large);
            return reply(StatusCode::PAYLOAD_TOO_LARGE, "payload too large", None);
        }
        Err(()) => {
            Counters::bump(&counters.bad_request);
            return reply(StatusCode::BAD_REQUEST, "bad request", None);
        }
    };
    let Ok(batch) = wire::parse(&body) else {
        Counters::bump(&counters.bad_request);
        return reply(StatusCode::BAD_REQUEST, "bad request", None);
    };
    drop(body);
    if let Some(ids) = &shared.ids {
        match ids.admit(&batch.installation_id, shared.clock.now_ms()) {
            IdVerdict::Admit => {}
            IdVerdict::TableFull => Counters::bump(&counters.id_table_full),
            IdVerdict::Refuse(retry) => {
                Counters::bump(&counters.limited_id);
                return reply(
                    StatusCode::TOO_MANY_REQUESTS,
                    "too many requests",
                    Some(retry),
                );
            }
        }
    }
    let rows = rows::normalize(&batch, shared.clock.unix_secs());
    drop(batch);
    match shared.queue.try_enqueue(rows) {
        Ok(()) => {
            Counters::bump(&counters.accepted);
            accepted()
        }
        Err(_) => {
            Counters::bump(&counters.limited_queue);
            reply(
                StatusCode::TOO_MANY_REQUESTS,
                "too many requests",
                Some(shared.retry_after_secs),
            )
        }
    }
}
