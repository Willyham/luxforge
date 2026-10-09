//! Forwarding accepted rows once to the private collector.
//!
//! A bounded in-memory queue (batches and bytes) feeds one worker. A full queue refuses the batch
//! at admission, so the client sees `429` and nothing waits in memory. The worker sends each body
//! one time: a failure drops that body and is counted, never retried and never spooled to disk.
//! The requests it makes carry only this module's own headers: nothing from the client's request
//! reaches the collector.

use crate::diag::{Counters, Diagnostics};
use crate::rows::Rows;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1::{self, SendRequest};
use hyper::header::{CONTENT_TYPE, HOST};
use hyper::{Method, Request, Uri};
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::timeout;

/// The collector's base URL, parsed. Only plain `http://` on a private network is supported.
#[derive(Debug, Clone)]
pub struct Collector {
    authority: String,
    base_path: String,
}

impl Collector {
    pub fn parse(url: &str) -> Result<Collector, &'static str> {
        let uri: Uri = url.parse().map_err(|_| "collector URL is not a URL")?;
        if uri.scheme_str() != Some("http") {
            return Err("collector URL must be http:// on a private network");
        }
        if uri.query().is_some() {
            return Err("collector URL must not have a query");
        }
        let authority = uri.authority().ok_or("collector URL has no host")?;
        let authority = if authority.port().is_some() {
            authority.as_str().to_owned()
        } else {
            format!("{}:80", authority.as_str())
        };
        Ok(Collector {
            authority,
            base_path: uri.path().trim_end_matches('/').to_owned(),
        })
    }

    fn events_path(&self) -> String {
        format!("{}/events", self.base_path)
    }

    fn profiles_path(&self) -> String {
        format!("{}/profiles", self.base_path)
    }
}

/// The admission side of the queue.
pub struct Queue {
    sender: mpsc::Sender<Rows>,
    bytes: Arc<AtomicUsize>,
    max_bytes: usize,
}

impl Queue {
    /// Reserve room and enqueue, or hand the rows back unqueued.
    pub fn try_enqueue(&self, rows: Rows) -> Result<(), Rows> {
        let size = rows.len();
        if self.bytes.fetch_add(size, SeqCst).saturating_add(size) > self.max_bytes {
            self.bytes.fetch_sub(size, SeqCst);
            return Err(rows);
        }
        match self.sender.try_send(rows) {
            Ok(()) => Ok(()),
            Err(
                mpsc::error::TrySendError::Full(rows) | mpsc::error::TrySendError::Closed(rows),
            ) => {
                self.bytes.fetch_sub(size, SeqCst);
                Err(rows)
            }
        }
    }

    /// Bytes queued or in flight.
    pub fn queued_bytes(&self) -> usize {
        self.bytes.load(SeqCst)
    }
}

/// Start the worker. It ends when every [`Queue`] handle is gone.
pub fn spawn(
    collector: Collector,
    max_batches: usize,
    max_bytes: usize,
    request_timeout: Duration,
    diag: Arc<Diagnostics>,
) -> (Queue, tokio::task::JoinHandle<()>) {
    let (sender, mut receiver) = mpsc::channel::<Rows>(max_batches.max(1));
    let bytes = Arc::new(AtomicUsize::new(0));
    let queue = Queue {
        sender,
        bytes: bytes.clone(),
        max_bytes,
    };
    let handle = tokio::spawn(async move {
        let mut downstream = Downstream {
            collector,
            connection: None,
            request_timeout,
        };
        while let Some(rows) = receiver.recv().await {
            let size = rows.len();
            let events = downstream
                .send(&downstream.collector.events_path(), rows.events)
                .await;
            let profile = downstream
                .send(&downstream.collector.profiles_path(), rows.profile)
                .await;
            for ok in [events, profile] {
                Counters::bump(if ok {
                    &diag.counters.forwarded
                } else {
                    &diag.counters.forward_failed
                });
            }
            bytes.fetch_sub(size, SeqCst);
        }
    });
    (queue, handle)
}

struct Downstream {
    collector: Collector,
    connection: Option<SendRequest<Full<Bytes>>>,
    request_timeout: Duration,
}

impl Downstream {
    async fn connect(&self) -> Option<SendRequest<Full<Bytes>>> {
        let stream = timeout(
            self.request_timeout,
            TcpStream::connect(&self.collector.authority),
        )
        .await
        .ok()?
        .ok()?;
        let (sender, connection) = http1::handshake(TokioIo::new(stream)).await.ok()?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Some(sender)
    }

    /// One attempt to POST `body` to `path`. True when the collector answered with a success.
    async fn send(&mut self, path: &str, body: Vec<u8>) -> bool {
        if body.is_empty() {
            return true;
        }
        if self.connection.as_ref().is_none_or(SendRequest::is_closed) {
            self.connection = self.connect().await;
        }
        let Some(mut sender) = self.connection.take() else {
            return false;
        };
        let request = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(HOST, &self.collector.authority)
            .header(CONTENT_TYPE, "application/x-ndjson")
            .body(Full::new(Bytes::from(body)));
        let Ok(request) = request else { return false };
        let exchange = async {
            sender.ready().await.ok()?;
            let response = sender.send_request(request).await.ok()?;
            let success = response.status().is_success();
            // Drain a bounded reply so the connection can be reused; a larger one ends the
            // connection instead.
            let mut body = response.into_body();
            let mut seen = 0usize;
            while let Some(frame) = body.frame().await {
                seen += frame.ok()?.data_ref().map_or(0, Bytes::len);
                if seen > 64 * 1024 {
                    return None;
                }
            }
            Some((success, sender))
        };
        match timeout(self.request_timeout, exchange).await {
            Ok(Some((success, sender))) => {
                self.connection = Some(sender);
                success
            }
            _ => false,
        }
    }
}
