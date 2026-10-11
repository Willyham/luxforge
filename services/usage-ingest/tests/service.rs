#![recursion_limit = "512"]
//! The running service against a fake downstream: rows, statuses, bounds under flood, and privacy.

mod support;

use serde_json::{Value, json};
use std::sync::atomic::Ordering::SeqCst;
use std::time::{Duration, Instant};
use support::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn lines(body: &[u8]) -> Vec<Value> {
    std::str::from_utf8(body)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_valid_batch_becomes_exactly_the_expected_rows() {
    let downstream = Downstream::start(Mode::Ok).await;
    let (server, _) = start(test_config(&downstream.url()), ManualClock::new()).await;
    let response = post_json(server.local_addr(), &batch()).await;
    assert_eq!(response.status, 202);
    assert_eq!(response.body, "");
    assert!(response.header("content-length").is_none_or(|v| v == "0"));

    let records = downstream.wait_for(2).await;
    assert_eq!(
        (records[0].path.as_str(), records[1].path.as_str()),
        ("/ingest/events", "/ingest/profiles")
    );
    let header = |r: &Received, name: &str| {
        r.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(
        header(&records[0], "content-type").as_deref(),
        Some("application/x-ndjson")
    );

    let common = |seq: u64, t: u64, name: &str| {
        json!({
            "received_at": "2026-10-09T14:03:12Z", "installation_id": INSTALLATION,
            "session_id": SESSION, "session_start": "2026-10-09T14:03:12Z", "batch_seq": 2,
            "seq": seq, "t": t, "name": name, "release": "0.0.0+9d4138ca1f2e",
            "trigger": null, "state": null, "workspace": null, "image": null, "feature": null,
            "tool": null, "count": null, "operation": null, "outcome": null, "duration_ms": null,
            "renderer": null, "cpu_percent": null, "memory_mib": null, "memory_kind": null,
            "gpu_percent": null, "gpu_memory_mib": null
        })
    };
    let with = |mut row: Value, fields: Value| {
        for (k, v) in fields.as_object().unwrap() {
            row[k] = v.clone();
        }
        row
    };
    let expected = vec![
        with(
            common(41, 1203400, "workspace.enter"),
            json!({"workspace": "develop"}),
        ),
        with(
            common(42, 1219050, "tool.commit"),
            json!({"tool": "basic", "image": IMAGE}),
        ),
        with(
            common(43, 1260000, "process.sample"),
            json!({"cpu_percent": 37, "memory_mib": 2140, "memory_kind": "footprint"}),
        ),
        with(
            common(44, 1284711, "operation.end"),
            json!({"operation": "export", "outcome": "completed", "duration_ms": 812,
                   "count": 1, "image": IMAGE}),
        ),
    ];
    assert_eq!(lines(&records[0].body), expected);

    let profile = lines(&records[1].body);
    assert_eq!(profile.len(), 1);
    let expected_profile = json!({
        "received_at": "2026-10-09T14:03:12Z", "installation_id": INSTALLATION,
        "session_id": SESSION, "release": "0.0.0+9d4138ca1f2e",
        "batch_seq": 2, "dropped": 0,
        "os_family": "macos", "os_version": [15, 5], "os_build": null, "os_distro": null,
        "os_distro_version": null, "os_kernel": null, "arch": "arm64",
        "cpu_vendor": "apple", "cpu_physical_cores": 12, "cpu_logical_cores": 12,
        "cpu_generation": 4, "cpu_tier": "pro", "cpu_family": null, "cpu_model": null,
        "cpu_stepping": null, "memory_gib_band": "32", "gpu_count": 1,
        "gpu0_vendor": "apple", "gpu0_backend": "metal", "gpu0_type": "integrated",
        "gpu0_unified_memory": true, "gpu0_vendor_id": null, "gpu0_device_id": null,
        "gpu0_generation": 4, "gpu0_tier": "pro", "gpu0_memory_gib_band": null,
        "gpu0_driver_version": null,
        "gpu1_vendor": null, "gpu1_backend": null, "gpu1_type": null, "gpu1_unified_memory": null,
        "gpu1_vendor_id": null, "gpu1_device_id": null, "gpu1_generation": null, "gpu1_tier": null,
        "gpu1_memory_gib_band": null, "gpu1_driver_version": null,
        "renderer": "gpu", "renderer_fallback": null
    });
    assert_eq!(profile[0], expected_profile);
    // Column order is fixed: the identifying columns come first.
    let text = std::str::from_utf8(&records[1].body).unwrap();
    assert!(text.starts_with("{\"received_at\":\"2026-10-09T14:03:12Z\",\"installation_id\":"));
    assert!(text.ends_with("\"renderer\":\"gpu\",\"renderer_fallback\":null}\n"));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_profile_row_carries_the_batch_sequence_and_dropped_count() {
    let downstream = Downstream::start(Mode::Ok).await;
    let (server, _) = start(test_config(&downstream.url()), ManualClock::new()).await;
    let mut body = batch();
    body["dropped"] = json!(4_294_967_295u64);
    assert_eq!(post_json(server.local_addr(), &body).await.status, 202);
    let records = downstream.wait_for(2).await;
    let profile = lines(&records[1].body);
    assert_eq!(profile[0]["batch_seq"], 2);
    assert_eq!(profile[0]["dropped"], 4_294_967_295u64);
    // The event rows do not carry it.
    assert!(
        lines(&records[0].body)
            .iter()
            .all(|row| row.get("dropped").is_none())
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_requests_get_their_status_and_forward_nothing() {
    let downstream = Downstream::start(Mode::Ok).await;
    let (server, _) = start(test_config(&downstream.url()), ManualClock::new()).await;
    let addr = server.local_addr();
    let route = "/v1/usage/batches";
    let body = batch().to_string();
    let body = body.as_bytes();

    let status = |response: Option<Response>| response.expect("a response").status;
    // 400: shape, range, ordering.
    for bad in [
        &b"{}"[..],
        b"",
        b"not json",
        br#"{"schema":1}"#,
        batch()
            .to_string()
            .replace("\"seq\":42", "\"seq\":41")
            .as_bytes(),
        batch()
            .to_string()
            .replace("\"develop\"", "\"library\"")
            .as_bytes(),
    ] {
        assert_eq!(
            status(raw(addr, &request("POST", route, &[JSON], bad)).await),
            400
        );
    }
    // 404: another path or a query, any method.
    for (method, path) in [
        ("POST", "/"),
        ("POST", "/v1/usage"),
        ("POST", "/v1/usage/batches/"),
        ("POST", "/V1/usage/batches"),
        ("POST", "/v1/usage/batches?x=1"),
        ("GET", "/healthz"),
        ("POST", "/v1/usage/batches/extra"),
        ("POST", "/v2/usage/batches"),
    ] {
        let r = raw(addr, &request(method, path, &[JSON], body))
            .await
            .expect("a response");
        assert_eq!(r.status, 404, "{method} {path}");
    }
    // 405: another method on the route.
    for method in ["GET", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"] {
        let r = raw(addr, &request(method, route, &[JSON], b""))
            .await
            .expect("a response");
        assert_eq!(r.status, 405, "{method}");
        assert_eq!(r.header("allow"), Some("POST"));
    }
    // 415: a content type other than application/json (optionally with utf-8).
    for headers in [
        vec![],
        vec![("content-type", "text/plain")],
        vec![("content-type", "application/jsonx")],
        vec![("content-type", "application/json; charset=latin1")],
        vec![("content-type", "application/json; charset=utf-8; x=y")],
        vec![("content-type", "application/x-www-form-urlencoded")],
        vec![
            ("content-type", "application/json"),
            ("content-type", "application/json"),
        ],
        vec![("content-type", "")],
    ] {
        let r = raw(addr, &request("POST", route, &headers, body))
            .await
            .expect("a response");
        assert_eq!(r.status, 415, "{headers:?}");
    }
    // 413: a declared length over the cap is refused before the body is sent.
    let head = "POST /v1/usage/batches HTTP/1.1\r\nhost: x\r\ncontent-type: application/json\r\ncontent-length: 65537\r\n\r\n";
    assert_eq!(status(raw(addr, head.as_bytes()).await), 413);
    // 413: a chunked body is cut off while it is read.
    let mut chunked = b"POST /v1/usage/batches HTTP/1.1\r\nhost: x\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n".to_vec();
    for _ in 0..70 {
        chunked.extend_from_slice(b"400\r\n");
        chunked.extend_from_slice(&[b' '; 1024]);
        chunked.extend_from_slice(b"\r\n");
    }
    chunked.extend_from_slice(b"0\r\n\r\n");
    assert_eq!(status(raw(addr, &chunked).await), 413);
    // A malformed request is just closed or refused, never forwarded.
    let _ = raw(addr, b"garbage\r\n\r\n").await;

    // Nothing above reached the collector: a valid batch is the first thing it sees.
    assert!(downstream.records().is_empty());
    assert_eq!(post(addr, body).await.status, 202);
    let records = downstream.wait_for(2).await;
    assert_eq!(records.len(), 2);
    let counters = &server.diagnostics().counters;
    assert_eq!(counters.accepted.load(SeqCst), 1);
    assert_eq!(counters.not_found.load(SeqCst), 8);
    assert_eq!(counters.method_not_allowed.load(SeqCst), 6);
    assert_eq!(counters.unsupported_media_type.load(SeqCst), 8);
    assert_eq!(counters.payload_too_large.load(SeqCst), 2);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn content_type_and_the_body_cap_boundaries() {
    let downstream = Downstream::start(Mode::Ok).await;
    let (server, _) = start(test_config(&downstream.url()), ManualClock::new()).await;
    let addr = server.local_addr();
    let route = "/v1/usage/batches";
    let body = batch().to_string();
    for content_type in [
        "application/json",
        "Application/JSON",
        "application/json; charset=utf-8",
        "application/json;charset=UTF-8",
    ] {
        let r = raw(
            addr,
            &request(
                "POST",
                route,
                &[("content-type", content_type)],
                body.as_bytes(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(r.status, 202, "{content_type}");
    }
    let mut at_cap = body.clone();
    at_cap.push_str(&" ".repeat(65_536 - at_cap.len()));
    assert_eq!(post(addr, at_cap.as_bytes()).await.status, 202);
    at_cap.push(' ');
    assert_eq!(post(addr, at_cap.as_bytes()).await.status, 413);
    // A body longer than it declared is never read past the declaration.
    let r = raw(
        addr,
        &request("POST", route, &[JSON, ("content-length", "2")], b"{}{}{}{}"),
    )
    .await;
    assert!(r.is_none_or(|r| r.status == 400));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deep_and_huge_bodies_are_refused_without_harm() {
    let downstream = Downstream::start(Mode::Ok).await;
    let (server, _) = start(test_config(&downstream.url()), ManualClock::new()).await;
    let addr = server.local_addr();
    let deep = format!("{}{}", "[".repeat(60_000), "]".repeat(5_000));
    assert_eq!(post(addr, deep.as_bytes()).await.status, 400);
    let mut nested = batch();
    nested["device"]["gpus"][0]["driver_version"] = json!([[1]]);
    assert_eq!(post_json(addr, &nested).await.status, 400);
    assert_eq!(post_json(addr, &batch()).await.status, 202);
    assert_eq!(downstream.wait_for(2).await.len(), 2);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn global_and_per_id_limits_refuse_with_retry_after() {
    let downstream = Downstream::start(Mode::Ok).await;
    let clock = ManualClock::new();
    let mut config = test_config(&downstream.url());
    config.global_per_second = 2;
    config.global_burst = 5;
    config.id_interval_secs = 30;
    config.id_burst = 2;
    let (server, _) = start(config, clock.clone()).await;
    let addr = server.local_addr();

    // The same installation: two batches, then a limit with Retry-After.
    assert_eq!(post_json(addr, &batch_for(&uuid(1), 0)).await.status, 202);
    assert_eq!(post_json(addr, &batch_for(&uuid(1), 1)).await.status, 202);
    let limited = post_json(addr, &batch_for(&uuid(1), 2)).await;
    assert_eq!(limited.status, 429);
    assert_eq!(limited.header("retry-after"), Some("30"));
    assert_eq!(limited.body, "too many requests");
    // Another installation is unaffected, and a malformed batch never allocates an ID entry.
    assert_eq!(post_json(addr, &batch_for(&uuid(2), 0)).await.status, 202);

    // The global limit: five tokens were available and all five are now spent.
    let last = post_json(addr, &batch_for(&uuid(3), 0)).await;
    assert_eq!(last.status, 202);
    let refused = post_json(addr, &batch_for(&uuid(4), 0)).await;
    assert_eq!(refused.status, 429);
    assert!(refused.header("retry-after").is_some());
    // Even a request that would be a 400 is refused before its body is read.
    assert_eq!(post(addr, b"{").await.status, 429);
    // Time passes and admission returns.
    clock.advance_ms(2_000);
    assert_eq!(post_json(addr, &batch_for(&uuid(4), 0)).await.status, 202);
    // The limit never touches other routes' statuses.
    assert_eq!(
        raw(addr, &request("GET", "/other", &[], b""))
            .await
            .unwrap()
            .status,
        404
    );
    let counters = &server.diagnostics().counters;
    assert_eq!(counters.limited_id.load(SeqCst), 1);
    assert!(counters.limited_global.load(SeqCst) >= 2);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hung_downstream_fills_a_bounded_queue_then_refuses() {
    let downstream = Downstream::start(Mode::Hang).await;
    let mut config = test_config(&downstream.url());
    config.queue_batches = 3;
    let (server, _) = start(config, ManualClock::new()).await;
    let addr = server.local_addr();
    let started = Instant::now();
    let mut accepted = 0;
    let mut refused = 0;
    for n in 0..40 {
        match post_json(addr, &batch_for(&uuid(n), 0)).await {
            r if r.status == 202 => accepted += 1,
            r => {
                assert_eq!(r.status, 429);
                assert!(r.header("retry-after").is_some());
                refused += 1;
            }
        }
    }
    // Three queued plus the one the worker holds in flight; the rest are refused promptly.
    assert!((3..=4).contains(&accepted), "accepted {accepted}");
    assert_eq!(accepted + refused, 40);
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(
        server.diagnostics().counters.limited_queue.load(SeqCst),
        refused
    );
    // The collector saw one request (the stuck one) and the service stayed responsive.
    assert_eq!(downstream.records().len(), 1);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_queue_is_bounded_in_bytes_too() {
    let downstream = Downstream::start(Mode::Hang).await;
    let mut config = test_config(&downstream.url());
    config.queue_batches = 1000;
    config.queue_bytes = 8_000;
    let (server, _) = start(config, ManualClock::new()).await;
    let addr = server.local_addr();
    let mut accepted = 0;
    for n in 0..20 {
        if post_json(addr, &batch(n)).await.status == 202 {
            accepted += 1;
        }
    }
    // One batch's rows are about 3 KiB: at most two fit in 8,000 bytes, and with the one held in flight (counted until done) at most three.
    assert!((1..=3).contains(&accepted), "accepted {accepted}");
    server.shutdown().await;

    fn batch(n: u64) -> Value {
        let mut b = support::batch();
        b["installation_id"] = uuid(n).into();
        b
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unavailable_downstream_never_blocks_clients_or_retries() {
    let dead = closed_port().await;
    let (server, _) = start(
        test_config(&format!("http://{dead}/ingest")),
        ManualClock::new(),
    )
    .await;
    let addr = server.local_addr();
    for n in 0..30 {
        let r = post_json(addr, &batch_for(&uuid(n), 0)).await;
        assert!(r.status == 202 || r.status == 429, "status {}", r.status);
    }
    let counters = server.diagnostics();
    for _ in 0..500 {
        if counters.counters.forward_failed.load(SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(counters.counters.forward_failed.load(SeqCst) > 0);
    assert_eq!(counters.counters.forwarded.load(SeqCst), 0);
    server.shutdown().await;

    // A collector that answers 500: each body is sent once, counted failed, never retried.
    let failing = Downstream::start(Mode::Fail).await;
    let (server, _) = start(test_config(&failing.url()), ManualClock::new()).await;
    assert_eq!(post_json(server.local_addr(), &batch()).await.status, 202);
    failing.wait_for(2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(failing.records().len(), 2);
    assert_eq!(server.diagnostics().counters.forward_failed.load(SeqCst), 2);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connections_are_bounded_and_slow_ones_time_out() {
    let downstream = Downstream::start(Mode::Ok).await;
    let mut config = test_config(&downstream.url());
    config.max_connections = 2;
    config.request_timeout = Duration::from_secs(1);
    let (server, _) = start(config, ManualClock::new()).await;
    let addr = server.local_addr();

    // Two connections that say nothing hold both slots.
    let mut idle_a = TcpStream::connect(addr).await.unwrap();
    let idle_b = TcpStream::connect(addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let refused = raw(addr, &request("POST", "/v1/usage/batches", &[JSON], b"{}")).await;
    assert_eq!(refused.expect("a canned reply").status, 429);
    assert_eq!(
        server
            .diagnostics()
            .counters
            .connections_refused
            .load(SeqCst),
        1
    );

    // They are closed at the request timeout, without any reply, and the slots free up.
    let mut buffer = Vec::new();
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(5), idle_a.read_to_end(&mut buffer))
        .await
        .unwrap()
        .ok();
    assert!(buffer.is_empty());
    assert!(started.elapsed() < Duration::from_secs(4));
    drop(idle_b);
    assert_eq!(post_json(addr, &batch()).await.status, 202);

    // A body that stalls is cut off the same way.
    let mut slow = TcpStream::connect(addr).await.unwrap();
    slow.write_all(b"POST /v1/usage/batches HTTP/1.1\r\nhost: x\r\ncontent-type: application/json\r\ncontent-length: 500\r\n\r\n{\"schema\"").await.unwrap();
    let mut buffer = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), slow.read_to_end(&mut buffer))
        .await
        .unwrap()
        .ok();
    assert!(buffer.is_empty());
    assert!(
        server
            .diagnostics()
            .counters
            .connections_timed_out
            .load(SeqCst)
            >= 2
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_flood_of_malformed_requests_leaves_valid_ones_working() {
    let downstream = Downstream::start(Mode::Ok).await;
    let mut config = test_config(&downstream.url());
    config.max_connections = 16;
    let (server, _) = start(config, ManualClock::new()).await;
    let addr = server.local_addr();
    let mut tasks = Vec::new();
    for n in 0..200u64 {
        tasks.push(tokio::spawn(async move {
            let body: Vec<u8> = match n % 4 {
                0 => vec![b'{'; 40_000],
                1 => vec![0xff; 1000],
                2 => b"{\"schema\":1}".to_vec(),
                _ => Vec::new(),
            };
            raw(addr, &request("POST", "/v1/usage/batches", &[JSON], &body))
                .await
                .map(|r| r.status)
        }));
    }
    for task in tasks {
        let status = task.await.unwrap();
        assert!(matches!(status, Some(400 | 429) | None), "{status:?}");
    }
    assert!(downstream.records().is_empty());
    assert_eq!(post_json(addr, &batch()).await.status, 202);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn no_address_header_or_id_reaches_a_log_or_the_collector() {
    const SENTINEL_IP: &str = "203.0.113.77";
    const SENTINEL_AGENT: &str = "sentinel-agent/9.9";
    const SENTINEL_FILE: &str = "sentinel-holiday-photo.jpg";
    let downstream = Downstream::start(Mode::Ok).await;
    let mut config = test_config(&downstream.url());
    config.id_interval_secs = 30;
    config.id_burst = 1;
    let (server, log) = start(config, ManualClock::new()).await;
    let addr = server.local_addr();
    let headers = [
        JSON,
        ("x-forwarded-for", SENTINEL_IP),
        ("forwarded", "for=203.0.113.77"),
        ("x-real-ip", SENTINEL_IP),
        ("user-agent", SENTINEL_AGENT),
        ("cookie", "sentinel-cookie=1"),
        ("authorization", "Bearer sentinel-token"),
        ("x-request-id", "sentinel-request-id"),
    ];
    let valid = batch().to_string();
    assert_eq!(
        raw(
            addr,
            &request("POST", "/v1/usage/batches", &headers, valid.as_bytes())
        )
        .await
        .unwrap()
        .status,
        202
    );
    // Same installation again: limited (a logged outcome class, never its ID).
    assert_eq!(
        raw(
            addr,
            &request("POST", "/v1/usage/batches", &headers, valid.as_bytes())
        )
        .await
        .unwrap()
        .status,
        429
    );
    // Invalid payloads carrying sentinels in values and keys.
    let mut bad = batch();
    bad["device"]["hostname"] = SENTINEL_FILE.into();
    bad["events"][0]["path"] = SENTINEL_FILE.into();
    for body in [
        bad.to_string(),
        format!("{{\"{SENTINEL_FILE}\":\"{SENTINEL_IP}\"}}"),
    ] {
        assert_eq!(
            raw(
                addr,
                &request("POST", "/v1/usage/batches", &headers, body.as_bytes())
            )
            .await
            .unwrap()
            .status,
            400
        );
    }
    raw(
        addr,
        &request("GET", &format!("/{SENTINEL_FILE}"), &headers, b""),
    )
    .await;
    raw(addr, &request("PUT", "/v1/usage/batches", &headers, b"")).await;

    downstream.wait_for(2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    server.diagnostics().emit();
    server.shutdown().await;

    let log = log.lock().unwrap().join("\n");
    assert!(log.contains("usage-ingest stats accepted=1 "), "{log}");
    for forbidden in [
        SENTINEL_IP,
        SENTINEL_AGENT,
        SENTINEL_FILE,
        "sentinel",
        "127.0.0.1",
        "localhost",
        INSTALLATION,
        SESSION,
        IMAGE,
        "2026-10-09",
        "0.0.0+",
        "203.0",
        "Bearer",
    ] {
        assert!(!log.contains(forbidden), "log holds {forbidden}: {log}");
    }
    // Every line is a fixed message or the counters.
    for line in log.lines() {
        assert!(
            line == "usage-ingest listening" || line.starts_with("usage-ingest stats "),
            "{line}"
        );
    }

    // The collector got only the service's own headers, and the installation ID only in its
    // own field of each row.
    let records = downstream.records();
    assert_eq!(records.len(), 2);
    for record in &records {
        let names: Vec<&str> = record.headers.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            names
                .iter()
                .all(|n| ["host", "content-type", "content-length"].contains(n)),
            "{names:?}"
        );
        let text = std::str::from_utf8(&record.body).unwrap();
        for forbidden in [
            SENTINEL_IP,
            SENTINEL_AGENT,
            SENTINEL_FILE,
            "sentinel",
            "127.0.0.1",
            "Bearer",
            "cookie",
        ] {
            assert!(!text.contains(forbidden), "{forbidden}");
        }
        let rows = lines(&record.body);
        assert_eq!(text.matches(INSTALLATION).count(), rows.len());
        assert_eq!(text.matches(SESSION).count(), rows.len());
        assert!(rows.iter().all(|r| r["installation_id"] == INSTALLATION));
    }
}

#[test]
fn the_source_never_reads_addresses_or_headers_it_should_not() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap().to_lowercase();
        for forbidden in [
            "peer_addr",
            "remote_addr",
            "connectinfo",
            "ipaddr",
            "x-forwarded",
            "user_agent",
            "user-agent",
            "println!(\"{:?}\"",
            "{:?}\", request",
            "tracing",
            "env_logger",
        ] {
            assert!(
                !text.contains(forbidden),
                "{} mentions {forbidden}",
                path.display()
            );
        }
    }
}

#[test]
fn the_checked_in_registry_file_is_sorted_kebab_case() {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("registry.json"),
    )
    .unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    for key in ["tool", "feature"] {
        let values: Vec<&str> = value[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(!values.is_empty());
        assert!(
            values.windows(2).all(|w| w[0] < w[1]),
            "{key} is not sorted and unique"
        );
        assert!(
            values
                .iter()
                .all(|v| luxforge_usage_ingest::registry::kebab_case(v)),
            "{key}"
        );
    }
}
