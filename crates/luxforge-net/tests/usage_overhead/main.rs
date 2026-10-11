//! What the existing transport allocates to send one statistics batch, which the design keeps
//! outside the statistics' own 256 KiB bound and asks to be measured separately
//! (`docs/design/usage-statistics.md`, "Bounded collection and performance sampling"): the heap the
//! sending thread holds while a 64 KiB `POST` with the statistics limits goes to a loopback
//! receiver, over plain HTTP and over TLS, and what stays allocated afterwards (the TLS client
//! configuration the transport builds at its first request and keeps).
//!
//! Counted on the sending thread alone ([`allocations::thread_live_bytes`]), so the receiver's own
//! threads in this process are not. Not counted: memory the system allocates for itself outside
//! the Rust allocator (the platform certificate verifier's Security framework calls on macOS, the
//! kernel's socket buffers) and the sending thread's stack. The TLS figures use the test roots;
//! the production transport verifies through the platform verifier instead, whose Rust-side
//! allocations this does not include. The figures are printed; the assertions only hold them under
//! a generous ceiling, so a large regression is noticed.
use luxforge_core::{
    capabilities::{
        endpoint::{EndpointClass, parse_endpoint},
        transport::{Method, RedirectPolicy, SendOptions, Transport, TransportRequest},
    },
    jobs::JobControl,
    usage::sender,
};
use luxforge_net::{CertificateDer, HttpTransport, TlsTrust, TransportConfig};
use luxforge_process::allocations::{self, Counting};
use luxforge_testbase::{Request, TestServer, respond};
use rustls::{
    ServerConfig,
    pki_types::{PrivateKeyDer, pem::PemObject},
};
use std::{io, io::Write, path::PathBuf, sync::Arc};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const PATH: &str = "/v1/usage/batches";
/// A body as large as a statistics batch gets, within the receiver's own 64 KiB limit.
const BODY: usize = 65_000;
/// The ceiling the figures are held under: a regression guard, not a target.
const CEILING: i64 = 4 * 1024 * 1024;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tls")
        .join(name)
}

fn server_tls() -> Arc<ServerConfig> {
    let chain = vec![CertificateDer::from_pem_file(fixture("leaf.pem")).unwrap()];
    let key = PrivateKeyDer::from_pem_file(fixture("leaf.key")).unwrap();
    Arc::new(
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap(),
    )
}

fn accept(_: &Request, out: &mut dyn Write) {
    respond(out, "202 Accepted", "", b"");
}

/// What sending one batch held on this thread at its peak, and what it left allocated, in bytes.
fn measure(transport: &HttpTransport, url: &str) -> (i64, i64) {
    let request = TransportRequest {
        method: Method::Post,
        endpoint: parse_endpoint(url, &[EndpointClass::Loopback]).unwrap(),
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: vec![b' '; BODY],
    };
    let job = JobControl::new();
    let before = allocations::thread_live_bytes();
    allocations::reset_peak();
    let response = transport
        .send(
            &request,
            SendOptions {
                max_request_bytes: sender::MAX_REQUEST_BYTES,
                max_response_bytes: sender::MAX_RESPONSE_BYTES,
                connect_timeout: sender::CONNECT_TIMEOUT,
                read_timeout: sender::IDLE_TIMEOUT,
                total_timeout: sender::TOTAL_TIMEOUT,
                redirects: RedirectPolicy::default(),
                control: &job,
                progress: &mut |_, _| {},
            },
            &mut io::sink(),
        )
        .expect("the receiver answered");
    assert_eq!(response.status, 202);
    drop(response);
    (
        allocations::thread_peak_bytes() - before,
        allocations::thread_live_bytes() - before,
    )
}

#[test]
fn usage_the_transports_heap_for_one_batch_is_measured_apart_from_the_statistics() {
    let plain = TestServer::http(accept).unwrap();
    let secure = TestServer::https(server_tls(), accept).unwrap();

    let http = HttpTransport::new(TransportConfig {
        trust: TlsTrust::Roots(Vec::new()),
        ..TransportConfig::default()
    });
    let (http_first, http_left) = measure(&http, &plain.url(PATH));
    let (http_next, _) = measure(&http, &plain.url(PATH));

    let tls = HttpTransport::new(TransportConfig {
        trust: TlsTrust::Roots(vec![
            CertificateDer::from_pem_file(fixture("ca.pem")).unwrap(),
        ]),
        ..TransportConfig::default()
    });
    let (tls_first, tls_kept) = measure(&tls, &secure.url(PATH));
    let (tls_next, tls_next_left) = measure(&tls, &secure.url(PATH));

    println!(
        "transport heap on the sending thread for one {BODY}-byte statistics POST, bytes: plain \
         HTTP peak {http_first} first and {http_next} after, {http_left} left; TLS (test roots) \
         peak {tls_first} on the first request, which builds and keeps the client configuration \
         ({tls_kept} left), and {tls_next} on each later one ({tls_next_left} left)"
    );
    for (what, bytes) in [
        ("plain HTTP", http_first.max(http_next)),
        ("TLS", tls_first.max(tls_next)),
    ] {
        assert!(bytes > 0, "{what}: nothing counted");
        assert!(bytes <= CEILING, "{what}: {bytes} bytes");
    }
    assert!(tls_kept <= CEILING);
    assert!(tls_next_left <= 1024, "a later request left {tls_next_left} bytes");
}
