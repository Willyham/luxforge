//! The update feed route: static content from the store, cache headers, refusals, a publication
//! swapping the store, and no effect on the statistics route's limits or logs.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::SeqCst;
use support::*;

const MAC: &str = "/v1/updates/macos-arm64/appcast.xml";
const LINUX: &str = "/v1/updates/linux-x86_64/appcast.xml";

/// A new, empty directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "luxforge-ingest-updates-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// One publication as the store holds it: `<version>/<target>/appcast.xml`.
fn publish(store: &Path, version: &str, feeds: &[(&str, &str)]) -> PathBuf {
    let dir = store.join(version);
    for (target, xml) in feeds {
        std::fs::create_dir_all(dir.join(target)).unwrap();
        std::fs::write(dir.join(target).join("appcast.xml"), xml).unwrap();
    }
    dir
}

/// Points `store/current` at `version`, replacing the link at once as the publication step does.
#[cfg(unix)]
fn point(store: &Path, version: &str) {
    let staged = store.join(".current-new");
    let _ = std::fs::remove_file(&staged);
    std::os::unix::fs::symlink(version, &staged).unwrap();
    std::fs::rename(&staged, store.join("current")).unwrap();
}

async fn get(addr: std::net::SocketAddr, method: &str, path: &str) -> Response {
    raw(addr, &request(method, path, &[], b""))
        .await
        .expect("a response")
}

fn feed(build: u32) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<rss version=\"2.0\"><channel><title>Luxforge</title>\
         <item><sparkle:version>{build}</sparkle:version></item></channel></rss>\n"
    )
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn feeds_are_served_as_static_content_with_cache_headers() {
    let store = scratch("served");
    publish(
        &store,
        "feeds-1",
        &[("macos-arm64", &feed(1)), ("linux-x86_64", &feed(2))],
    );
    point(&store, "feeds-1");
    let mut config = test_config("http://127.0.0.1:9");
    config.updates_dir = Some(store.join("current"));
    // The statistics route's global limit does not apply to feeds.
    config.global_per_second = 1;
    config.global_burst = 1;
    let (server, log) = start(config, ManualClock::new()).await;
    let addr = server.local_addr();

    for (path, build) in [(MAC, 1), (LINUX, 2), (MAC, 1), (LINUX, 2)] {
        let response = get(addr, "GET", path).await;
        assert_eq!(response.status, 200, "{path}");
        assert_eq!(response.body, feed(build));
        assert_eq!(
            response.header("content-type"),
            Some("application/rss+xml; charset=utf-8")
        );
        assert_eq!(
            response.header("cache-control"),
            Some("public, max-age=900")
        );
        assert_eq!(
            response.header("content-length"),
            Some(feed(build).len().to_string().as_str())
        );
        // Only what HTTP needs, the type and the cache policy: no cookie, identifier or redirect.
        let names: Vec<&str> = response.headers.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            names.iter().all(|n| [
                "content-type",
                "cache-control",
                "content-length",
                "connection",
                "date"
            ]
            .contains(n)),
            "{names:?}"
        );
    }
    let head = get(addr, "HEAD", LINUX).await;
    assert_eq!(head.status, 200);
    assert_eq!(head.body, "");
    assert_eq!(
        head.header("content-length"),
        Some(feed(2).len().to_string().as_str())
    );

    // A publication replaces the link; the next request reads the new feed.
    publish(
        &store,
        "feeds-2",
        &[("macos-arm64", &feed(3)), ("linux-x86_64", &feed(4))],
    );
    point(&store, "feeds-2");
    assert_eq!(get(addr, "GET", MAC).await.body, feed(3));
    assert_eq!(get(addr, "GET", LINUX).await.body, feed(4));

    // Refusals.
    let refused = |status: u16, method: &'static str, path: &'static str| async move {
        let response = get(addr, method, path).await;
        assert_eq!(response.status, status, "{method} {path}");
        assert!(response.body.len() <= 64);
        response
    };
    for method in ["POST", "PUT", "DELETE", "OPTIONS"] {
        let response = refused(405, method, LINUX).await;
        assert_eq!(response.header("allow"), Some("GET, HEAD"));
    }
    for path in [
        "/v1/updates/windows-x86_64/appcast.xml",
        "/v1/updates/linux-x86_64/appcast.xml?build=3",
        "/v1/updates/linux-x86_64/appcast.xml?",
        "/v1/updates/linux-arm64/appcast.xml",
        "/v1/updates/linux-x86_64/",
        "/v1/updates/",
        "/v1/updates/linux-x86_64/../macos-arm64/appcast.xml",
    ] {
        refused(404, "GET", path).await;
    }
    // A feed over the bound is not served.
    std::fs::write(
        store.join("feeds-2/macos-arm64/appcast.xml"),
        vec![b' '; 4 * 1024 * 1024 + 1],
    )
    .unwrap();
    refused(404, "GET", MAC).await;
    // Nor is a directory where the file should be, nor a store that lost its link.
    std::fs::remove_file(store.join("feeds-2/macos-arm64/appcast.xml")).unwrap();
    std::fs::create_dir(store.join("feeds-2/macos-arm64/appcast.xml")).unwrap();
    refused(404, "GET", MAC).await;
    std::fs::remove_file(store.join("current")).unwrap();
    refused(404, "GET", LINUX).await;

    let counters = &server.diagnostics().counters;
    assert_eq!(counters.feeds_served.load(SeqCst), 7);
    assert_eq!(counters.feeds_missing.load(SeqCst), 4);
    assert_eq!(counters.limited_global.load(SeqCst), 0);
    server.diagnostics().emit();
    server.shutdown().await;
    let log = log.lock().unwrap().join("\n");
    for line in log.lines() {
        assert!(
            line == "usage-ingest listening" || line.starts_with("usage-ingest stats "),
            "{line}"
        );
    }
    assert!(log.contains(" feeds_served=7 feeds_missing=4"), "{log}");
    assert!(!log.contains("linux-x86_64") && !log.contains("appcast"));
    std::fs::remove_dir_all(&store).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_store_the_feed_route_does_not_exist() {
    let (server, _) = start(test_config("http://127.0.0.1:9"), ManualClock::new()).await;
    let addr = server.local_addr();
    for method in ["GET", "HEAD"] {
        assert_eq!(get(addr, method, LINUX).await.status, 404);
    }
    // The statistics route is unchanged beside it.
    assert_eq!(
        get(addr, "GET", "/v1/usage/batches").await.header("allow"),
        Some("POST")
    );
    server.shutdown().await;
}
